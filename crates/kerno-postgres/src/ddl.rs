//! Rebuilding `CREATE` statements. PostgreSQL has no `SHOW CREATE TABLE`:
//! a table's DDL is assembled from the catalog, and everything else comes
//! from the server's own `pg_get_*def` functions.
//!
//! The order of a table script (columns, constraints, partitioning, then
//! indexes, comments and triggers as separate statements) follows what
//! `pg_dump` emits and what DBeaver Community generates in
//! `org.jkiss.dbeaver.model/.../sql/edit/struct/SQLTableManager.java`
//! (Apache-2.0); the code here is written from the catalog documentation.

use kerno_core::dialect::Dialect;
use kerno_core::schema::{
    ColumnGeneration, ColumnInfo, ObjectRef, RelationKind, RoutineKind, TableRef, TypeShape,
};
use kerno_core::{DbError, Error, Result};

use crate::dialect::PgDialect;
use crate::introspect::{self, flag, lit, opt, text};
use crate::session::PgSession;

const D: PgDialect = PgDialect;

fn not_found(what: &str) -> Error {
    Error::Db(Box::new(DbError::message(format!("{what} does not exist"))))
}

pub(crate) async fn ddl(session: &PgSession, object: &ObjectRef) -> Result<String> {
    match object {
        ObjectRef::Relation(table, RelationKind::View) => view(session, table, false).await,
        ObjectRef::Relation(table, RelationKind::MaterializedView) => {
            view(session, table, true).await
        }
        ObjectRef::Relation(table, _) => self::table(session, table).await,
        ObjectRef::Routine {
            schema,
            name,
            kind,
            identity_arguments,
        } => routine(session, schema, name, *kind, identity_arguments).await,
        ObjectRef::Sequence(sequence) => self::sequence(session, sequence).await,
        ObjectRef::Type(ty) => user_type(session, ty).await,
        ObjectRef::Trigger { table, name } => {
            let sql = format!(
                "SELECT pg_catalog.pg_get_triggerdef(t.oid, true) \
                 FROM pg_catalog.pg_trigger t \
                 JOIN pg_catalog.pg_class c ON c.oid = t.tgrelid \
                 JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = {} AND c.relname = {} AND t.tgname = {} AND NOT t.tgisinternal",
                lit(&table.schema),
                lit(&table.name),
                lit(name),
            );
            single(session, &sql, &format!("trigger \"{name}\""))
                .await
                .map(terminated)
        }
        ObjectRef::Index { table, name } => {
            let sql = format!(
                "SELECT pg_catalog.pg_get_indexdef(c.oid) \
                 FROM pg_catalog.pg_class c \
                 JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = {} AND c.relname = {} AND c.relkind IN ('i', 'I')",
                lit(&table.schema),
                lit(name),
            );
            single(session, &sql, &format!("index \"{name}\""))
                .await
                .map(terminated)
        }
        ObjectRef::Schema(name) => {
            let sql = format!(
                "SELECT pg_catalog.pg_get_userbyid(n.nspowner) FROM pg_catalog.pg_namespace n WHERE n.nspname = {}",
                lit(name)
            );
            let owner = single(session, &sql, &format!("schema \"{name}\"")).await?;
            Ok(format!(
                "CREATE SCHEMA {} AUTHORIZATION {};",
                D.quote_ident(name),
                D.quote_ident(&owner)
            ))
        }
        ObjectRef::Database(name) => {
            let sql = format!(
                "SELECT pg_catalog.pg_get_userbyid(d.datdba), pg_catalog.pg_encoding_to_char(d.encoding), \
                        d.datcollate, d.datctype \
                 FROM pg_catalog.pg_database d WHERE d.datname = {}",
                lit(name)
            );
            let rows = session.query(&sql).await?;
            let row = rows
                .first()
                .ok_or_else(|| not_found(&format!("database \"{name}\"")))?;
            Ok(format!(
                "CREATE DATABASE {}\n    WITH OWNER = {}\n    ENCODING = {}\n    LC_COLLATE = {}\n    LC_CTYPE = {};",
                D.quote_ident(name),
                D.quote_ident(&text(row, 0)),
                lit(&text(row, 1)),
                lit(&text(row, 2)),
                lit(&text(row, 3)),
            ))
        }
        ObjectRef::Event(_) => Err(Error::Unsupported("scheduled events".into())),
    }
}

/// The single value a one-row, one-column query returns.
async fn single(session: &PgSession, sql: &str, what: &str) -> Result<String> {
    session
        .query(sql)
        .await?
        .first()
        .and_then(|row| row.get(0).map(str::to_string))
        .ok_or_else(|| not_found(what))
}

/// `sql` with exactly one trailing semicolon.
fn terminated(sql: String) -> String {
    format!("{};", sql.trim_end().trim_end_matches(';'))
}

/// One column line of `CREATE TABLE`.
fn column_line(column: &ColumnInfo) -> String {
    let mut line = D.quote_ident(&column.name);
    line.push(' ');
    // `serial` is shorthand for a NOT NULL integer column fed by a sequence
    // it owns; print it that way instead of exposing the sequence's
    // nextval(). A nullable one is not what `serial` would create.
    let serial = match (&column.generation, column.data_type.as_str()) {
        _ if column.nullable => None,
        (Some(ColumnGeneration::AutoIncrement), "smallint") => Some("smallserial"),
        (Some(ColumnGeneration::AutoIncrement), "integer") => Some("serial"),
        (Some(ColumnGeneration::AutoIncrement), "bigint") => Some("bigserial"),
        _ => None,
    };
    line.push_str(serial.unwrap_or(&column.data_type));
    if let Some(collation) = &column.collation {
        line.push_str(&format!(" COLLATE {}", D.quote_ident(collation)));
    }
    match &column.generation {
        Some(ColumnGeneration::IdentityAlways) => line.push_str(" GENERATED ALWAYS AS IDENTITY"),
        Some(ColumnGeneration::IdentityByDefault) => {
            line.push_str(" GENERATED BY DEFAULT AS IDENTITY")
        }
        Some(ColumnGeneration::Stored(expression)) => {
            line.push_str(&format!(" GENERATED ALWAYS AS ({expression}) STORED"));
        }
        Some(ColumnGeneration::Virtual(expression)) => {
            line.push_str(&format!(" GENERATED ALWAYS AS ({expression}) VIRTUAL"));
        }
        Some(ColumnGeneration::AutoIncrement) | None => {}
    }
    if serial.is_none()
        && let Some(default) = &column.default
    {
        line.push_str(&format!(" DEFAULT {default}"));
    }
    // serial implies NOT NULL.
    if !column.nullable && serial.is_none() {
        line.push_str(" NOT NULL");
    }
    line
}

/// `COMMENT ON` statements for a relation and its columns.
fn comments(
    keyword: &str,
    name: &str,
    table_comment: Option<&str>,
    columns: &[ColumnInfo],
) -> Vec<String> {
    let mut statements = Vec::new();
    if let Some(comment) = table_comment {
        statements.push(format!("COMMENT ON {keyword} {name} IS {};", lit(comment)));
    }
    for column in columns {
        if let Some(comment) = &column.comment {
            statements.push(format!(
                "COMMENT ON COLUMN {name}.{} IS {};",
                D.quote_ident(&column.name),
                lit(comment)
            ));
        }
    }
    statements
}

async fn table(session: &PgSession, table: &TableRef) -> Result<String> {
    let (oid, _, _) = introspect::relation_oid(session, table).await?;
    let structure = introspect::table_structure(session, table).await?;
    let name = D.table_name(table);

    let facts = session
        .query(&format!(
            "SELECT c.relkind, c.relpersistence, c.relispartition, \
                    pg_catalog.pg_get_expr(c.relpartbound, c.oid), \
                    CASE WHEN c.relkind = 'p' THEN pg_catalog.pg_get_partkeydef(c.oid) END, \
                    (SELECT pg_catalog.quote_ident(pn.nspname) || '.' || pg_catalog.quote_ident(p.relname) \
                     FROM pg_catalog.pg_inherits i \
                     JOIN pg_catalog.pg_class p ON p.oid = i.inhparent \
                     JOIN pg_catalog.pg_namespace pn ON pn.oid = p.relnamespace \
                     WHERE i.inhrelid = c.oid LIMIT 1) \
             FROM pg_catalog.pg_class c WHERE c.oid = {oid}"
        ))
        .await?;
    let facts = facts
        .first()
        .ok_or_else(|| not_found(&format!("table {name}")))?;
    let persistence = match facts.get(1) {
        Some("u") => "UNLOGGED ",
        Some("t") => "TEMPORARY ",
        _ => "",
    };
    let foreign = if facts.get(0) == Some("f") {
        "FOREIGN "
    } else {
        ""
    };

    let mut statements = Vec::new();
    if flag(facts, 2) {
        // A partition takes its columns from its parent.
        statements.push(format!(
            "CREATE {persistence}TABLE {name} PARTITION OF {} {};",
            opt(facts, 5).unwrap_or_default(),
            text(facts, 3),
        ));
    } else {
        let mut lines: Vec<String> = structure.columns.iter().map(column_line).collect();
        // Constraints in the order pg_dump uses: key, unique, exclusion,
        // check, then foreign keys.
        let constraint_rows = session
            .query(&format!(
                "SELECT c.conname, pg_catalog.pg_get_constraintdef(c.oid, true) \
                 FROM pg_catalog.pg_constraint c \
                 WHERE c.conrelid = {oid} AND c.contype IN ('p', 'u', 'x', 'c', 'f') AND c.conparentid = 0 \
                 ORDER BY pg_catalog.array_position(ARRAY['p', 'u', 'x', 'c', 'f']::\"char\"[], c.contype), c.conname"
            ))
            .await?;
        lines.extend(constraint_rows.iter().map(|row| {
            format!(
                "CONSTRAINT {} {}",
                D.quote_ident(&text(row, 0)),
                text(row, 1)
            )
        }));
        let mut create = format!(
            "CREATE {persistence}{foreign}TABLE {name} (\n    {}\n)",
            lines.join(",\n    ")
        );
        if let Some(partition_key) = opt(facts, 4) {
            create.push_str(&format!("\nPARTITION BY {partition_key}"));
        }
        create.push(';');
        statements.push(create);
    }

    // Indexes that back a constraint were created by it.
    let index_rows = session
        .query(&format!(
            "SELECT pg_catalog.pg_get_indexdef(i.indexrelid) \
             FROM pg_catalog.pg_index i \
             JOIN pg_catalog.pg_class ic ON ic.oid = i.indexrelid \
             WHERE i.indrelid = {oid} \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint c WHERE c.conindid = i.indexrelid) \
             ORDER BY ic.relname"
        ))
        .await?;
    statements.extend(index_rows.iter().map(|row| terminated(text(row, 0))));

    statements.extend(comments(
        if foreign.is_empty() {
            "TABLE"
        } else {
            "FOREIGN TABLE"
        },
        &name,
        structure.comment.as_deref(),
        &structure.columns,
    ));

    let trigger_rows = session
        .query(&format!(
            "SELECT pg_catalog.pg_get_triggerdef(t.oid, true) \
             FROM pg_catalog.pg_trigger t \
             WHERE t.tgrelid = {oid} AND NOT t.tgisinternal ORDER BY t.tgname"
        ))
        .await?;
    statements.extend(trigger_rows.iter().map(|row| terminated(text(row, 0))));

    Ok(statements.join("\n\n"))
}

async fn view(session: &PgSession, view: &TableRef, materialized: bool) -> Result<String> {
    let (oid, _, comment) = introspect::relation_oid(session, view).await?;
    let name = D.table_name(view);
    let rows = session
        .query(&format!(
            "SELECT pg_catalog.pg_get_viewdef({oid}, true), c.relispopulated \
             FROM pg_catalog.pg_class c WHERE c.oid = {oid}"
        ))
        .await?;
    let row = rows
        .first()
        .ok_or_else(|| not_found(&format!("view {name}")))?;
    let body = text(row, 0);
    let body = body.trim_end().trim_end_matches(';');

    let mut statements = Vec::new();
    if materialized {
        // There is no CREATE OR REPLACE for materialized views.
        let data = if flag(row, 1) {
            "WITH DATA"
        } else {
            "WITH NO DATA"
        };
        statements.push(format!(
            "CREATE MATERIALIZED VIEW {name} AS\n{body}\n{data};"
        ));
        let index_rows = session
            .query(&format!(
                "SELECT pg_catalog.pg_get_indexdef(i.indexrelid) \
                 FROM pg_catalog.pg_index i \
                 JOIN pg_catalog.pg_class ic ON ic.oid = i.indexrelid \
                 WHERE i.indrelid = {oid} ORDER BY ic.relname"
            ))
            .await?;
        statements.extend(index_rows.iter().map(|row| terminated(text(row, 0))));
    } else {
        statements.push(format!("CREATE OR REPLACE VIEW {name} AS\n{body};"));
    }

    let structure = introspect::table_structure(session, view).await?;
    statements.extend(comments(
        if materialized {
            "MATERIALIZED VIEW"
        } else {
            "VIEW"
        },
        &name,
        comment.as_deref(),
        &structure.columns,
    ));
    Ok(statements.join("\n\n"))
}

async fn routine(
    session: &PgSession,
    schema: &str,
    name: &str,
    kind: RoutineKind,
    identity_arguments: &str,
) -> Result<String> {
    if kind == RoutineKind::Aggregate {
        // pg_get_functiondef() refuses aggregates.
        return Err(Error::Unsupported(
            "printing the definition of an aggregate function".into(),
        ));
    }
    let sql = format!(
        "SELECT pg_catalog.pg_get_functiondef(p.oid) \
         FROM pg_catalog.pg_proc p \
         JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace \
         WHERE n.nspname = {} AND p.proname = {} \
           AND pg_catalog.pg_get_function_identity_arguments(p.oid) = {} AND p.prokind <> 'a'",
        lit(schema),
        lit(name),
        lit(identity_arguments),
    );
    single(
        session,
        &sql,
        &format!("routine \"{schema}.{name}({identity_arguments})\""),
    )
    .await
    .map(terminated)
}

async fn sequence(session: &PgSession, sequence: &TableRef) -> Result<String> {
    let sql = format!(
        "SELECT s.data_type::text, s.increment_by, s.min_value, s.max_value, s.start_value, s.cache_size, s.cycle \
         FROM pg_catalog.pg_sequences s WHERE s.schemaname = {} AND s.sequencename = {}",
        lit(&sequence.schema),
        lit(&sequence.name),
    );
    let rows = session.query(&sql).await?;
    let row = rows
        .first()
        .ok_or_else(|| not_found(&format!("sequence {}", D.table_name(sequence))))?;
    Ok(format!(
        "CREATE SEQUENCE {}\n    AS {}\n    INCREMENT BY {}\n    MINVALUE {}\n    MAXVALUE {}\n    START WITH {}\n    CACHE {}\n    {};",
        D.table_name(sequence),
        text(row, 0),
        text(row, 1),
        text(row, 2),
        text(row, 3),
        text(row, 4),
        text(row, 5),
        if flag(row, 6) { "CYCLE" } else { "NO CYCLE" },
    ))
}

async fn user_type(session: &PgSession, ty: &TableRef) -> Result<String> {
    let name = D.table_name(ty);
    let objects = introspect::objects(session, &ty.schema).await?;
    let info = objects
        .types
        .into_iter()
        .find(|candidate| candidate.name == ty.name)
        .ok_or_else(|| not_found(&format!("type {name}")))?;
    let mut statement = match &info.shape {
        TypeShape::Enum(labels) => format!(
            "CREATE TYPE {name} AS ENUM (\n    {}\n);",
            labels
                .iter()
                .map(|label| lit(label))
                .collect::<Vec<_>>()
                .join(",\n    ")
        ),
        TypeShape::Composite(attributes) => format!(
            "CREATE TYPE {name} AS (\n    {}\n);",
            attributes
                .iter()
                .map(|(attribute, ty)| format!("{} {ty}", D.quote_ident(attribute)))
                .collect::<Vec<_>>()
                .join(",\n    ")
        ),
        TypeShape::Domain { base, checks } => {
            let mut domain = format!("CREATE DOMAIN {name} AS {base}");
            for check in checks {
                domain.push_str(&format!("\n    {check}"));
            }
            domain.push(';');
            domain
        }
        TypeShape::Range(subtype) => {
            format!("CREATE TYPE {name} AS RANGE (\n    SUBTYPE = {subtype}\n);")
        }
        TypeShape::Other => {
            return Err(Error::Unsupported(
                "printing the definition of this type".into(),
            ));
        }
    };
    if let Some(comment) = &info.comment {
        let keyword = if matches!(info.shape, TypeShape::Domain { .. }) {
            "DOMAIN"
        } else {
            "TYPE"
        };
        statement.push_str(&format!(
            "\n\nCOMMENT ON {keyword} {name} IS {};",
            lit(comment)
        ));
    }
    Ok(statement)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kerno_core::TypeKind;

    fn column(name: &str, data_type: &str) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: data_type.into(),
            kind: TypeKind::Other,
            nullable: true,
            default: None,
            generation: None,
            primary_key_position: None,
            comment: None,
            collation: None,
            enum_values: Vec::new(),
        }
    }

    #[test]
    fn column_lines() {
        assert_eq!(column_line(&column("notes", "text")), "\"notes\" text");

        let mut c = column("price", "numeric(10,2)");
        c.nullable = false;
        c.default = Some("0".into());
        assert_eq!(
            column_line(&c),
            "\"price\" numeric(10,2) DEFAULT 0 NOT NULL"
        );

        let mut c = column("id", "bigint");
        c.nullable = false;
        c.generation = Some(ColumnGeneration::IdentityAlways);
        assert_eq!(
            column_line(&c),
            "\"id\" bigint GENERATED ALWAYS AS IDENTITY NOT NULL"
        );

        let mut c = column("id", "integer");
        c.nullable = false;
        c.generation = Some(ColumnGeneration::AutoIncrement);
        c.default = Some("nextval('t_id_seq'::regclass)".into());
        assert_eq!(column_line(&c), "\"id\" serial");

        // Nullable with an owned sequence: not what `serial` means.
        let mut c = column("id", "integer");
        c.generation = Some(ColumnGeneration::AutoIncrement);
        c.default = Some("nextval('t_id_seq'::regclass)".into());
        assert_eq!(
            column_line(&c),
            "\"id\" integer DEFAULT nextval('t_id_seq'::regclass)"
        );

        let mut c = column("total", "numeric");
        c.generation = Some(ColumnGeneration::Stored("(price * qty)".into()));
        assert_eq!(
            column_line(&c),
            "\"total\" numeric GENERATED ALWAYS AS ((price * qty)) STORED"
        );

        let mut c = column("name", "text");
        c.collation = Some("C".into());
        assert_eq!(column_line(&c), "\"name\" text COLLATE \"C\"");
    }

    #[test]
    fn statements_end_with_one_semicolon() {
        assert_eq!(terminated("select 1".into()), "select 1;");
        assert_eq!(terminated("select 1;\n".into()), "select 1;");
        assert_eq!(terminated("select 1;;".into()), "select 1;");
    }
}
