//! Reading the catalog: databases, schemas, the objects of a schema and the
//! structure of a table.
//!
//! The queries read `pg_catalog` directly (not `information_schema`, which
//! hides objects the role has no privilege on and omits materialized views)
//! and are written for PostgreSQL 12 and later. Objects that belong to an
//! extension are left out of schema listings, as `pg_dump` does: they are
//! reached through the extension.

use kerno_core::dialect::Dialect;
use kerno_core::schema::{
    ColumnGeneration, ColumnInfo, ConstraintInfo, ConstraintKind, DatabaseInfo, EventInfo,
    ExtensionInfo, ForeignKeyInfo, IndexInfo, RelationInfo, RelationKind, RoutineInfo, RoutineKind,
    SchemaInfo, SchemaObjects, SequenceInfo, TableRef, TableStructure, TriggerInfo, TypeInfo,
    TypeShape,
};
use kerno_core::{Error, Result};
use tokio_postgres::SimpleQueryRow;

use crate::dialect::PgDialect;
use crate::session::PgSession;
use crate::types;

// Row helpers: metadata queries return text.

pub(crate) fn text(row: &SimpleQueryRow, index: usize) -> String {
    row.get(index).unwrap_or_default().to_string()
}

pub(crate) fn opt(row: &SimpleQueryRow, index: usize) -> Option<String> {
    row.get(index)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub(crate) fn flag(row: &SimpleQueryRow, index: usize) -> bool {
    row.get(index) == Some("t")
}

fn number(row: &SimpleQueryRow, index: usize) -> Option<u64> {
    row.get(index).and_then(|value| value.parse().ok())
}

/// A JSON array of strings, as produced by `json_agg(...)::text`. NULL (no
/// rows aggregated) is the empty list.
fn list(row: &SimpleQueryRow, index: usize) -> Vec<String> {
    row.get(index)
        .and_then(|value| serde_json::from_str::<Vec<Option<String>>>(value).ok())
        .map(|items| items.into_iter().map(Option::unwrap_or_default).collect())
        .unwrap_or_default()
}

pub(crate) fn lit(value: &str) -> String {
    PgDialect.quote_string(value)
}

/// Excludes objects owned by an extension. `catalog` is the system catalog
/// the object lives in, `oid` the SQL expression of its oid.
fn not_in_extension(catalog: &str, oid: &str) -> String {
    format!(
        "NOT EXISTS (SELECT 1 FROM pg_catalog.pg_depend dep \
         WHERE dep.classid = 'pg_catalog.{catalog}'::regclass AND dep.objid = {oid} AND dep.deptype = 'e')"
    )
}

pub(crate) async fn databases(session: &PgSession) -> Result<Vec<DatabaseInfo>> {
    let rows = session
        .query(
            "SELECT d.datname, \
                    pg_catalog.pg_get_userbyid(d.datdba), \
                    pg_catalog.pg_encoding_to_char(d.encoding), \
                    d.datcollate, \
                    CASE WHEN d.datallowconn AND pg_catalog.has_database_privilege(d.oid, 'CONNECT') \
                         THEN pg_catalog.pg_database_size(d.oid) END, \
                    d.datistemplate OR NOT d.datallowconn, \
                    pg_catalog.shobj_description(d.oid, 'pg_database') \
             FROM pg_catalog.pg_database d \
             ORDER BY d.datistemplate, d.datname",
        )
        .await?;
    Ok(rows
        .iter()
        .map(|row| DatabaseInfo {
            name: text(row, 0),
            owner: opt(row, 1),
            encoding: opt(row, 2),
            collation: opt(row, 3),
            size_bytes: number(row, 4),
            is_system: flag(row, 5),
            comment: opt(row, 6),
        })
        .collect())
}

pub(crate) async fn schemas(session: &PgSession) -> Result<Vec<SchemaInfo>> {
    // TOAST and per-session temporary schemas are implementation details.
    let rows = session
        .query(
            "SELECT n.nspname, \
                    pg_catalog.pg_get_userbyid(n.nspowner), \
                    n.nspname = 'information_schema' OR n.nspname LIKE 'pg\\_%' AS is_system, \
                    pg_catalog.obj_description(n.oid, 'pg_namespace') \
             FROM pg_catalog.pg_namespace n \
             WHERE n.nspname NOT LIKE 'pg\\_toast%' AND n.nspname NOT LIKE 'pg\\_temp\\_%' \
             ORDER BY is_system, n.nspname",
        )
        .await?;
    Ok(rows
        .iter()
        .map(|row| SchemaInfo {
            name: text(row, 0),
            owner: opt(row, 1),
            is_system: flag(row, 2),
            comment: opt(row, 3),
        })
        .collect())
}

pub(crate) async fn objects(session: &PgSession, schema: &str) -> Result<SchemaObjects> {
    Ok(SchemaObjects {
        relations: relations(session, schema).await?,
        routines: routines(session, schema).await?,
        sequences: sequences(session, schema).await?,
        types: user_types(session, schema).await?,
        triggers: triggers(session, &format!("n.nspname = {}", lit(schema))).await?,
        events: Vec::<EventInfo>::new(),
    })
}

async fn relations(session: &PgSession, schema: &str) -> Result<Vec<RelationInfo>> {
    let sql = format!(
        "SELECT c.relname, c.relkind, c.relispartition, \
                CASE WHEN c.relkind IN ('r', 'p', 'm', 'f') AND c.reltuples >= 0 THEN c.reltuples::bigint END, \
                CASE WHEN c.relkind IN ('r', 'm') THEN pg_catalog.pg_total_relation_size(c.oid) END, \
                pg_catalog.obj_description(c.oid, 'pg_class') \
         FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = {} AND c.relkind IN ('r', 'p', 'f', 'v', 'm') AND {} \
         ORDER BY c.relname",
        lit(schema),
        not_in_extension("pg_class", "c.oid"),
    );
    Ok(session
        .query(&sql)
        .await?
        .iter()
        .map(|row| RelationInfo {
            name: text(row, 0),
            kind: match row.get(1) {
                Some("p") => RelationKind::PartitionedTable,
                Some("f") => RelationKind::ForeignTable,
                Some("v") => RelationKind::View,
                Some("m") => RelationKind::MaterializedView,
                _ => RelationKind::Table,
            },
            is_partition: flag(row, 2),
            row_estimate: number(row, 3),
            size_bytes: number(row, 4),
            comment: opt(row, 5),
            engine: None,
        })
        .collect())
}

async fn routines(session: &PgSession, schema: &str) -> Result<Vec<RoutineInfo>> {
    let sql = format!(
        "SELECT p.proname, p.prokind, \
                pg_catalog.pg_get_function_arguments(p.oid), \
                pg_catalog.pg_get_function_result(p.oid), \
                l.lanname, \
                pg_catalog.pg_get_function_identity_arguments(p.oid), \
                pg_catalog.obj_description(p.oid, 'pg_proc') \
         FROM pg_catalog.pg_proc p \
         JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace \
         JOIN pg_catalog.pg_language l ON l.oid = p.prolang \
         WHERE n.nspname = {} AND {} \
         ORDER BY p.proname, 6",
        lit(schema),
        not_in_extension("pg_proc", "p.oid"),
    );
    Ok(session
        .query(&sql)
        .await?
        .iter()
        .map(|row| RoutineInfo {
            name: text(row, 0),
            kind: match row.get(1) {
                Some("p") => RoutineKind::Procedure,
                Some("a") => RoutineKind::Aggregate,
                Some("w") => RoutineKind::Window,
                _ => RoutineKind::Function,
            },
            arguments: text(row, 2),
            returns: opt(row, 3),
            language: opt(row, 4),
            identity_arguments: text(row, 5),
            comment: opt(row, 6),
        })
        .collect())
}

async fn sequences(session: &PgSession, schema: &str) -> Result<Vec<SequenceInfo>> {
    // last_value is NULL when the role may not read the sequence.
    let sql = format!(
        "SELECT s.sequencename, s.data_type::text, s.start_value, s.increment_by, \
                s.min_value, s.max_value, s.last_value, s.cycle, \
                (SELECT t.relname || '.' || a.attname \
                 FROM pg_catalog.pg_depend d \
                 JOIN pg_catalog.pg_class t ON t.oid = d.refobjid \
                 JOIN pg_catalog.pg_attribute a ON a.attrelid = d.refobjid AND a.attnum = d.refobjsubid \
                 WHERE d.classid = 'pg_catalog.pg_class'::regclass AND d.objid = c.oid \
                   AND d.refclassid = 'pg_catalog.pg_class'::regclass AND d.deptype IN ('a', 'i') \
                 LIMIT 1) \
         FROM pg_catalog.pg_sequences s \
         JOIN pg_catalog.pg_namespace n ON n.nspname = s.schemaname \
         JOIN pg_catalog.pg_class c ON c.relnamespace = n.oid AND c.relname = s.sequencename AND c.relkind = 'S' \
         WHERE s.schemaname = {} AND {} \
         ORDER BY s.sequencename",
        lit(schema),
        not_in_extension("pg_class", "c.oid"),
    );
    Ok(session
        .query(&sql)
        .await?
        .iter()
        .map(|row| SequenceInfo {
            name: text(row, 0),
            data_type: opt(row, 1),
            start: opt(row, 2),
            increment: opt(row, 3),
            min: opt(row, 4),
            max: opt(row, 5),
            last_value: opt(row, 6),
            cycles: flag(row, 7),
            owned_by: opt(row, 8),
        })
        .collect())
}

async fn user_types(session: &PgSession, schema: &str) -> Result<Vec<TypeInfo>> {
    // Composite types that are the row type of a table or view are skipped:
    // only stand-alone ones (relkind 'c') are user-defined types.
    let sql = format!(
        "SELECT t.typname, t.typtype, \
                (SELECT json_agg(e.enumlabel ORDER BY e.enumsortorder)::text \
                 FROM pg_catalog.pg_enum e WHERE e.enumtypid = t.oid), \
                (SELECT json_agg(a.attname ORDER BY a.attnum)::text \
                 FROM pg_catalog.pg_attribute a \
                 WHERE a.attrelid = t.typrelid AND a.attnum > 0 AND NOT a.attisdropped), \
                (SELECT json_agg(pg_catalog.format_type(a.atttypid, a.atttypmod) ORDER BY a.attnum)::text \
                 FROM pg_catalog.pg_attribute a \
                 WHERE a.attrelid = t.typrelid AND a.attnum > 0 AND NOT a.attisdropped), \
                pg_catalog.format_type(t.typbasetype, t.typtypmod), \
                (SELECT json_agg(pg_catalog.pg_get_constraintdef(c.oid, true) ORDER BY c.conname)::text \
                 FROM pg_catalog.pg_constraint c WHERE c.contypid = t.oid), \
                (SELECT pg_catalog.format_type(r.rngsubtype, NULL) \
                 FROM pg_catalog.pg_range r WHERE r.rngtypid = t.oid), \
                pg_catalog.obj_description(t.oid, 'pg_type') \
         FROM pg_catalog.pg_type t \
         JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace \
         LEFT JOIN pg_catalog.pg_class c ON c.oid = t.typrelid \
         WHERE n.nspname = {} AND t.typtype IN ('e', 'c', 'd', 'r') \
           AND (t.typtype <> 'c' OR c.relkind = 'c') AND {} \
         ORDER BY t.typname",
        lit(schema),
        not_in_extension("pg_type", "t.oid"),
    );
    Ok(session
        .query(&sql)
        .await?
        .iter()
        .map(|row| TypeInfo {
            name: text(row, 0),
            shape: match row.get(1) {
                Some("e") => TypeShape::Enum(list(row, 2)),
                Some("c") => {
                    TypeShape::Composite(list(row, 3).into_iter().zip(list(row, 4)).collect())
                }
                Some("d") => TypeShape::Domain {
                    base: text(row, 5),
                    checks: list(row, 6),
                },
                Some("r") => TypeShape::Range(text(row, 7)),
                _ => TypeShape::Other,
            },
            comment: opt(row, 8),
        })
        .collect())
}

/// Triggers matching `filter`, a condition over `c` (the table's `pg_class`
/// row), `n` (its schema) and `t` (the trigger). Internal triggers, which
/// enforce foreign keys, are never listed.
async fn triggers(session: &PgSession, filter: &str) -> Result<Vec<TriggerInfo>> {
    let sql = format!(
        "SELECT t.tgname, c.relname, t.tgtype, t.tgenabled <> 'D', \
                pn.nspname || '.' || p.proname \
         FROM pg_catalog.pg_trigger t \
         JOIN pg_catalog.pg_class c ON c.oid = t.tgrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         JOIN pg_catalog.pg_proc p ON p.oid = t.tgfoid \
         JOIN pg_catalog.pg_namespace pn ON pn.oid = p.pronamespace \
         WHERE NOT t.tgisinternal AND {filter} \
         ORDER BY c.relname, t.tgname"
    );
    Ok(session
        .query(&sql)
        .await?
        .iter()
        .map(|row| {
            let (timing, events, level) = decode_trigger_type(number(row, 2).unwrap_or(0));
            TriggerInfo {
                name: text(row, 0),
                table: text(row, 1),
                timing,
                events,
                level,
                enabled: flag(row, 3),
                action: text(row, 4),
            }
        })
        .collect())
}

/// Decodes `pg_trigger.tgtype`: bit 0 row-level, bit 1 BEFORE, bit 2
/// INSERT, bit 3 DELETE, bit 4 UPDATE, bit 5 TRUNCATE, bit 6 INSTEAD OF.
fn decode_trigger_type(bits: u64) -> (String, String, String) {
    let timing = if bits & 0x40 != 0 {
        "INSTEAD OF"
    } else if bits & 0x02 != 0 {
        "BEFORE"
    } else {
        "AFTER"
    };
    let events: Vec<&str> = [
        (0x04, "INSERT"),
        (0x10, "UPDATE"),
        (0x08, "DELETE"),
        (0x20, "TRUNCATE"),
    ]
    .into_iter()
    .filter(|(bit, _)| bits & bit != 0)
    .map(|(_, name)| name)
    .collect();
    let level = if bits & 0x01 != 0 { "ROW" } else { "STATEMENT" };
    (timing.to_string(), events.join(" OR "), level.to_string())
}

/// The oid of a table-like object, its relkind and its comment.
pub(crate) async fn relation_oid(
    session: &PgSession,
    table: &TableRef,
) -> Result<(String, String, Option<String>)> {
    let sql = format!(
        "SELECT c.oid, c.relkind, pg_catalog.obj_description(c.oid, 'pg_class') \
         FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = {} AND c.relname = {} AND c.relkind IN ('r', 'p', 'f', 'v', 'm')",
        lit(&table.schema),
        lit(&table.name),
    );
    let rows = session.query(&sql).await?;
    let row = rows.first().ok_or_else(|| {
        Error::Db(Box::new(kerno_core::DbError::message(format!(
            "relation \"{}.{}\" does not exist",
            table.schema, table.name
        ))))
    })?;
    Ok((text(row, 0), text(row, 1), opt(row, 2)))
}

pub(crate) async fn table_structure(
    session: &PgSession,
    table: &TableRef,
) -> Result<TableStructure> {
    let (oid, _, comment) = relation_oid(session, table).await?;
    Ok(TableStructure {
        columns: columns(session, &oid).await?,
        indexes: indexes(session, &oid).await?,
        constraints: constraints(session, &oid).await?,
        foreign_keys: foreign_keys(session, &format!("c.conrelid = {oid}")).await?,
        referenced_by: foreign_keys(session, &format!("c.confrelid = {oid}")).await?,
        triggers: triggers(session, &format!("t.tgrelid = {oid}")).await?,
        comment,
    })
}

async fn columns(session: &PgSession, oid: &str) -> Result<Vec<ColumnInfo>> {
    // The element type gives enum labels for arrays of enums too.
    let sql = format!(
        "SELECT a.attname, \
                pg_catalog.format_type(a.atttypid, a.atttypmod), \
                t.typname, t.typtype, t.typcategory, bt.typname, \
                a.attnotnull, \
                pg_catalog.pg_get_expr(ad.adbin, ad.adrelid, true), \
                a.attidentity, a.attgenerated, \
                (SELECT k.ord FROM pg_catalog.pg_index i, \
                        unnest(i.indkey) WITH ORDINALITY AS k(attnum, ord) \
                 WHERE i.indrelid = a.attrelid AND i.indisprimary AND k.attnum = a.attnum), \
                pg_catalog.col_description(a.attrelid, a.attnum), \
                CASE WHEN a.attcollation <> t.typcollation THEN coll.collname END, \
                (SELECT json_agg(e.enumlabel ORDER BY e.enumsortorder)::text \
                 FROM pg_catalog.pg_enum e \
                 WHERE e.enumtypid = CASE WHEN t.typcategory = 'A' THEN t.typelem ELSE a.atttypid END) \
         FROM pg_catalog.pg_attribute a \
         JOIN pg_catalog.pg_type t ON t.oid = a.atttypid \
         LEFT JOIN pg_catalog.pg_type bt ON bt.oid = t.typbasetype \
         LEFT JOIN pg_catalog.pg_attrdef ad ON ad.adrelid = a.attrelid AND ad.adnum = a.attnum \
         LEFT JOIN pg_catalog.pg_collation coll ON coll.oid = a.attcollation \
         WHERE a.attrelid = {oid} AND a.attnum > 0 AND NOT a.attisdropped \
         ORDER BY a.attnum"
    );
    Ok(session
        .query(&sql)
        .await?
        .iter()
        .map(|row| {
            let expression = opt(row, 7);
            let kind = types::kind_of_catalog(
                row.get(2).unwrap_or_default(),
                row.get(3).unwrap_or_default(),
                row.get(4).unwrap_or_default(),
                row.get(5),
            );
            let generation = match (row.get(8), row.get(9)) {
                (Some("a"), _) => Some(ColumnGeneration::IdentityAlways),
                (Some("d"), _) => Some(ColumnGeneration::IdentityByDefault),
                (_, Some("s")) => expression.clone().map(ColumnGeneration::Stored),
                (_, Some("v")) => expression.clone().map(ColumnGeneration::Virtual),
                // `serial` is an integer column whose default draws from a sequence.
                _ if kind == kerno_core::TypeKind::Integer
                    && expression
                        .as_deref()
                        .is_some_and(|e| e.starts_with("nextval(")) =>
                {
                    Some(ColumnGeneration::AutoIncrement)
                }
                _ => None,
            };
            let is_generated_expression = matches!(
                generation,
                Some(ColumnGeneration::Stored(_) | ColumnGeneration::Virtual(_))
            );
            ColumnInfo {
                name: text(row, 0),
                data_type: text(row, 1),
                kind,
                nullable: !flag(row, 6),
                // A generation expression is not a default.
                default: if is_generated_expression {
                    None
                } else {
                    expression
                },
                generation,
                primary_key_position: number(row, 10).map(|position| position as u32),
                comment: opt(row, 11),
                collation: opt(row, 12),
                enum_values: list(row, 13),
            }
        })
        .collect())
}

async fn indexes(session: &PgSession, oid: &str) -> Result<Vec<IndexInfo>> {
    // Key columns only (indnkeyatts): INCLUDE columns are in the definition.
    // A plain column is reported by its name; an expression by its text.
    let sql = format!(
        "SELECT ic.relname, i.indisunique, i.indisprimary, am.amname, \
                pg_catalog.pg_get_expr(i.indpred, i.indrelid, true), \
                pg_catalog.pg_get_indexdef(i.indexrelid), \
                (SELECT json_agg(COALESCE(a.attname, pg_catalog.pg_get_indexdef(i.indexrelid, k.n, true)) ORDER BY k.n)::text \
                 FROM generate_series(1, i.indnkeyatts) AS k(n) \
                 LEFT JOIN pg_catalog.pg_attribute a \
                        ON a.attrelid = i.indrelid AND a.attnum = i.indkey[k.n - 1] AND a.attnum > 0) \
         FROM pg_catalog.pg_index i \
         JOIN pg_catalog.pg_class ic ON ic.oid = i.indexrelid \
         JOIN pg_catalog.pg_am am ON am.oid = ic.relam \
         WHERE i.indrelid = {oid} \
         ORDER BY i.indisprimary DESC, ic.relname"
    );
    Ok(session
        .query(&sql)
        .await?
        .iter()
        .map(|row| IndexInfo {
            name: text(row, 0),
            unique: flag(row, 1),
            primary: flag(row, 2),
            method: text(row, 3),
            predicate: opt(row, 4),
            definition: opt(row, 5),
            columns: list(row, 6),
        })
        .collect())
}

async fn constraints(session: &PgSession, oid: &str) -> Result<Vec<ConstraintInfo>> {
    // Not-null constraints (contype 'n', PostgreSQL 18) are a column
    // property here, not a row per column.
    let sql = format!(
        "SELECT c.conname, c.contype, pg_catalog.pg_get_constraintdef(c.oid, true), \
                (SELECT json_agg(a.attname ORDER BY k.ord)::text \
                 FROM unnest(c.conkey) WITH ORDINALITY AS k(attnum, ord) \
                 JOIN pg_catalog.pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k.attnum) \
         FROM pg_catalog.pg_constraint c \
         WHERE c.conrelid = {oid} AND c.contype IN ('p', 'u', 'c', 'x') \
         ORDER BY pg_catalog.array_position(ARRAY['p', 'u', 'x', 'c']::\"char\"[], c.contype), c.conname"
    );
    Ok(session
        .query(&sql)
        .await?
        .iter()
        .map(|row| ConstraintInfo {
            name: text(row, 0),
            kind: match row.get(1) {
                Some("p") => ConstraintKind::PrimaryKey,
                Some("u") => ConstraintKind::Unique,
                Some("x") => ConstraintKind::Exclusion,
                _ => ConstraintKind::Check,
            },
            definition: text(row, 2),
            columns: list(row, 3),
        })
        .collect())
}

/// Foreign keys matching `filter` over `c` (`pg_constraint`). Columns are
/// paired by position in `conkey` / `confkey`. Clones on partitions
/// (`conparentid <> 0`) are skipped: the parent's constraint is the one
/// the user declared.
async fn foreign_keys(session: &PgSession, filter: &str) -> Result<Vec<ForeignKeyInfo>> {
    let sql = format!(
        "SELECT c.conname, n.nspname, t.relname, \
                (SELECT json_agg(a.attname ORDER BY k.ord)::text \
                 FROM unnest(c.conkey) WITH ORDINALITY AS k(attnum, ord) \
                 JOIN pg_catalog.pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k.attnum), \
                rn.nspname, rt.relname, \
                (SELECT json_agg(a.attname ORDER BY k.ord)::text \
                 FROM unnest(c.confkey) WITH ORDINALITY AS k(attnum, ord) \
                 JOIN pg_catalog.pg_attribute a ON a.attrelid = c.confrelid AND a.attnum = k.attnum), \
                c.confupdtype, c.confdeltype \
         FROM pg_catalog.pg_constraint c \
         JOIN pg_catalog.pg_class t ON t.oid = c.conrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
         JOIN pg_catalog.pg_class rt ON rt.oid = c.confrelid \
         JOIN pg_catalog.pg_namespace rn ON rn.oid = rt.relnamespace \
         WHERE c.contype = 'f' AND c.conparentid = 0 AND {filter} \
         ORDER BY n.nspname, t.relname, c.conname"
    );
    let action = |code: Option<&str>| {
        match code {
            Some("r") => "RESTRICT",
            Some("c") => "CASCADE",
            Some("n") => "SET NULL",
            Some("d") => "SET DEFAULT",
            _ => "NO ACTION",
        }
        .to_string()
    };
    Ok(session
        .query(&sql)
        .await?
        .iter()
        .map(|row| ForeignKeyInfo {
            name: text(row, 0),
            table: TableRef::new(text(row, 1), text(row, 2)),
            columns: list(row, 3),
            referenced_table: TableRef::new(text(row, 4), text(row, 5)),
            referenced_columns: list(row, 6),
            on_update: action(row.get(7)),
            on_delete: action(row.get(8)),
        })
        .collect())
}

pub(crate) async fn extensions(session: &PgSession) -> Result<Vec<ExtensionInfo>> {
    let rows = session
        .query(
            "SELECT a.name, a.installed_version, a.default_version, n.nspname, a.comment \
             FROM pg_catalog.pg_available_extensions a \
             LEFT JOIN pg_catalog.pg_extension e ON e.extname = a.name \
             LEFT JOIN pg_catalog.pg_namespace n ON n.oid = e.extnamespace \
             ORDER BY a.installed_version IS NULL, a.name",
        )
        .await?;
    Ok(rows
        .iter()
        .map(|row| ExtensionInfo {
            name: text(row, 0),
            installed_version: opt(row, 1),
            default_version: opt(row, 2),
            schema: opt(row, 3),
            comment: opt(row, 4),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_type_bits() {
        let decode = |bits| {
            let (timing, events, level) = decode_trigger_type(bits);
            format!("{timing} {events} FOR EACH {level}")
        };
        // BEFORE UPDATE, row level: 1 + 2 + 16.
        assert_eq!(decode(19), "BEFORE UPDATE FOR EACH ROW");
        // AFTER UPDATE, row level: 1 + 16.
        assert_eq!(decode(17), "AFTER UPDATE FOR EACH ROW");
        // AFTER INSERT OR UPDATE OR DELETE, statement level: 4 + 8 + 16.
        assert_eq!(
            decode(28),
            "AFTER INSERT OR UPDATE OR DELETE FOR EACH STATEMENT"
        );
        // INSTEAD OF INSERT on a view, row level: 1 + 4 + 64.
        assert_eq!(decode(69), "INSTEAD OF INSERT FOR EACH ROW");
        // BEFORE TRUNCATE, statement level: 2 + 32.
        assert_eq!(decode(34), "BEFORE TRUNCATE FOR EACH STATEMENT");
    }

    #[test]
    fn extension_filter_names_the_catalog() {
        let filter = not_in_extension("pg_proc", "p.oid");
        assert!(filter.contains("'pg_catalog.pg_proc'::regclass"));
        assert!(filter.contains("dep.objid = p.oid"));
        assert!(filter.contains("deptype = 'e'"));
    }
}
