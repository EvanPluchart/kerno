//! PostgreSQL's SQL dialect.

use kerno_core::dialect::{
    DatabaseOptions, Dialect, IsolationLevel, default_drop_sql, default_rename_sql, hex,
};
use kerno_core::maintenance::{Maintenance, MaintenanceSql};
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

/// A session id as `pg_stat_activity.pid`: nothing else reaches the SQL.
fn backend_pid(id: &str) -> Result<i32> {
    id.trim()
        .parse()
        .map_err(|_| Error::Config(format!("\"{id}\" is not a session id")))
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

    fn read_only_session_sql(&self) -> Option<&'static str> {
        Some("SET default_transaction_read_only = on")
    }

    /// READ UNCOMMITTED is accepted but behaves as READ COMMITTED here: it
    /// is not offered.
    fn isolation_levels(&self) -> &'static [IsolationLevel] {
        &[
            IsolationLevel::ReadCommitted,
            IsolationLevel::RepeatableRead,
            IsolationLevel::Serializable,
        ]
    }

    fn set_isolation_sql(&self, level: IsolationLevel) -> String {
        format!(
            "SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL {}",
            level.sql()
        )
    }

    fn isolation_sql(&self) -> &'static str {
        "SHOW transaction_isolation"
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

    fn cancel_session_sql(&self, id: &str) -> Result<String> {
        Ok(format!(
            "SELECT pg_catalog.pg_cancel_backend({})",
            backend_pid(id)?
        ))
    }

    fn terminate_session_sql(&self, id: &str) -> Result<String> {
        Ok(format!(
            "SELECT pg_catalog.pg_terminate_backend({})",
            backend_pid(id)?
        ))
    }

    /// `reltuples`, as the last `ANALYZE` or `VACUUM` left it.
    fn estimated_rows_sql(&self, table: &TableRef) -> String {
        format!(
            "SELECT c.reltuples::bigint FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = {} AND c.relname = {}",
            self.quote_string(&table.schema),
            self.quote_string(&table.name)
        )
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

    /// Ported from DBeaver's PostgreSQL tools (`ext.postgresql/.../tasks/`
    /// `PostgreToolBaseVacuum.java`, `PostgreToolBaseAnalyze.java`,
    /// `PostgreToolMViewRefresh.java`, `PostgreToolTableTruncate.java`,
    /// `PostgreToolTableTriggerEnable.java`, `PostgreToolTableTriggerDisable.java`).
    /// `VACUUM` and `ANALYZE` are always `VERBOSE`, as there: the server
    /// reports through notices. Unlike there, `TRUNCATE` writes only the
    /// options asked for (no `CONTINUE IDENTITY` / `RESTRICT`, the
    /// defaults), `CONCURRENTLY` is offered, and views and foreign tables
    /// get no `VACUUM` (the server skips them with a warning).
    fn maintenance_sql(&self, tool: &Maintenance, target: &ObjectRef) -> Result<MaintenanceSql> {
        use RelationKind::{ForeignTable, MaterializedView, PartitionedTable, Table};
        let unsupported = || Error::Unsupported("this tool on this kind of object".into());
        // ` s.t` for a relation of `kinds`; nothing for the database.
        let on = |kinds: &[RelationKind]| match target {
            ObjectRef::Relation(table, kind) if kinds.contains(kind) => {
                Ok(format!(" {}", self.table_name(table)))
            }
            ObjectRef::Database(_) => Ok(String::new()),
            _ => Err(unsupported()),
        };
        let sql = match (tool, target) {
            (Maintenance::Vacuum(o), _) => {
                let options: Vec<&str> = [
                    (o.full, "FULL"),
                    (o.freeze, "FREEZE"),
                    (true, "VERBOSE"),
                    (o.analyze, "ANALYZE"),
                    (o.disable_page_skipping, "DISABLE_PAGE_SKIPPING"),
                    (o.skip_locked, "SKIP_LOCKED"),
                    (o.index_cleanup, "INDEX_CLEANUP"),
                ]
                .into_iter()
                .filter_map(|(on, word)| on.then_some(word))
                .collect();
                let on = on(&[Table, PartitionedTable, MaterializedView])?;
                format!("VACUUM ({}){on}", options.join(", "))
            }
            (Maintenance::Analyze, _) => format!(
                "ANALYZE VERBOSE{}",
                on(&[Table, PartitionedTable, ForeignTable, MaterializedView])?
            ),
            (
                Maintenance::RefreshMaterializedView(o),
                ObjectRef::Relation(view, MaterializedView),
            ) => {
                format!(
                    "REFRESH MATERIALIZED VIEW {}{} WITH {}DATA",
                    if o.concurrently { "CONCURRENTLY " } else { "" },
                    self.table_name(view),
                    if o.with_data { "" } else { "NO " }
                )
            }
            // Foreign tables too: the server truncates them through their
            // wrapper since PostgreSQL 14, and says so when it cannot.
            (
                Maintenance::Truncate(o),
                ObjectRef::Relation(table, Table | PartitionedTable | ForeignTable),
            ) => {
                format!(
                    "TRUNCATE TABLE {}{}{}{}",
                    if o.only { "ONLY " } else { "" },
                    self.table_name(table),
                    if o.restart_identity {
                        " RESTART IDENTITY"
                    } else {
                        ""
                    },
                    if o.cascade { " CASCADE" } else { "" }
                )
            }
            (
                Maintenance::EnableTrigger | Maintenance::DisableTrigger,
                ObjectRef::Trigger { table, name },
            ) => format!(
                "ALTER TABLE {} {} TRIGGER {}",
                self.table_name(table),
                if *tool == Maintenance::EnableTrigger {
                    "ENABLE"
                } else {
                    "DISABLE"
                },
                self.quote_ident(name)
            ),
            _ => return Err(unsupported()),
        };
        Ok(MaintenanceSql {
            sql,
            returns_rows: false,
            autocommit: matches!(tool, Maintenance::Vacuum(_)),
        })
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

    /// Roles, databases and tablespaces are names; the encoding and the
    /// locales are strings. Another encoding or locale than the template's
    /// needs `template0` as the template: the server says so otherwise.
    fn create_database_with_sql(&self, name: &str, options: &DatabaseOptions) -> Result<String> {
        let mut sql = format!("CREATE DATABASE {}", self.quote_ident(name));
        let mut with = |key: &str, value: &Option<String>, as_name: bool| {
            if let Some(value) = value.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
                let value = if as_name {
                    self.quote_ident(value)
                } else {
                    self.quote_string(value)
                };
                sql.push_str(&format!("\n    {key} = {value}"));
            }
        };
        with("OWNER", &options.owner, true);
        with("TEMPLATE", &options.template, true);
        with("ENCODING", &options.encoding, false);
        with("LC_COLLATE", &options.collation, false);
        with("LC_CTYPE", &options.ctype, false);
        with("TABLESPACE", &options.tablespace, true);
        Ok(sql)
    }

    /// The encodings a server can store are the first 35 of PostgreSQL's
    /// list (`SQL_ASCII` to `KOI8U`, "Server: Yes" in the documentation's
    /// table of character sets); the others are client-only. Locales are
    /// the operating system's (`libc` collations), each for one encoding or
    /// for any; `pg_global` holds shared catalogs, not databases.
    fn database_options_sql(&self) -> Option<&'static str> {
        Some(
            "\
SELECT 'owner' AS field, r.rolname::text AS value, NULL::text AS applies_to
FROM pg_catalog.pg_roles r
WHERE r.rolname !~ '^pg_'
UNION ALL
SELECT 'template', d.datname::text, NULL
FROM pg_catalog.pg_database d
UNION ALL
SELECT 'encoding', pg_catalog.pg_encoding_to_char(e), NULL
FROM generate_series(0, 34) e
UNION ALL
SELECT DISTINCT 'collation', c.collcollate,
       CASE WHEN c.collencoding >= 0 THEN pg_catalog.pg_encoding_to_char(c.collencoding) END
FROM pg_catalog.pg_collation c
WHERE c.collprovider = 'c' AND c.collcollate IS NOT NULL
UNION ALL
SELECT 'tablespace', t.spcname::text, NULL
FROM pg_catalog.pg_tablespace t
WHERE t.spcname <> 'pg_global'
ORDER BY 1, 2",
        )
    }

    /// From `pg_class` and the statistics collector. PostgreSQL does not
    /// record when a table was created or altered.
    fn properties_sql(&self, table: &TableRef, _kind: RelationKind) -> Option<String> {
        Some(format!(
            "\
SELECT pg_catalog.pg_get_userbyid(c.relowner) AS \"Owner\",
       CASE c.relkind WHEN 'r' THEN 'table' WHEN 'p' THEN 'partitioned table'
                      WHEN 'f' THEN 'foreign table' WHEN 'v' THEN 'view'
                      WHEN 'm' THEN 'materialized view' ELSE c.relkind::text END AS \"Kind\",
       CASE c.relpersistence WHEN 'p' THEN 'permanent' WHEN 'u' THEN 'unlogged'
                             WHEN 't' THEN 'temporary' END AS \"Persistence\",
       CASE WHEN c.relkind <> 'v' THEN COALESCE(ts.spcname, '(database default)') END AS \"Tablespace\",
       am.amname AS \"Access method\",
       CASE WHEN c.relkind <> 'v' AND c.reltuples >= 0 THEN c.reltuples::bigint END AS \"Estimated rows\",
       CASE WHEN c.relkind <> 'v' THEN pg_catalog.pg_table_size(c.oid) END AS \"Table size\",
       CASE WHEN c.relkind <> 'v' THEN pg_catalog.pg_indexes_size(c.oid) END AS \"Index size\",
       CASE WHEN c.relkind <> 'v' THEN pg_catalog.pg_total_relation_size(c.oid) END AS \"Total size\",
       c.relrowsecurity AS \"Row-level security\",
       pg_catalog.array_to_string(c.reloptions, ', ') AS \"Options\",
       date_trunc('second', GREATEST(s.last_vacuum, s.last_autovacuum)) AS \"Last vacuum\",
       date_trunc('second', GREATEST(s.last_analyze, s.last_autoanalyze)) AS \"Last analyze\",
       c.oid AS \"OID\",
       pg_catalog.obj_description(c.oid, 'pg_class') AS \"Comment\"
FROM pg_catalog.pg_class c
JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
LEFT JOIN pg_catalog.pg_tablespace ts ON ts.oid = c.reltablespace
LEFT JOIN pg_catalog.pg_am am ON am.oid = c.relam
LEFT JOIN pg_catalog.pg_stat_all_tables s ON s.relid = c.oid
WHERE n.nspname = {} AND c.relname = {}",
            self.quote_string(&table.schema),
            self.quote_string(&table.name)
        ))
    }

    /// From `pg_database`; the size needs the CONNECT privilege.
    fn database_properties_sql(&self, database: &str) -> Option<String> {
        Some(format!(
            "\
SELECT pg_catalog.pg_get_userbyid(d.datdba) AS \"Owner\",
       pg_catalog.pg_encoding_to_char(d.encoding) AS \"Encoding\",
       d.datcollate AS \"Collation\",
       d.datctype AS \"Character type\",
       t.spcname AS \"Tablespace\",
       CASE WHEN d.datconnlimit < 0 THEN 'no limit' ELSE d.datconnlimit::text END AS \"Connection limit\",
       d.datistemplate AS \"Template\",
       d.datallowconn AS \"Allows connections\",
       CASE WHEN pg_catalog.has_database_privilege(d.oid, 'CONNECT')
            THEN pg_catalog.pg_database_size(d.oid) END AS \"Total size\",
       d.oid AS \"OID\",
       pg_catalog.shobj_description(d.oid, 'pg_database') AS \"Comment\"
FROM pg_catalog.pg_database d
LEFT JOIN pg_catalog.pg_tablespace t ON t.oid = d.dattablespace
WHERE d.datname = {}",
            self.quote_string(database)
        ))
    }

    /// The relation of an OID from the row description, with its columns
    /// (dropped ones left out).
    fn origin_sql(&self, source: &kerno_core::editable::Source) -> Option<String> {
        let kerno_core::editable::Source::Oid(oid) = source else {
            return None;
        };
        Some(format!(
            "\
SELECT n.nspname, c.relname, c.relkind::text, a.attnum, a.attname
FROM pg_catalog.pg_class c
JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
JOIN pg_catalog.pg_attribute a ON a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped
WHERE c.oid = {oid}
ORDER BY a.attnum"
        ))
    }

    /// From `pg_namespace`, with what the schema holds.
    fn schema_properties_sql(&self, schema: &str) -> Option<String> {
        Some(format!(
            "\
SELECT pg_catalog.pg_get_userbyid(n.nspowner) AS \"Owner\",
       (SELECT count(*) FROM pg_catalog.pg_class c
        WHERE c.relnamespace = n.oid AND c.relkind IN ('r', 'p', 'f')) AS \"Tables\",
       (SELECT count(*) FROM pg_catalog.pg_class c
        WHERE c.relnamespace = n.oid AND c.relkind IN ('v', 'm')) AS \"Views\",
       (SELECT COALESCE(sum(pg_catalog.pg_total_relation_size(c.oid)), 0) FROM pg_catalog.pg_class c
        WHERE c.relnamespace = n.oid AND c.relkind IN ('r', 'p', 'm')) AS \"Total size\",
       n.oid AS \"OID\",
       pg_catalog.obj_description(n.oid, 'pg_namespace') AS \"Comment\"
FROM pg_catalog.pg_namespace n
WHERE n.nspname = {}",
            self.quote_string(schema)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolation_is_set_for_the_session_s_transactions() {
        let d = PgDialect;
        assert_eq!(
            d.isolation_levels(),
            [
                IsolationLevel::ReadCommitted,
                IsolationLevel::RepeatableRead,
                IsolationLevel::Serializable
            ]
        );
        assert_eq!(
            d.set_isolation_sql(IsolationLevel::Serializable),
            "SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL SERIALIZABLE"
        );
        assert_eq!(d.isolation_sql(), "SHOW transaction_isolation");
    }

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

    #[test]
    fn the_row_estimate_is_read_from_the_catalog() {
        assert_eq!(
            PgDialect.estimated_rows_sql(&TableRef::new("shop", "it's")),
            "SELECT c.reltuples::bigint FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = 'shop' AND c.relname = 'it''s'"
        );
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
    fn signals_name_a_backend_by_its_number_only() {
        let d = PgDialect;
        assert_eq!(
            d.cancel_session_sql(" 4242 ").unwrap(),
            "SELECT pg_catalog.pg_cancel_backend(4242)"
        );
        assert_eq!(
            d.terminate_session_sql("17").unwrap(),
            "SELECT pg_catalog.pg_terminate_backend(17)"
        );
        for id in ["", "not a pid", "1); DROP TABLE x; --"] {
            let error = d.cancel_session_sql(id).unwrap_err();
            assert!(matches!(error, Error::Config(_)), "{id}: {error:?}");
            assert!(d.terminate_session_sql(id).is_err(), "{id}");
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
        let options = DatabaseOptions {
            owner: Some("app owner".into()),
            template: Some("template0".into()),
            encoding: Some("UTF8".into()),
            collation: Some("C".into()),
            ctype: Some("en_US.utf8".into()),
            tablespace: Some("pg_default".into()),
        };
        assert_eq!(
            d.create_database_with_sql("my db", &options).unwrap(),
            "CREATE DATABASE \"my db\"\n    OWNER = \"app owner\"\n    TEMPLATE = \"template0\"\
             \n    ENCODING = 'UTF8'\n    LC_COLLATE = 'C'\n    LC_CTYPE = 'en_US.utf8'\
             \n    TABLESPACE = \"pg_default\""
        );
        // Blank fields are the server's defaults; quotes are doubled.
        let options = DatabaseOptions {
            owner: Some("  ".into()),
            collation: Some("it's".into()),
            ..DatabaseOptions::default()
        };
        assert_eq!(
            d.create_database_with_sql("x", &options).unwrap(),
            "CREATE DATABASE \"x\"\n    LC_COLLATE = 'it''s'"
        );
        assert!(d.database_options_sql().is_some());
        let properties = d
            .properties_sql(&TableRef::new("sh'op", "orders"), RelationKind::Table)
            .unwrap();
        assert!(properties.contains("n.nspname = 'sh''op' AND c.relname = 'orders'"));
        let database = d.database_properties_sql("it's").unwrap();
        assert!(database.contains("WHERE d.datname = 'it''s'"), "{database}");
        let schema = d.schema_properties_sql("sh'op").unwrap();
        assert!(schema.contains("WHERE n.nspname = 'sh''op'"), "{schema}");
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
                auto_increment: kerno_core::table_draft::AutoIncrement::Off,
                on_update: None,
                invisible: false,
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

    mod maintenance {
        use super::*;
        use kerno_core::maintenance::{
            RefreshOptions, RepairOptions, TruncateOptions, VacuumOptions,
        };

        fn odd() -> TableRef {
            TableRef::new("shop", "order \"items\"")
        }

        const ODD: &str = "\"shop\".\"order \"\"items\"\"\"";

        fn relation(kind: RelationKind) -> ObjectRef {
            ObjectRef::Relation(odd(), kind)
        }

        fn sql(tool: Maintenance, target: &ObjectRef) -> String {
            let out = PgDialect.maintenance_sql(&tool, target).unwrap();
            assert!(!out.returns_rows, "{}", out.sql);
            assert_eq!(
                out.autocommit,
                matches!(tool, Maintenance::Vacuum(_)),
                "{}",
                out.sql
            );
            out.sql
        }

        #[test]
        fn vacuum_writes_each_option_in_order_and_is_always_verbose() {
            let table = relation(RelationKind::Table);
            let vacuum = |o| sql(Maintenance::Vacuum(o), &table);
            assert_eq!(
                vacuum(VacuumOptions::default()),
                format!("VACUUM (VERBOSE) {ODD}")
            );
            assert_eq!(
                vacuum(VacuumOptions {
                    full: true,
                    freeze: true,
                    analyze: true,
                    disable_page_skipping: true,
                    skip_locked: true,
                    index_cleanup: true,
                }),
                format!(
                    "VACUUM (FULL, FREEZE, VERBOSE, ANALYZE, DISABLE_PAGE_SKIPPING, \
                     SKIP_LOCKED, INDEX_CLEANUP) {ODD}"
                )
            );
            assert_eq!(
                vacuum(VacuumOptions {
                    analyze: true,
                    ..VacuumOptions::default()
                }),
                format!("VACUUM (VERBOSE, ANALYZE) {ODD}")
            );
            let database = ObjectRef::Database("any".into());
            assert_eq!(
                sql(Maintenance::Vacuum(VacuumOptions::default()), &database),
                "VACUUM (VERBOSE)"
            );
            assert_eq!(sql(Maintenance::Analyze, &database), "ANALYZE VERBOSE");
            assert_eq!(
                sql(Maintenance::Analyze, &table),
                format!("ANALYZE VERBOSE {ODD}")
            );
        }

        #[test]
        fn refresh_truncate_and_triggers() {
            let view = relation(RelationKind::MaterializedView);
            let refresh = |concurrently, with_data| {
                sql(
                    Maintenance::RefreshMaterializedView(RefreshOptions {
                        concurrently,
                        with_data,
                    }),
                    &view,
                )
            };
            assert_eq!(
                refresh(false, true),
                format!("REFRESH MATERIALIZED VIEW {ODD} WITH DATA")
            );
            assert_eq!(
                refresh(false, false),
                format!("REFRESH MATERIALIZED VIEW {ODD} WITH NO DATA")
            );
            assert_eq!(
                refresh(true, true),
                format!("REFRESH MATERIALIZED VIEW CONCURRENTLY {ODD} WITH DATA")
            );

            let table = relation(RelationKind::Table);
            let truncate = |o| sql(Maintenance::Truncate(o), &table);
            assert_eq!(
                truncate(TruncateOptions::default()),
                PgDialect.truncate_sql(&odd())
            );
            assert_eq!(
                truncate(TruncateOptions {
                    only: true,
                    restart_identity: true,
                    cascade: true,
                }),
                format!("TRUNCATE TABLE ONLY {ODD} RESTART IDENTITY CASCADE")
            );
            assert_eq!(
                truncate(TruncateOptions {
                    restart_identity: true,
                    ..TruncateOptions::default()
                }),
                format!("TRUNCATE TABLE {ODD} RESTART IDENTITY")
            );

            let trigger = ObjectRef::Trigger {
                table: odd(),
                name: "audit \"it\"".into(),
            };
            assert_eq!(
                sql(Maintenance::DisableTrigger, &trigger),
                format!("ALTER TABLE {ODD} DISABLE TRIGGER \"audit \"\"it\"\"\"")
            );
            assert_eq!(
                sql(Maintenance::EnableTrigger, &trigger),
                format!("ALTER TABLE {ODD} ENABLE TRIGGER \"audit \"\"it\"\"\"")
            );
        }

        #[test]
        fn each_kind_of_object_lists_its_tools() {
            let names = |target: &ObjectRef| -> Vec<&'static str> {
                PgDialect
                    .maintenance_tools(target)
                    .iter()
                    .map(|tool| match tool {
                        Maintenance::Vacuum(_) => "vacuum",
                        Maintenance::Analyze => "analyze",
                        Maintenance::RefreshMaterializedView(_) => "refresh",
                        Maintenance::Truncate(_) => "truncate",
                        Maintenance::EnableTrigger => "enable",
                        Maintenance::DisableTrigger => "disable",
                        Maintenance::Check(_) => "check",
                        Maintenance::Optimize => "optimize",
                        Maintenance::Repair(_) => "repair",
                    })
                    .collect()
            };
            let table = ["vacuum", "analyze", "truncate"];
            assert_eq!(names(&relation(RelationKind::Table)), table);
            assert_eq!(names(&relation(RelationKind::PartitionedTable)), table);
            assert_eq!(
                names(&relation(RelationKind::MaterializedView)),
                ["vacuum", "analyze", "refresh"]
            );
            assert_eq!(
                names(&relation(RelationKind::ForeignTable)),
                ["analyze", "truncate"]
            );
            assert!(names(&relation(RelationKind::View)).is_empty());
            assert_eq!(
                names(&ObjectRef::Database("kerno".into())),
                ["vacuum", "analyze"]
            );
            assert_eq!(
                names(&ObjectRef::Trigger {
                    table: odd(),
                    name: "t".into()
                }),
                ["enable", "disable"]
            );
            for nothing in [
                ObjectRef::Schema("shop".into()),
                ObjectRef::Sequence(odd()),
                ObjectRef::Type(odd()),
                ObjectRef::Index {
                    table: odd(),
                    name: "i".into(),
                },
            ] {
                assert!(names(&nothing).is_empty(), "{nothing:?}");
            }
            // MySQL's tools are refused with `Unsupported`.
            for tool in [
                Maintenance::Check(None),
                Maintenance::Optimize,
                Maintenance::Repair(RepairOptions::default()),
            ] {
                assert!(matches!(
                    PgDialect.maintenance_sql(&tool, &relation(RelationKind::Table)),
                    Err(Error::Unsupported(_))
                ));
            }
        }
    }
}
