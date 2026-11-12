//! PostgreSQL's SQL dialect.

use kerno_core::dialect::{Dialect, default_drop_sql, default_rename_sql, hex};
use kerno_core::schema::{ObjectRef, RoutineKind};
use kerno_core::sql::LexRules;
use kerno_core::{Engine, Plan, Result};

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use kerno_core::Error;
    use kerno_core::schema::{RelationKind, TableRef};

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
