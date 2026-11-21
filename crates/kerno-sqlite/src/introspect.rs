//! What a SQLite file holds, from `sqlite_schema` and the pragma functions
//! (<https://www.sqlite.org/pragma.html>), in the engine-neutral types of
//! `kerno_core::schema`.

use std::collections::BTreeMap;

use kerno_core::schema::{
    ColumnGeneration, ColumnInfo, ConstraintInfo, ConstraintKind, DatabaseInfo, ForeignKeyInfo,
    IndexInfo, ObjectRef, RelationInfo, RelationKind, SchemaObjects, TableRef, TableStructure,
    TriggerInfo,
};
use kerno_core::sql::{Catalog, CatalogColumn, CatalogForeignKey, CatalogRelation, CatalogSchema};
use kerno_core::{Error, Result};
use rusqlite::{Connection, OptionalExtension, params};

use crate::connect::statement_error;
use crate::dialect::MAIN;
use crate::session::SqliteSession;
use crate::types::declared_kind;

/// `schema.sqlite_schema`, quoted.
fn catalog(schema: &str) -> String {
    format!("\"{}\".sqlite_schema", schema.replace('"', "\"\""))
}

/// Names SQLite keeps for itself (`sqlite_sequence`, `sqlite_stat1`, …).
const NOT_INTERNAL: &str = "name NOT LIKE 'sqlite\\_%' ESCAPE '\\'";

/// Runs `work` on the session's thread; a failure is told as SQLite's.
async fn read<T: Send + 'static>(
    session: &SqliteSession,
    what: &'static str,
    work: impl FnOnce(&Connection) -> rusqlite::Result<T> + Send + 'static,
) -> Result<T> {
    session
        .call(move |conn| work(conn))
        .await?
        .map_err(|error| statement_error(error, what))
}

pub(crate) async fn databases(session: &SqliteSession) -> Result<Vec<DatabaseInfo>> {
    let (encoding, pages, page_size) = read(session, "PRAGMA page_count", |conn| {
        let pragma = |name: &str| {
            conn.query_row(&format!("PRAGMA {name}"), [], |row| row.get::<_, String>(0))
        };
        Ok((
            pragma("encoding")?,
            conn.query_row("PRAGMA page_count", [], |row| row.get::<_, i64>(0))?,
            conn.query_row("PRAGMA page_size", [], |row| row.get::<_, i64>(0))?,
        ))
    })
    .await?;
    Ok(vec![DatabaseInfo {
        name: MAIN.into(),
        encoding: Some(encoding),
        size_bytes: u64::try_from(pages.saturating_mul(page_size)).ok(),
        ..DatabaseInfo::default()
    }])
}

/// `BEFORE`, `AFTER` or `INSTEAD OF`, and the event, of a `CREATE TRIGGER`.
fn trigger_shape(sql: &str) -> (String, String) {
    let upper = sql.to_ascii_uppercase();
    let words: Vec<&str> = upper.split_whitespace().collect();
    let timing = if let Some(at) = words.iter().position(|word| *word == "INSTEAD") {
        (at, "INSTEAD OF")
    } else if let Some(at) = words.iter().position(|word| *word == "BEFORE") {
        (at, "BEFORE")
    } else if let Some(at) = words.iter().position(|word| *word == "AFTER") {
        (at, "AFTER")
    } else {
        (0, "BEFORE")
    };
    let event = words[timing.0..]
        .iter()
        .find(|word| matches!(**word, "INSERT" | "UPDATE" | "DELETE"))
        .copied()
        .unwrap_or_default();
    (timing.1.to_string(), event.to_string())
}

fn trigger(name: String, table: String, sql: String) -> TriggerInfo {
    let (timing, events) = trigger_shape(&sql);
    TriggerInfo {
        name,
        table,
        timing,
        events,
        level: "ROW".into(),
        enabled: true,
        action: sql,
    }
}

pub(crate) async fn objects(session: &SqliteSession, schema: &str) -> Result<SchemaObjects> {
    let sql = format!(
        "SELECT type, name, tbl_name, sql FROM {} WHERE {NOT_INTERNAL} \
         AND type IN ('table', 'view', 'trigger') ORDER BY name",
        catalog(schema)
    );
    let schema = schema.to_string();
    read(
        session,
        "reading the objects of the database",
        move |conn| {
            let mut statement = conn.prepare(&sql)?;
            let entries = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut objects = SchemaObjects::default();
            for (kind, name, table, sql) in entries {
                match kind.as_str() {
                    "table" | "view" => {
                        let view = kind == "view";
                        // Exact, as SQLite keeps no estimate; a virtual table
                        // whose module is not here cannot be counted.
                        let rows = if view {
                            None
                        } else {
                            conn.query_row(
                                &format!(
                                    "SELECT count(*) FROM \"{}\".\"{}\"",
                                    schema.replace('"', "\"\""),
                                    name.replace('"', "\"\"")
                                ),
                                [],
                                |row| row.get::<_, i64>(0),
                            )
                            .ok()
                            .and_then(|count| u64::try_from(count).ok())
                        };
                        objects.relations.push(RelationInfo {
                            name,
                            kind: if view {
                                RelationKind::View
                            } else {
                                RelationKind::Table
                            },
                            row_estimate: rows,
                            size_bytes: None,
                            comment: None,
                            engine: None,
                            is_partition: false,
                        });
                    }
                    _ => objects
                        .triggers
                        .push(trigger(name, table, sql.unwrap_or_default())),
                }
            }
            Ok(objects)
        },
    )
    .await
}

/// A foreign key's rows, grouped: `(id, referenced table, from, to,
/// on update, on delete)` in key order.
type KeyRow = (i64, String, String, Option<String>, String, String);

/// The primary key columns of `table`, in key order.
fn primary_key(conn: &Connection, schema: &str, table: &str) -> rusqlite::Result<Vec<String>> {
    let mut statement =
        conn.prepare("SELECT name FROM pragma_table_info(?1, ?2) WHERE pk > 0 ORDER BY pk")?;
    statement
        .query_map(params![table, schema], |row| row.get(0))?
        .collect()
}

/// Turns grouped key rows into foreign keys of `table`; a key that names
/// no referenced column refers to the other table's primary key.
fn foreign_keys(
    conn: &Connection,
    schema: &str,
    table: &str,
    rows: Vec<KeyRow>,
) -> rusqlite::Result<Vec<ForeignKeyInfo>> {
    let mut keys: Vec<ForeignKeyInfo> = Vec::new();
    let mut ids: Vec<i64> = Vec::new();
    for (id, referenced, from, to, on_update, on_delete) in rows {
        if ids.last() != Some(&id) {
            ids.push(id);
            keys.push(ForeignKeyInfo {
                // SQLite's foreign keys are often unnamed: named by position.
                name: format!("{table}_fk{id}"),
                table: TableRef::new(schema, table),
                columns: Vec::new(),
                referenced_table: TableRef::new(schema, referenced),
                referenced_columns: Vec::new(),
                on_update,
                on_delete,
            });
        }
        let key = keys.last_mut().expect("pushed above");
        key.columns.push(from);
        if let Some(to) = to {
            key.referenced_columns.push(to);
        }
    }
    for key in &mut keys {
        if key.referenced_columns.is_empty() {
            key.referenced_columns = primary_key(conn, schema, &key.referenced_table.name)?;
        }
    }
    Ok(keys)
}

/// The text after a partial index's `WHERE`.
fn predicate(sql: &str) -> Option<String> {
    let upper = sql.to_ascii_uppercase();
    upper
        .rfind(" WHERE ")
        .map(|at| sql[at + " WHERE ".len()..].trim().to_string())
}

pub(crate) async fn table_structure(
    session: &SqliteSession,
    table: &TableRef,
) -> Result<TableStructure> {
    let (schema, name) = (table.schema.clone(), table.name.clone());
    let schema_table = catalog(&schema);
    read(session, "reading the structure of the table", move |conn| {
        let without_rowid: bool = conn
            .query_row(
                "SELECT wr FROM pragma_table_list(?1) WHERE schema = ?2",
                params![name, schema],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .is_some_and(|wr| wr != 0);
        let mut statement = conn.prepare(
            "SELECT name, type, \"notnull\", dflt_value, pk, hidden \
             FROM pragma_table_xinfo(?1, ?2) ORDER BY cid",
        )?;
        let raw = statement
            .query_map(params![name, schema], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)? != 0,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if raw.is_empty() {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        let key_columns = raw.iter().filter(|column| column.4 > 0).count();
        let columns: Vec<ColumnInfo> = raw
            .into_iter()
            .map(|(column, data_type, not_null, default, pk, hidden)| {
                // The row id's alias: numbered by SQLite, never NULL.
                let rowid = pk == 1
                    && key_columns == 1
                    && !without_rowid
                    && data_type.trim().eq_ignore_ascii_case("INTEGER");
                ColumnInfo {
                    kind: declared_kind(&data_type),
                    name: column,
                    data_type,
                    nullable: !not_null && !rowid,
                    default,
                    generation: match hidden {
                        2 => Some(ColumnGeneration::Virtual(String::new())),
                        3 => Some(ColumnGeneration::Stored(String::new())),
                        _ if rowid => Some(ColumnGeneration::AutoIncrement),
                        _ => None,
                    },
                    on_update: None,
                    invisible: hidden == 1,
                    primary_key_position: u32::try_from(pk).ok().filter(|pk| *pk > 0),
                    comment: None,
                    collation: None,
                    enum_values: Vec::new(),
                }
            })
            .collect();

        let mut statement = conn.prepare(
            "SELECT name, \"unique\", origin, partial FROM pragma_index_list(?1, ?2) ORDER BY seq",
        )?;
        let listed = statement
            .query_map(params![name, schema], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)? != 0,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)? != 0,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut indexes = Vec::new();
        let mut constraints = Vec::new();
        for (index, unique, origin, partial) in listed {
            let mut statement = conn.prepare(
                "SELECT coalesce(name, '<expression>') FROM pragma_index_info(?1, ?2) ORDER BY seqno",
            )?;
            let indexed: Vec<String> = statement
                .query_map(params![index, schema], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            let definition: Option<String> = conn
                .query_row(
                    &format!("SELECT sql FROM {schema_table} WHERE type = 'index' AND name = ?1"),
                    params![index],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            if origin == "u" {
                let quoted: Vec<String> = indexed.iter().map(|c| format!("\"{c}\"")).collect();
                constraints.push(ConstraintInfo {
                    name: index.clone(),
                    kind: ConstraintKind::Unique,
                    columns: indexed.clone(),
                    definition: format!("UNIQUE ({})", quoted.join(", ")),
                });
            }
            indexes.push(IndexInfo {
                predicate: definition.as_deref().filter(|_| partial).and_then(predicate),
                name: index,
                columns: indexed,
                unique,
                primary: origin == "pk",
                backs_constraint: origin != "c",
                method: "btree".into(),
                definition,
            });
        }
        let mut key: Vec<(u32, String)> = columns
            .iter()
            .filter_map(|c| Some((c.primary_key_position?, c.name.clone())))
            .collect();
        key.sort();
        if !key.is_empty() {
            let names: Vec<String> = key.into_iter().map(|(_, name)| name).collect();
            let quoted: Vec<String> = names.iter().map(|c| format!("\"{c}\"")).collect();
            constraints.insert(
                0,
                ConstraintInfo {
                    name: "PRIMARY KEY".into(),
                    kind: ConstraintKind::PrimaryKey,
                    definition: format!("PRIMARY KEY ({})", quoted.join(", ")),
                    columns: names,
                },
            );
        }

        let mut statement = conn.prepare(
            "SELECT id, \"table\", \"from\", \"to\", on_update, on_delete \
             FROM pragma_foreign_key_list(?1, ?2) ORDER BY id, seq",
        )?;
        let rows = statement
            .query_map(params![name, schema], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<KeyRow>>>()?;
        let own_keys = foreign_keys(conn, &schema, &name, rows)?;

        let mut statement = conn.prepare(&format!(
            "SELECT m.name, f.id, f.\"table\", f.\"from\", f.\"to\", f.on_update, f.on_delete \
             FROM {schema_table} m JOIN pragma_foreign_key_list(m.name, ?2) f \
             WHERE m.type = 'table' AND f.\"table\" = ?1 COLLATE NOCASE \
             ORDER BY m.name, f.id, f.seq"
        ))?;
        let mut by_table: BTreeMap<String, Vec<KeyRow>> = BTreeMap::new();
        for row in statement.query_map(params![name, schema], |row| {
            Ok((
                row.get::<_, String>(0)?,
                (
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ),
            ))
        })? {
            let (referencing, key) = row?;
            by_table.entry(referencing).or_default().push(key);
        }
        let mut referenced_by = Vec::new();
        for (referencing, rows) in by_table {
            referenced_by.extend(foreign_keys(conn, &schema, &referencing, rows)?);
        }

        let mut statement = conn.prepare(&format!(
            "SELECT name, tbl_name, sql FROM {schema_table} \
             WHERE type = 'trigger' AND tbl_name = ?1 COLLATE NOCASE ORDER BY name"
        ))?;
        let triggers = statement
            .query_map(params![name], |row| {
                Ok(trigger(
                    row.get(0)?,
                    row.get(1)?,
                    row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(TableStructure {
            columns,
            indexes,
            constraints,
            foreign_keys: own_keys,
            referenced_by,
            triggers,
            comment: None,
        })
    })
    .await
    .map_err(|error| match error {
        Error::Db(db) if db.message.contains("Query returned no rows") => Error::Config(format!(
            "there is no table or view “{}” in this database",
            table.name
        )),
        other => other,
    })
}

/// The statements that recreate `object`, from what SQLite kept of them.
pub(crate) async fn ddl(session: &SqliteSession, object: &ObjectRef) -> Result<String> {
    let (schema, sql) = match object {
        ObjectRef::Relation(table, _) => (
            table.schema.clone(),
            // The table or view, then its indexes and triggers.
            "SELECT sql FROM {catalog} WHERE sql IS NOT NULL AND (name = ?1 COLLATE NOCASE \
             OR (tbl_name = ?1 COLLATE NOCASE AND type IN ('index', 'trigger'))) \
             ORDER BY CASE type WHEN 'table' THEN 0 WHEN 'view' THEN 0 WHEN 'index' THEN 1 \
             ELSE 2 END, name",
        ),
        ObjectRef::Index { table, .. } | ObjectRef::Trigger { table, .. } => (
            table.schema.clone(),
            "SELECT sql FROM {catalog} WHERE sql IS NOT NULL AND name = ?1 COLLATE NOCASE",
        ),
        ObjectRef::Database(name) => (
            name.clone(),
            "SELECT sql FROM {catalog} WHERE sql IS NOT NULL AND ?1 IS NOT NULL \
             ORDER BY CASE type WHEN 'table' THEN 0 WHEN 'index' THEN 1 WHEN 'view' THEN 2 \
             ELSE 3 END, rowid",
        ),
        _ => {
            return Err(Error::Unsupported(
                "SQLite has no such kind of object".into(),
            ));
        }
    };
    let sql = sql.replace("{catalog}", &catalog(&schema));
    let name = object.name().to_string();
    let statements: Vec<String> = read(session, "reading the definition", move |conn| {
        let mut statement = conn.prepare(&sql)?;
        statement
            .query_map(params![name], |row| row.get(0))?
            .collect()
    })
    .await?;
    if statements.is_empty() {
        return Err(Error::Config(match object {
            ObjectRef::Index { name, .. } => format!(
                "index “{name}” was made by SQLite for a key of its table: it has no statement of \
                 its own (see the table's)"
            ),
            other => format!("there is no “{}” in this database", other.name()),
        }));
    }
    Ok(statements
        .iter()
        .map(|statement| format!("{};\n", statement.trim_end_matches(';')))
        .collect::<Vec<_>>()
        .join("\n"))
}

pub(crate) async fn completion_catalog(session: &SqliteSession) -> Result<Catalog> {
    let schema_table = catalog(MAIN);
    let relations = read(session, "reading names for completion", move |conn| {
        let mut statement = conn.prepare(&format!(
            "SELECT m.name, m.type, c.name, c.type, c.pk FROM {schema_table} m \
             JOIN pragma_table_info(m.name) c \
             WHERE m.type IN ('table', 'view') AND m.{NOT_INTERNAL} ORDER BY m.name, c.cid"
        ))?;
        let mut relations: Vec<(CatalogRelation, Vec<String>)> = Vec::new();
        for row in statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })? {
            let (relation, kind, column, data_type, pk) = row?;
            if relations
                .last()
                .is_none_or(|(last, _)| last.name != relation)
            {
                relations.push((
                    CatalogRelation {
                        name: relation,
                        kind: if kind == "view" {
                            RelationKind::View
                        } else {
                            RelationKind::Table
                        },
                        columns: Vec::new(),
                        foreign_keys: Vec::new(),
                    },
                    Vec::new(),
                ));
            }
            let (last, key) = relations.last_mut().expect("pushed above");
            if pk > 0 {
                key.push(column.clone());
            }
            last.columns.push(CatalogColumn {
                name: column,
                data_type,
            });
        }
        let mut statement = conn.prepare(&format!(
            "SELECT m.name, f.id, f.\"table\", f.\"from\", f.\"to\" FROM {schema_table} m \
             JOIN pragma_foreign_key_list(m.name) f WHERE m.type = 'table' \
             ORDER BY m.name, f.id, f.seq"
        ))?;
        let keys = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let primary: BTreeMap<String, Vec<String>> = relations
            .iter()
            .map(|(relation, key)| (relation.name.to_lowercase(), key.clone()))
            .collect();
        let mut last: Option<(String, i64)> = None;
        for (table, id, referenced, from, to) in keys {
            let Some((relation, _)) = relations.iter_mut().find(|(r, _)| r.name == table) else {
                continue;
            };
            if last.as_ref() != Some(&(table.clone(), id)) {
                relation.foreign_keys.push(CatalogForeignKey {
                    columns: Vec::new(),
                    referenced_schema: MAIN.into(),
                    referenced_table: referenced.clone(),
                    referenced_columns: Vec::new(),
                });
                last = Some((table, id));
            }
            let key = relation.foreign_keys.last_mut().expect("pushed above");
            key.columns.push(from);
            match to {
                Some(to) => key.referenced_columns.push(to),
                None => {
                    // The other table's primary key, column by column.
                    let position = key.columns.len() - 1;
                    if let Some(column) = primary
                        .get(&referenced.to_lowercase())
                        .and_then(|columns| columns.get(position))
                    {
                        key.referenced_columns.push(column.clone());
                    }
                }
            }
        }
        Ok(relations
            .into_iter()
            .map(|(relation, _)| relation)
            .collect())
    })
    .await?;
    Ok(Catalog {
        schemas: vec![CatalogSchema {
            name: MAIN.into(),
            relations,
            routines: Vec::new(),
        }],
        search_path: vec![MAIN.into()],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triggers_are_read_from_their_statement() {
        assert_eq!(
            trigger_shape("CREATE TRIGGER t_after AFTER UPDATE OF a ON t BEGIN SELECT 1; END"),
            ("AFTER".into(), "UPDATE".into())
        );
        assert_eq!(
            trigger_shape("create trigger v_ins instead of insert on v begin select 1; end"),
            ("INSTEAD OF".into(), "INSERT".into())
        );
        assert_eq!(
            trigger_shape("CREATE TRIGGER x BEFORE DELETE ON t BEGIN SELECT 1; END"),
            ("BEFORE".into(), "DELETE".into())
        );
    }

    #[test]
    fn a_partial_index_keeps_its_condition() {
        assert_eq!(
            predicate("CREATE INDEX i ON t (a) WHERE a > 0").as_deref(),
            Some("a > 0")
        );
        assert_eq!(predicate("CREATE INDEX i ON t (a)"), None);
    }
}
