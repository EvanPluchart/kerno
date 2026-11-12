//! PostgreSQL's SQL dialect.

use kerno_core::dialect::{Dialect, default_drop_sql, default_rename_sql, hex};
use kerno_core::schema::{ObjectRef, RelationKind, RoutineKind, TableRef};
use kerno_core::sql::LexRules;
use kerno_core::{Engine, Error, Plan, Result, TypeKind};

/// The PostgreSQL dialect.
#[derive(Debug, Clone, Copy, Default)]
pub struct PgDialect;

/// `<kind> <name>` for the objects `ALTER` and `DROP` do not name the
/// standard way: a trigger is named within its table, and an aggregate
/// over no argument is written `name(*)`.
fn object_target(dialect: &PgDialect, object: &ObjectRef) -> Option<String> {
    match object {
        ObjectRef::Trigger { table, name } => Some(format!(
            "TRIGGER {} ON {}",
            dialect.quote_ident(name),
            dialect.table_name(table)
        )),
        ObjectRef::Routine {
            schema,
            name,
            kind: RoutineKind::Aggregate,
            identity_arguments,
        } if identity_arguments.is_empty() => Some(format!(
            "AGGREGATE {}.{}(*)",
            dialect.quote_ident(schema),
            dialect.quote_ident(name)
        )),
        _ => None,
    }
}

impl Dialect for PgDialect {
    fn engine(&self) -> Engine {
        Engine::Postgres
    }

    fn has_schemas(&self) -> bool {
        true
    }

    fn lex_rules(&self) -> LexRules {
        LexRules::POSTGRES
    }

    fn quote_ident(&self, ident: &str) -> String {
        format!("\"{}\"", ident.replace('"', "\"\""))
    }

    fn quote_string(&self, text: &str) -> String {
        if text.contains('\\') {
            // An escape string means the same thing whatever
            // standard_conforming_strings is set to.
            format!("E'{}'", text.replace('\\', "\\\\").replace('\'', "''"))
        } else {
            format!("'{}'", text.replace('\'', "''"))
        }
    }

    fn quote_bytes(&self, bytes: &[u8]) -> String {
        // decode() rather than a '\x..' literal, which depends on
        // standard_conforming_strings.
        format!("decode('{}', 'hex')", hex(bytes))
    }

    fn type_for(&self, kind: TypeKind) -> &'static str {
        match kind {
            TypeKind::Integer => "bigint",
            TypeKind::Float => "double precision",
            TypeKind::Decimal => "numeric",
            TypeKind::Boolean => "boolean",
            TypeKind::Date => "date",
            TypeKind::DateTime | TypeKind::DateTimeTz => "timestamptz",
            TypeKind::Uuid => "uuid",
            TypeKind::Json => "jsonb",
            TypeKind::Binary => "bytea",
            _ => "text",
        }
    }

    /// `CHAR` without a length is one character here.
    fn cast_to_text(&self, expr: &str) -> String {
        format!("CAST({expr} AS text)")
    }

    fn explain(&self, sql: &str, analyze: bool) -> String {
        if analyze {
            format!("EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) {sql}")
        } else {
            format!("EXPLAIN (FORMAT JSON) {sql}")
        }
    }

    fn parse_plan(&self, output: &str, _analyze: bool) -> Result<Plan> {
        crate::plan::parse(output)
    }

    /// `ALTER <kind> <name> RENAME TO` for every kind of object; indexes,
    /// like relations, are named within their schema.
    fn rename_sql(&self, object: &ObjectRef, new_name: &str) -> Result<String> {
        match object_target(self, object) {
            Some(target) => Ok(format!(
                "ALTER {target} RENAME TO {}",
                self.quote_ident(new_name)
            )),
            None => default_rename_sql(self, object, new_name),
        }
    }

    fn drop_sql(&self, object: &ObjectRef) -> Result<String> {
        match object_target(self, object) {
            Some(target) => Ok(format!("DROP {target}")),
            None => default_drop_sql(self, object),
        }
    }

    /// Views and routines are printed as `CREATE OR REPLACE` already
    /// (`pg_get_viewdef` wrapped by the driver, `pg_get_functiondef`). A
    /// materialized view cannot be replaced: it is dropped and created
    /// again in one transaction, so a definition that fails keeps the old
    /// one.
    fn replace_source_sql(&self, object: &ObjectRef, ddl: &str) -> Result<String> {
        match object {
            ObjectRef::Relation(_, RelationKind::View)
            | ObjectRef::Routine {
                kind: RoutineKind::Function | RoutineKind::Procedure | RoutineKind::Window,
                ..
            } => Ok(ddl.to_string()),
            ObjectRef::Relation(view, RelationKind::MaterializedView) => Ok(format!(
                "BEGIN;\n\nDROP MATERIALIZED VIEW {};\n\n{}\n\nCOMMIT;",
                self.table_name(view),
                ddl.trim()
            )),
            _ => Err(Error::Unsupported(
                "editing the source of this kind of object".into(),
            )),
        }
    }

    fn create_extension_sql(&self, name: &str) -> Result<String> {
        Ok(format!(
            "CREATE EXTENSION IF NOT EXISTS {}",
            self.quote_ident(name)
        ))
    }

    fn drop_extension_sql(&self, name: &str) -> Result<String> {
        Ok(format!("DROP EXTENSION {}", self.quote_ident(name)))
    }

    fn create_enum_sql(&self, ty: &TableRef, values: &[String]) -> Result<String> {
        let values: Vec<String> = values.iter().map(|v| self.quote_string(v)).collect();
        Ok(format!(
            "CREATE TYPE {} AS ENUM ({})",
            self.table_name(ty),
            values.join(", ")
        ))
    }

    fn add_enum_value_sql(
        &self,
        ty: &TableRef,
        value: &str,
        before: Option<&str>,
    ) -> Result<String> {
        let mut sql = format!(
            "ALTER TYPE {} ADD VALUE {}",
            self.table_name(ty),
            self.quote_string(value)
        );
        if let Some(before) = before {
            sql.push_str(&format!(" BEFORE {}", self.quote_string(before)));
        }
        Ok(sql)
    }

    fn rename_enum_value_sql(&self, ty: &TableRef, from: &str, to: &str) -> Result<String> {
        Ok(format!(
            "ALTER TYPE {} RENAME VALUE {} TO {}",
            self.table_name(ty),
            self.quote_string(from),
            self.quote_string(to)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_always_quoted() {
        let d = PgDialect;
        assert_eq!(d.quote_ident("orders"), "\"orders\"");
        assert_eq!(d.quote_ident("select"), "\"select\"");
        assert_eq!(d.quote_ident("Weird \"Name\""), "\"Weird \"\"Name\"\"\"");
        assert_eq!(
            d.table_name(&TableRef::new("shop", "order items")),
            "\"shop\".\"order items\""
        );
    }

    #[test]
    fn strings_escape_quotes_and_backslashes() {
        let d = PgDialect;
        assert_eq!(d.quote_string(""), "''");
        assert_eq!(d.quote_string("it's"), "'it''s'");
        assert_eq!(d.quote_string(r"a\b'c"), r"E'a\\b''c'");
        assert_eq!(d.quote_string("multi\nline"), "'multi\nline'");
    }

    #[test]
    fn bytes_and_paging() {
        let d = PgDialect;
        assert_eq!(
            d.quote_bytes(&[0xde, 0xad, 0x00]),
            "decode('dead00', 'hex')"
        );
        assert_eq!(d.quote_bytes(&[]), "decode('', 'hex')");
        assert_eq!(d.limit_offset(100, 0), "LIMIT 100");
        assert_eq!(d.limit_offset(100, 300), "LIMIT 100 OFFSET 300");
    }

    fn routine(kind: RoutineKind, identity_arguments: &str) -> ObjectRef {
        ObjectRef::Routine {
            schema: "shop".into(),
            name: "order total".into(),
            kind,
            identity_arguments: identity_arguments.into(),
        }
    }

    /// One object of every kind PostgreSQL has, with the words that name
    /// it in `ALTER` and `DROP`.
    fn objects() -> Vec<(ObjectRef, &'static str)> {
        let t = || TableRef::new("shop", "order items");
        let relation = |kind| ObjectRef::Relation(t(), kind);
        vec![
            (
                relation(RelationKind::Table),
                "TABLE \"shop\".\"order items\"",
            ),
            (
                relation(RelationKind::PartitionedTable),
                "TABLE \"shop\".\"order items\"",
            ),
            (
                relation(RelationKind::ForeignTable),
                "FOREIGN TABLE \"shop\".\"order items\"",
            ),
            (
                relation(RelationKind::View),
                "VIEW \"shop\".\"order items\"",
            ),
            (
                relation(RelationKind::MaterializedView),
                "MATERIALIZED VIEW \"shop\".\"order items\"",
            ),
            (
                routine(RoutineKind::Function, "integer, note text"),
                "FUNCTION \"shop\".\"order total\"(integer, note text)",
            ),
            (
                routine(RoutineKind::Function, ""),
                "FUNCTION \"shop\".\"order total\"()",
            ),
            (
                routine(RoutineKind::Window, "anyelement"),
                "FUNCTION \"shop\".\"order total\"(anyelement)",
            ),
            (
                routine(RoutineKind::Procedure, "IN id integer"),
                "PROCEDURE \"shop\".\"order total\"(IN id integer)",
            ),
            (
                routine(RoutineKind::Aggregate, "numeric"),
                "AGGREGATE \"shop\".\"order total\"(numeric)",
            ),
            // An aggregate over no argument is named with a star.
            (
                routine(RoutineKind::Aggregate, ""),
                "AGGREGATE \"shop\".\"order total\"(*)",
            ),
            (
                ObjectRef::Sequence(t()),
                "SEQUENCE \"shop\".\"order items\"",
            ),
            (ObjectRef::Type(t()), "TYPE \"shop\".\"order items\""),
            // A trigger is named within its table.
            (
                ObjectRef::Trigger {
                    table: t(),
                    name: "audit \"it\"".into(),
                },
                "TRIGGER \"audit \"\"it\"\"\" ON \"shop\".\"order items\"",
            ),
            // An index lives in the schema of its table.
            (
                ObjectRef::Index {
                    table: t(),
                    name: "order".into(),
                },
                "INDEX \"shop\".\"order\"",
            ),
            (ObjectRef::Schema("Shop".into()), "SCHEMA \"Shop\""),
            (ObjectRef::Database("my db".into()), "DATABASE \"my db\""),
        ]
    }

    #[test]
    fn rename_and_drop_name_every_kind_of_object() {
        let d = PgDialect;
        for (object, target) in objects() {
            assert_eq!(
                d.rename_sql(&object, "Weird \"name\"").unwrap(),
                format!("ALTER {target} RENAME TO \"Weird \"\"name\"\"\""),
                "{object:?}"
            );
            assert_eq!(
                d.drop_sql(&object).unwrap(),
                format!("DROP {target}"),
                "{object:?}"
            );
        }
    }

    #[test]
    fn scheduled_events_do_not_exist() {
        let d = PgDialect;
        let event = ObjectRef::Event(TableRef::new("shop", "nightly"));
        for result in [d.rename_sql(&event, "x"), d.drop_sql(&event)] {
            let error = result.unwrap_err();
            assert!(matches!(error, Error::Unsupported(_)), "{error:?}");
            assert_eq!(
                error.to_string(),
                "not supported by this database: scheduled events"
            );
        }
    }

    #[test]
    fn truncate_and_create_statements() {
        let d = PgDialect;
        assert_eq!(
            d.truncate_sql(&TableRef::new("shop", "order items")),
            "TRUNCATE TABLE \"shop\".\"order items\""
        );
        assert_eq!(
            d.create_schema_sql("Weird \"name\"").unwrap(),
            "CREATE SCHEMA \"Weird \"\"name\"\"\""
        );
        assert_eq!(
            d.create_database_sql("my db").unwrap(),
            "CREATE DATABASE \"my db\""
        );
    }

    #[test]
    fn table_ddl_from_a_draft() {
        use kerno_core::table_draft::{ColumnDraft, TableChange, TableDraft};
        let d = PgDialect;
        let t = TableRef::new("shop", "order items");
        let mut draft = TableDraft::new(t.clone());
        draft.comment = Some(r"C:\orders".into());
        assert_eq!(
            d.create_table_sql(&draft).unwrap(),
            vec![
                "CREATE TABLE \"shop\".\"order items\" (\n    \
                 \"id\" bigint GENERATED BY DEFAULT AS IDENTITY NOT NULL,\n    \
                 PRIMARY KEY (\"id\")\n)"
                    .to_string(),
                r#"COMMENT ON TABLE "shop"."order items" IS E'C:\\orders'"#.to_string(),
            ]
        );
        let changes = [
            TableChange::DropIndex("by date".into()),
            TableChange::AddColumn(ColumnDraft {
                original: None,
                name: "placed".into(),
                data_type: "timestamptz".into(),
                nullable: false,
                default: Some("now()".into()),
                comment: None,
                auto_increment: false,
                on_update: None,
            }),
            TableChange::RenameTable {
                to: "orders".into(),
            },
        ];
        assert_eq!(
            d.alter_table_sql(&t, &changes).unwrap(),
            vec![
                // An index lives in the schema of its table.
                "DROP INDEX \"shop\".\"by date\"",
                "ALTER TABLE \"shop\".\"order items\" ADD COLUMN \"placed\" timestamptz DEFAULT now() NOT NULL",
                "ALTER TABLE \"shop\".\"order items\" RENAME TO \"orders\"",
            ]
        );
    }

    #[test]
    fn source_scripts_replace_what_the_driver_printed() {
        let d = PgDialect;
        let t = TableRef::new("shop", "v");
        // What `ddl` prints, already `CREATE OR REPLACE`: kept as it is.
        let view = "CREATE OR REPLACE VIEW \"shop\".\"v\" AS\n SELECT 1 AS a;";
        assert_eq!(
            d.replace_source_sql(&ObjectRef::Relation(t.clone(), RelationKind::View), view)
                .unwrap(),
            view
        );
        let function = "CREATE OR REPLACE FUNCTION shop.f()\n RETURNS integer\n LANGUAGE sql\nAS $function$SELECT 1$function$;";
        for kind in [
            RoutineKind::Function,
            RoutineKind::Procedure,
            RoutineKind::Window,
        ] {
            assert_eq!(
                d.replace_source_sql(&routine(kind, ""), function).unwrap(),
                function
            );
        }
        // A materialized view is dropped and created in one transaction.
        let matview = "CREATE MATERIALIZED VIEW \"shop\".\"v\" AS\n SELECT 1 AS a\nWITH DATA;\n";
        assert_eq!(
            d.replace_source_sql(
                &ObjectRef::Relation(t.clone(), RelationKind::MaterializedView),
                matview
            )
            .unwrap(),
            "BEGIN;\n\nDROP MATERIALIZED VIEW \"shop\".\"v\";\n\n\
             CREATE MATERIALIZED VIEW \"shop\".\"v\" AS\n SELECT 1 AS a\nWITH DATA;\n\nCOMMIT;"
        );
        for object in [
            ObjectRef::Relation(t.clone(), RelationKind::Table),
            routine(RoutineKind::Aggregate, "numeric"),
            ObjectRef::Type(t),
        ] {
            assert!(
                matches!(
                    d.replace_source_sql(&object, ""),
                    Err(Error::Unsupported(_))
                ),
                "{object:?}"
            );
        }
    }

    #[test]
    fn extension_and_enum_statements() {
        let d = PgDialect;
        assert_eq!(
            d.create_extension_sql("uuid-ossp").unwrap(),
            "CREATE EXTENSION IF NOT EXISTS \"uuid-ossp\""
        );
        assert_eq!(
            d.drop_extension_sql("citext").unwrap(),
            "DROP EXTENSION \"citext\""
        );
        let mood = TableRef::new("public", "Mood");
        assert_eq!(
            d.create_enum_sql(&mood, &["sad".into(), "it's ok".into()])
                .unwrap(),
            "CREATE TYPE \"public\".\"Mood\" AS ENUM ('sad', 'it''s ok')"
        );
        assert_eq!(
            d.create_enum_sql(&mood, &[]).unwrap(),
            "CREATE TYPE \"public\".\"Mood\" AS ENUM ()"
        );
        assert_eq!(
            d.add_enum_value_sql(&mood, "happy", None).unwrap(),
            "ALTER TYPE \"public\".\"Mood\" ADD VALUE 'happy'"
        );
        assert_eq!(
            d.add_enum_value_sql(&mood, r"a\b", Some("sad")).unwrap(),
            r#"ALTER TYPE "public"."Mood" ADD VALUE E'a\\b' BEFORE 'sad'"#
        );
        assert_eq!(
            d.rename_enum_value_sql(&mood, "sad", "glum").unwrap(),
            "ALTER TYPE \"public\".\"Mood\" RENAME VALUE 'sad' TO 'glum'"
        );
    }

    #[test]
    fn explain_forms() {
        let d = PgDialect;
        assert_eq!(
            d.explain("select 1", false),
            "EXPLAIN (FORMAT JSON) select 1"
        );
        assert_eq!(
            d.explain("select 1", true),
            "EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) select 1"
        );
    }
}
