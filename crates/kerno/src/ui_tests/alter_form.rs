//! Altering a table through its form (CLAUDE.md §4 F2): every kind of
//! change in one apply, the statements previewed one by one, the wording of
//! the confirmation on each engine, and what the server has afterwards.

use super::tool_wizards::paints;
use super::*;

impl Fixture {
    /// The statements the active table form would run.
    fn form_statements(&self, cx: &mut TestAppContext) -> Vec<String> {
        let editor = self.editor(cx);
        cx.update(|cx| editor.read(cx).statements(cx))
            .map(|(statements, _)| statements)
            .unwrap_or_else(|problems| panic!("{problems:?}"))
    }

    /// Clicks Apply and checks the confirmation: its title, its
    /// explanation, and the statements as one script.
    fn confirm_alter(&self, cx: &mut TestAppContext, title: &str, explanation: &str) {
        let script = self
            .form_statements(cx)
            .iter()
            .map(|statement| format!("{statement};"))
            .collect::<Vec<_>>()
            .join("\n");
        self.click(cx, "editor-apply");
        assert!(self.has(cx, "sql-preview"));
        assert_eq!(self.label_of(cx, "sql-title"), title);
        assert!(paints(self, cx, explanation), "{explanation}");
        assert!(paints(self, cx, &script), "{script}");
        self.click(cx, "sql-confirm");
        self.wait_until(cx, "the alter", |app, cx| app.has(cx, "editor-no-change"));
        let editor = self.editor(cx);
        assert_eq!(
            cx.update(|cx| editor.read(cx).error().map(str::to_string)),
            None
        );
    }
}

#[gpui_kit::test]
fn every_change_of_the_form_is_previewed_confirmed_and_applied(cx: &mut TestAppContext) {
    for engine in ENGINES {
        if !testing::available(engine) {
            return;
        }
        let pg = engine == Engine::Postgres;
        let schema = "kerno_ui_alter_all";
        let (drop_schema, create_schema) = scratch_schema(engine, schema);
        let parents = format!("CREATE TABLE {schema}.parents (id INT PRIMARY KEY)");
        let items = scratch_table(
            engine,
            &format!("{schema}.items"),
            "name varchar(50) NOT NULL, note text, qty int, parent_id int",
        );
        let _scratch = Scratch::new(engine, &[&drop_schema], &[&create_schema, &parents, &items]);
        let server = |sql: &str| on_server(engine, sql);
        let app = connected(cx, engine);
        app.open_structure(cx, schema, "items", RelationKind::Table);
        app.wait_for_structure(cx);
        app.click(cx, "structure-edit");
        app.wait_for_editor(cx);
        // Whether columns can be moved on an existing table, and why not.
        assert!(paints(
            &app,
            cx,
            if pg {
                kerno_core::table_draft::CANNOT_MOVE_COLUMNS
            } else {
                "↑ and ↓ move a column; the server rewrites the table to put it there."
            }
        ));

        // Columns: 0 id, 1 name, 2 note, 3 qty, 4 parent_id, then 5 added.
        app.fill_in(cx, "editor-name", "goods");
        app.fill_in(cx, "editor-comment", "Goods on sale");
        app.fill_in(cx, ("column-name", 1usize), "title");
        app.fill_in(cx, ("column-type", 3usize), "bigint");
        app.click(cx, "editor-add-column");
        app.fill_in(cx, ("column-name", 5usize), "added");
        app.fill_in(cx, ("column-type", 5usize), "int");
        app.click(cx, "editor-add-index");
        app.fill_in(cx, ("index-name", 0usize), "goods_added_idx");
        app.click(cx, "index-0-column-5".to_string());
        app.click(cx, "editor-add-key");
        app.fill_in(cx, ("key-name", 0usize), "goods_parent_fk");
        let editor = app.editor(cx);
        app.step(cx, |window, cx| {
            editor.update(cx, |editor, cx| editor.reference(0, "parents", window, cx))
        });
        app.fill_in(cx, ("key-referenced", 0usize), "id");
        app.click(cx, "key-0-column-4".to_string());
        app.click(cx, ("column-remove", 2usize));

        let expected: Vec<String> = if pg {
            [
                "ALTER TABLE \"kerno_ui_alter_all\".\"items\" DROP COLUMN \"note\"",
                "ALTER TABLE \"kerno_ui_alter_all\".\"items\" RENAME COLUMN \"name\" TO \"title\"",
                "ALTER TABLE \"kerno_ui_alter_all\".\"items\" ADD COLUMN \"added\" int",
                "ALTER TABLE \"kerno_ui_alter_all\".\"items\" ALTER COLUMN \"qty\" TYPE bigint",
                "CREATE INDEX \"goods_added_idx\" ON \"kerno_ui_alter_all\".\"items\" (\"added\")",
                "ALTER TABLE \"kerno_ui_alter_all\".\"items\" ADD CONSTRAINT \"goods_parent_fk\" \
                 FOREIGN KEY (\"parent_id\") REFERENCES \"kerno_ui_alter_all\".\"parents\" (\"id\") \
                 ON UPDATE NO ACTION ON DELETE NO ACTION",
                "COMMENT ON TABLE \"kerno_ui_alter_all\".\"items\" IS 'Goods on sale'",
                "ALTER TABLE \"kerno_ui_alter_all\".\"items\" RENAME TO \"goods\"",
            ]
            .map(str::to_string)
            .to_vec()
        } else {
            // One statement, a clause per line.
            vec![
                [
                    "ALTER TABLE `kerno_ui_alter_all`.`items`",
                    "DROP COLUMN `note`,",
                    "RENAME COLUMN `name` TO `title`,",
                    "ADD COLUMN `added` int NULL,",
                    "MODIFY COLUMN `qty` bigint NULL,",
                    "ADD INDEX `goods_added_idx` (`added`),",
                    "ADD CONSTRAINT `goods_parent_fk` FOREIGN KEY (`parent_id`) \
                     REFERENCES `kerno_ui_alter_all`.`parents` (`id`) \
                     ON UPDATE NO ACTION ON DELETE NO ACTION,",
                    "COMMENT = 'Goods on sale',",
                    "RENAME TO `kerno_ui_alter_all`.`goods`",
                ]
                .join("\n    "),
            ]
        };
        assert_eq!(app.form_statements(cx), expected, "{engine:?}");
        let explanation = if pg {
            "These statements run in one transaction: all of them take effect, or none does."
                .to_string()
        } else {
            format!(
                "{} applies this statement at once, all of it or none of it; it cannot be \
                 rolled back afterwards.",
                engine.label()
            )
        };
        app.confirm_alter(cx, "Alter table items", &explanation);

        // The form and the structure tab follow the new name.
        assert_eq!(
            app.titles(cx),
            ["goods · structure", "goods · edit"],
            "{engine:?}"
        );
        assert_eq!(
            server(&format!(
                "SELECT table_name FROM information_schema.tables \
                 WHERE table_schema = '{schema}' AND table_name <> 'parents'"
            )),
            "goods",
            "{engine:?}"
        );
        assert_eq!(
            columns_on_server(engine, schema, "goods"),
            "id,title,qty,parent_id,added",
            "{engine:?}"
        );
        assert_eq!(
            server(&format!(
                "SELECT data_type FROM information_schema.columns \
                 WHERE table_schema = '{schema}' AND table_name = 'goods' \
                 AND column_name = 'qty'"
            )),
            "bigint",
            "{engine:?}"
        );
        let comment = if pg {
            format!("SELECT obj_description('{schema}.goods'::regclass, 'pg_class')")
        } else {
            format!(
                "SELECT table_comment FROM information_schema.tables \
                 WHERE table_schema = '{schema}' AND table_name = 'goods'"
            )
        };
        assert_eq!(server(&comment), "Goods on sale", "{engine:?}");
        let index = if pg {
            format!(
                "SELECT indexdef FROM pg_indexes \
                 WHERE schemaname = '{schema}' AND indexname = 'goods_added_idx'"
            )
        } else {
            format!(
                "SELECT concat(table_name, '.', column_name) FROM information_schema.statistics \
                 WHERE table_schema = '{schema}' AND index_name = 'goods_added_idx'"
            )
        };
        let index = server(&index);
        assert!(
            index.contains("goods") && index.contains("added"),
            "{engine:?}: {index}"
        );
        let key = if pg {
            format!(
                "SELECT pg_get_constraintdef(oid) FROM pg_constraint \
                 WHERE conname = 'goods_parent_fk' AND conrelid = '{schema}.goods'::regclass"
            )
        } else {
            format!(
                "SELECT concat('FOREIGN KEY (', column_name, ') REFERENCES ', \
                 referenced_table_name, '(', referenced_column_name, ')') \
                 FROM information_schema.key_column_usage \
                 WHERE table_schema = '{schema}' AND table_name = 'goods' \
                 AND constraint_name = 'goods_parent_fk'"
            )
        };
        // PostgreSQL names the schema, which is not on the search path.
        let referenced = if pg {
            "kerno_ui_alter_all.parents"
        } else {
            "parents"
        };
        assert_eq!(
            server(&key),
            format!("FOREIGN KEY (parent_id) REFERENCES {referenced}(id)"),
            "{engine:?}"
        );

        // On MySQL and MariaDB a dropped foreign key is a statement of its
        // own, and the confirmation says the first one stays applied if the
        // second fails.
        if !pg {
            app.click(cx, ("key-remove", 0usize));
            app.fill_in(cx, ("column-name", 4usize), "extra");
            assert_eq!(
                app.form_statements(cx),
                [
                    "ALTER TABLE `kerno_ui_alter_all`.`goods`\n    DROP FOREIGN KEY `goods_parent_fk`",
                    "ALTER TABLE `kerno_ui_alter_all`.`goods`\n    RENAME COLUMN `added` TO `extra`",
                ],
                "{engine:?}"
            );
            app.confirm_alter(
                cx,
                "Alter table goods",
                &format!(
                    "{} applies each statement at once; a failure leaves the earlier ones applied.",
                    engine.label()
                ),
            );
            assert_eq!(
                columns_on_server(engine, schema, "goods"),
                "id,title,qty,parent_id,extra",
                "{engine:?}"
            );
            assert_eq!(
                server(&format!(
                    "SELECT count(*) FROM information_schema.table_constraints \
                     WHERE table_schema = '{schema}' AND constraint_type = 'FOREIGN KEY'"
                )),
                "0",
                "{engine:?}"
            );
        }
        // The structure tab that followed the rename shows the new columns.
        app.open_structure(cx, schema, "goods", RelationKind::Table);
        assert_eq!(
            app.titles(cx),
            ["goods · structure", "goods · edit"],
            "{engine:?}"
        );
        let last = if pg { "added" } else { "extra" };
        app.wait_until(cx, "the structure of goods", |app, cx| {
            app.structure_column(cx, List::Columns, 1) == ["id", "title", "qty", "parent_id", last]
        });
    }
}
