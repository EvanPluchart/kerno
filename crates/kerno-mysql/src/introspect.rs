//! Reading the catalog: databases, the objects of a database and the
//! structure of a table.
//!
//! Everything comes from `information_schema`, which both engines have,
//! with the database always named explicitly (never `DATABASE()`), so a
//! session without a default database sees everything too. Where MySQL and
//! MariaDB print the same fact differently (defaults, generation and check
//! expressions) the text is brought to one form, so the same table reads
//! the same on both. Lists are sorted here, by byte order: the two engines
//! collate catalog names differently.

use kerno_core::dialect::Dialect;
use kerno_core::schema::{
    ColumnGeneration, ColumnInfo, ConstraintInfo, ConstraintKind, DatabaseInfo, EventInfo,
    ForeignKeyInfo, IndexInfo, RelationInfo, RelationKind, RoutineInfo, RoutineKind, SchemaObjects,
    SequenceInfo, TableRef, TableStructure, TriggerInfo,
};
use kerno_core::sql::params::is_number;
use kerno_core::{DbError, Engine, Error, Result, Session, TypeKind};

use crate::session::MysqlSession;
use crate::types;

/// The databases every server has, as a list for `IN (...)`.
pub(crate) const SYSTEM_DATABASES: &str =
    "'information_schema', 'performance_schema', 'mysql', 'sys'";

// Row helpers: metadata queries return text.

type Row = Vec<Option<String>>;

pub(crate) fn text(row: &Row, index: usize) -> String {
    row.get(index).cloned().flatten().unwrap_or_default()
}

/// A value that is neither NULL nor empty.
pub(crate) fn opt(row: &Row, index: usize) -> Option<String> {
    row.get(index)
        .cloned()
        .flatten()
        .filter(|value| !value.is_empty())
}

fn number(row: &Row, index: usize) -> Option<u64> {
    row.get(index)?.as_deref()?.parse().ok()
}

pub(crate) fn lit(session: &MysqlSession, value: &str) -> String {
    session.dialect().quote_string(value)
}

pub(crate) fn not_found(what: &str) -> Error {
    Error::Db(Box::new(DbError::message(format!("{what} does not exist"))))
}

fn is_mariadb(session: &MysqlSession) -> bool {
    session.engine() == Engine::MariaDb
}

pub(crate) async fn databases(session: &MysqlSession) -> Result<Vec<DatabaseInfo>> {
    // MySQL has no comment on databases; MariaDB has since 10.5.
    let comment = if is_mariadb(session) && session.server().version_number >= (10, 5, 0) {
        "s.SCHEMA_COMMENT"
    } else {
        "NULL"
    };
    // ponytail: sizes of every database in one pass over
    // information_schema.TABLES, which MariaDB answers by opening every
    // table. Make it lazy per database if a server with tens of thousands
    // of tables shows up.
    let rows = session
        .query(&format!(
            "SELECT s.SCHEMA_NAME, s.DEFAULT_CHARACTER_SET_NAME, s.DEFAULT_COLLATION_NAME, \
                    COALESCE(z.bytes, 0), {comment} \
             FROM information_schema.SCHEMATA s \
             LEFT JOIN (SELECT t.TABLE_SCHEMA AS name, SUM(t.DATA_LENGTH + t.INDEX_LENGTH) AS bytes \
                        FROM information_schema.TABLES t GROUP BY t.TABLE_SCHEMA) z \
                    ON z.name = s.SCHEMA_NAME"
        ))
        .await?;
    let mut databases: Vec<DatabaseInfo> = rows
        .iter()
        .map(|row| {
            let name = text(row, 0);
            DatabaseInfo {
                is_system: is_system_database(&name),
                name,
                owner: None,
                encoding: opt(row, 1),
                collation: opt(row, 2),
                size_bytes: number(row, 3),
                comment: opt(row, 4),
            }
        })
        .collect();
    databases.sort_by(|a, b| (a.is_system, &a.name).cmp(&(b.is_system, &b.name)));
    Ok(databases)
}

fn is_system_database(name: &str) -> bool {
    ["information_schema", "performance_schema", "mysql", "sys"]
        .iter()
        .any(|system| name.eq_ignore_ascii_case(system))
}

pub(crate) async fn objects(session: &MysqlSession, database: &str) -> Result<SchemaObjects> {
    let (relations, sequence_names) = relations(session, database).await?;
    let mut sequences = Vec::with_capacity(sequence_names.len());
    for name in sequence_names {
        sequences.push(sequence(session, &TableRef::new(database, name)).await?);
    }
    Ok(SchemaObjects {
        relations,
        routines: routines(session, database).await?,
        sequences,
        types: Vec::new(),
        triggers: triggers(
            session,
            &format!("t.TRIGGER_SCHEMA = {}", lit(session, database)),
        )
        .await?,
        events: events(session, database).await?,
    })
}

/// The tables and views of a database, and the names of its sequences
/// (MariaDB lists a sequence as a table of type `SEQUENCE`).
async fn relations(
    session: &MysqlSession,
    database: &str,
) -> Result<(Vec<RelationInfo>, Vec<String>)> {
    let rows = session
        .query(&format!(
            "SELECT t.TABLE_NAME, t.TABLE_TYPE, t.ENGINE, t.TABLE_ROWS, \
                    t.DATA_LENGTH + t.INDEX_LENGTH, t.TABLE_COMMENT, t.CREATE_OPTIONS \
             FROM information_schema.TABLES t WHERE t.TABLE_SCHEMA = {}",
            lit(session, database)
        ))
        .await?;
    let mut relations = Vec::new();
    let mut sequences = Vec::new();
    for row in &rows {
        let table_type = text(row, 1);
        if table_type == "SEQUENCE" {
            sequences.push(text(row, 0));
            continue;
        }
        // "VIEW" and "SYSTEM VIEW"; everything else holds rows of its own
        // ("BASE TABLE", MariaDB's "SYSTEM VERSIONED").
        let is_view = table_type.contains("VIEW");
        relations.push(RelationInfo {
            name: text(row, 0),
            kind: if is_view {
                RelationKind::View
            } else if text(row, 6).contains("partitioned") {
                RelationKind::PartitionedTable
            } else {
                RelationKind::Table
            },
            row_estimate: number(row, 3),
            size_bytes: number(row, 4),
            // A view's comment column just says "VIEW".
            comment: opt(row, 5).filter(|_| !is_view),
            engine: opt(row, 2),
            // Partitions are not tables of their own here.
            is_partition: false,
        });
    }
    relations.sort_by(|a, b| a.name.cmp(&b.name));
    sequences.sort();
    Ok((relations, sequences))
}

async fn routines(session: &MysqlSession, database: &str) -> Result<Vec<RoutineInfo>> {
    let database = lit(session, database);
    // Position 0 is a function's return value, not an argument.
    let parameters = session
        .query(&format!(
            "SELECT p.ROUTINE_TYPE, p.SPECIFIC_NAME, p.PARAMETER_MODE, p.PARAMETER_NAME, p.DTD_IDENTIFIER \
             FROM information_schema.PARAMETERS p \
             WHERE p.SPECIFIC_SCHEMA = {database} AND p.ORDINAL_POSITION > 0 \
             ORDER BY p.ROUTINE_TYPE, p.SPECIFIC_NAME, p.ORDINAL_POSITION"
        ))
        .await?;
    let rows = session
        .query(&format!(
            "SELECT r.ROUTINE_NAME, r.ROUTINE_TYPE, r.DTD_IDENTIFIER, r.ROUTINE_BODY, r.ROUTINE_COMMENT \
             FROM information_schema.ROUTINES r \
             WHERE r.ROUTINE_SCHEMA = {database} AND r.ROUTINE_TYPE IN ('PROCEDURE', 'FUNCTION')"
        ))
        .await?;
    let mut routines: Vec<RoutineInfo> = rows
        .iter()
        .map(|row| {
            let name = text(row, 0);
            let routine_type = text(row, 1);
            // A function and a procedure may share a name.
            let arguments: Vec<String> = parameters
                .iter()
                .filter(|p| text(p, 0) == routine_type && text(p, 1) == name)
                .map(|p| {
                    // Mode, name and type, as the server prints them.
                    [opt(p, 2), opt(p, 3), opt(p, 4)]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .collect();
            let is_function = routine_type == "FUNCTION";
            RoutineInfo {
                name,
                kind: if is_function {
                    RoutineKind::Function
                } else {
                    RoutineKind::Procedure
                },
                arguments: arguments.join(", "),
                returns: opt(row, 2).filter(|_| is_function),
                language: opt(row, 3),
                // No overloading: the name is enough.
                identity_arguments: String::new(),
                comment: opt(row, 4),
            }
        })
        .collect();
    routines.sort_by(|a, b| (&a.name, a.kind as u8).cmp(&(&b.name, b.kind as u8)));
    Ok(routines)
}

/// A MariaDB sequence, read from the sequence itself: selecting from it
/// returns one row describing it.
async fn sequence(session: &MysqlSession, sequence: &TableRef) -> Result<SequenceInfo> {
    let rows = session
        .query(&format!(
            "SELECT start_value, increment, minimum_value, maximum_value, cycle_option FROM {}",
            session.dialect().table_name(sequence)
        ))
        .await?;
    let row = rows.first().cloned().unwrap_or_default();
    Ok(SequenceInfo {
        name: sequence.name.clone(),
        data_type: None,
        start: opt(&row, 0),
        increment: opt(&row, 1),
        min: opt(&row, 2),
        max: opt(&row, 3),
        // The sequence only shows the next value it has not cached, and
        // "previous value" is per session.
        last_value: None,
        cycles: row.get(4).cloned().flatten().as_deref() == Some("1"),
        owned_by: None,
    })
}

/// Triggers matching `filter`, a condition over `t`
/// (`information_schema.TRIGGERS`).
async fn triggers(session: &MysqlSession, filter: &str) -> Result<Vec<TriggerInfo>> {
    let rows = session
        .query(&format!(
            "SELECT t.TRIGGER_NAME, t.EVENT_OBJECT_TABLE, t.ACTION_TIMING, t.EVENT_MANIPULATION, \
                    t.ACTION_ORIENTATION, t.ACTION_STATEMENT \
             FROM information_schema.TRIGGERS t WHERE {filter}"
        ))
        .await?;
    let mut triggers: Vec<TriggerInfo> = rows
        .iter()
        .map(|row| TriggerInfo {
            name: text(row, 0),
            table: text(row, 1),
            timing: text(row, 2),
            events: text(row, 3),
            level: text(row, 4),
            // Triggers cannot be disabled on either engine.
            enabled: true,
            action: text(row, 5),
        })
        .collect();
    triggers.sort_by(|a, b| (&a.table, &a.name).cmp(&(&b.table, &b.name)));
    Ok(triggers)
}

async fn events(session: &MysqlSession, database: &str) -> Result<Vec<EventInfo>> {
    let rows = session
        .query(&format!(
            "SELECT e.EVENT_NAME, e.STATUS, e.EVENT_TYPE, e.EXECUTE_AT, e.INTERVAL_VALUE, \
                    e.INTERVAL_FIELD, e.STARTS, e.ENDS, e.EVENT_COMMENT \
             FROM information_schema.EVENTS e WHERE e.EVENT_SCHEMA = {}",
            lit(session, database)
        ))
        .await?;
    let mut events: Vec<EventInfo> = rows
        .iter()
        .map(|row| EventInfo {
            name: text(row, 0),
            status: text(row, 1),
            schedule: event_schedule(
                &text(row, 2),
                opt(row, 3).as_deref(),
                &text(row, 4),
                &text(row, 5),
                opt(row, 6).as_deref(),
                opt(row, 7).as_deref(),
            ),
            comment: opt(row, 8),
        })
        .collect();
    events.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(events)
}

/// An event's schedule in the words of `CREATE EVENT`.
fn event_schedule(
    event_type: &str,
    execute_at: Option<&str>,
    interval_value: &str,
    interval_field: &str,
    starts: Option<&str>,
    ends: Option<&str>,
) -> String {
    if event_type == "ONE TIME" {
        return format!("AT {}", execute_at.unwrap_or_default());
    }
    let mut schedule = format!("EVERY {interval_value} {interval_field}");
    if let Some(starts) = starts {
        schedule.push_str(&format!(" STARTS {starts}"));
    }
    if let Some(ends) = ends {
        schedule.push_str(&format!(" ENDS {ends}"));
    }
    schedule
}

pub(crate) async fn table_structure(
    session: &MysqlSession,
    table: &TableRef,
) -> Result<TableStructure> {
    let schema = lit(session, &table.schema);
    let name = lit(session, &table.name);
    let facts = session
        .query(&format!(
            "SELECT t.TABLE_TYPE, t.TABLE_COMMENT, t.TABLE_COLLATION \
             FROM information_schema.TABLES t WHERE t.TABLE_SCHEMA = {schema} AND t.TABLE_NAME = {name}"
        ))
        .await?;
    let facts = facts
        .first()
        .ok_or_else(|| not_found(&format!("table {}", session.dialect().table_name(table))))?;
    let is_view = text(facts, 0).contains("VIEW");
    let table_collation = opt(facts, 2);

    let indexes = indexes(session, table).await?;
    let mut columns = columns(session, table, table_collation.as_deref()).await?;
    if let Some(primary) = indexes.iter().find(|index| index.primary) {
        for column in &mut columns {
            column.primary_key_position = primary
                .columns
                .iter()
                .position(|name| name == &column.name)
                .map(|position| position as u32 + 1);
        }
    }
    let mut constraints = key_constraints(session, &indexes);
    constraints.extend(check_constraints(session, table).await?);

    Ok(TableStructure {
        columns,
        constraints,
        indexes,
        foreign_keys: foreign_keys(
            session,
            &format!("k.TABLE_SCHEMA = {schema} AND k.TABLE_NAME = {name}"),
        )
        .await?,
        referenced_by: foreign_keys(
            session,
            &format!("k.REFERENCED_TABLE_SCHEMA = {schema} AND k.REFERENCED_TABLE_NAME = {name}"),
        )
        .await?,
        triggers: triggers(
            session,
            &format!("t.EVENT_OBJECT_SCHEMA = {schema} AND t.EVENT_OBJECT_TABLE = {name}"),
        )
        .await?,
        comment: opt(facts, 1).filter(|_| !is_view),
    })
}

async fn columns(
    session: &MysqlSession,
    table: &TableRef,
    table_collation: Option<&str>,
) -> Result<Vec<ColumnInfo>> {
    let mariadb = is_mariadb(session);
    let version = session.server().version_number;
    let generation = if (mariadb && version >= (10, 2, 5)) || (!mariadb && version >= (5, 7, 0)) {
        "c.GENERATION_EXPRESSION"
    } else {
        "NULL"
    };
    // MariaDB's JSON is LONGTEXT with a check the server adds itself
    // (10.4.3), named after the column.
    let json = if mariadb && version >= (10, 4, 3) {
        "EXISTS (SELECT 1 FROM information_schema.CHECK_CONSTRAINTS k \
                 WHERE k.CONSTRAINT_SCHEMA = c.TABLE_SCHEMA AND k.TABLE_NAME = c.TABLE_NAME \
                   AND k.CONSTRAINT_NAME = c.COLUMN_NAME \
                   AND k.CHECK_CLAUSE = CONCAT('json_valid(`', c.COLUMN_NAME, '`)'))"
    } else {
        "0"
    };
    let rows = session
        .query(&format!(
            "SELECT c.COLUMN_NAME, c.COLUMN_TYPE, c.DATA_TYPE, c.IS_NULLABLE, c.COLUMN_DEFAULT, \
                    c.EXTRA, {generation}, c.COLUMN_COMMENT, c.COLLATION_NAME, {json} \
             FROM information_schema.COLUMNS c \
             WHERE c.TABLE_SCHEMA = {} AND c.TABLE_NAME = {} \
             ORDER BY c.ORDINAL_POSITION",
            lit(session, &table.schema),
            lit(session, &table.name),
        ))
        .await?;
    Ok(rows
        .iter()
        .map(|row| {
            let column_type = text(row, 1);
            let data_type = text(row, 2);
            let extra = text(row, 5).to_ascii_uppercase();
            let expression = opt(row, 6).map(|expression| {
                let expression = if mariadb {
                    expression
                } else {
                    unescape(&expression)
                };
                strip_outer_parens(&expression).to_string()
            });
            let generation = match expression {
                // MariaDB once called stored columns persistent.
                Some(expression) if extra.contains("STORED") || extra.contains("PERSISTENT") => {
                    Some(ColumnGeneration::Stored(expression))
                }
                Some(expression) if extra.contains("VIRTUAL") => {
                    Some(ColumnGeneration::Virtual(expression))
                }
                _ if extra.contains("AUTO_INCREMENT") => Some(ColumnGeneration::AutoIncrement),
                _ => None,
            };
            let generated = matches!(
                generation,
                Some(ColumnGeneration::Stored(_) | ColumnGeneration::Virtual(_))
            );
            ColumnInfo {
                name: text(row, 0),
                kind: if row.get(9).cloned().flatten().as_deref() == Some("1") {
                    TypeKind::Json
                } else {
                    types::kind_of_catalog(&data_type, &column_type)
                },
                nullable: text(row, 3) == "YES",
                // A generated column has no default.
                default: if generated {
                    None
                } else {
                    normalize_default(
                        mariadb,
                        row.get(4).cloned().flatten().as_deref(),
                        &data_type,
                        &extra,
                        session.dialect(),
                    )
                },
                generation,
                // Filled in from the primary key's index.
                primary_key_position: None,
                comment: opt(row, 7),
                collation: opt(row, 8)
                    .filter(|collation| table_collation.is_some_and(|table| table != collation)),
                enum_values: types::enum_labels(&column_type),
                data_type: column_type,
            }
        })
        .collect())
}

/// The text of a single-quoted string literal as MariaDB prints defaults:
/// quotes doubled, backslash escapes. `None` when `sql` is not exactly one
/// such literal.
fn string_literal_value(sql: &str) -> Option<String> {
    let inner = sql.strip_prefix('\'')?.strip_suffix('\'')?;
    let mut text = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            // A lone quote would end the literal: this is not a single one.
            '\'' => match chars.next() {
                Some('\'') => text.push('\''),
                _ => return None,
            },
            '\\' => text.push(match chars.next()? {
                '0' => '\0',
                'b' => '\u{8}',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                'Z' => '\u{1a}',
                other => other,
            }),
            other => text.push(other),
        }
    }
    Some(text)
}

/// A column default as an SQL expression, the same on both engines.
///
/// MariaDB already prints SQL: strings quoted, expressions bare, and the
/// word `NULL` for a nullable column without a default. MySQL prints the
/// raw value of a literal, unquoted, and marks expression defaults with
/// `DEFAULT_GENERATED` in `EXTRA` (upper-cased by the caller).
fn normalize_default(
    mariadb: bool,
    raw: Option<&str>,
    data_type: &str,
    extra: &str,
    dialect: &dyn Dialect,
) -> Option<String> {
    let raw = raw?;
    let is_now = |text: &str| text.to_ascii_lowercase().starts_with("current_timestamp");
    // MariaDB spells it current_timestamp(); both accept the keyword.
    let now = |text: &str| {
        let precision = text
            .split_once('(')
            .map(|(_, rest)| rest.trim_end_matches(')').trim())
            .filter(|precision| !precision.is_empty());
        match precision {
            Some(precision) => format!("CURRENT_TIMESTAMP({precision})"),
            None => "CURRENT_TIMESTAMP".to_string(),
        }
    };
    let lower = raw.to_ascii_lowercase();
    if mariadb {
        let literal = raw.starts_with('\'')
            || is_number(raw)
            || lower.starts_with("b'")
            || lower.starts_with("x'");
        return if raw.eq_ignore_ascii_case("NULL") {
            None
        } else if is_now(raw) {
            Some(now(raw))
        } else if let Some(text) = string_literal_value(raw) {
            // Re-written by the dialect, so a string default has one
            // spelling on both engines.
            Some(dialect.quote_string(&text))
        } else if literal {
            Some(raw.to_string())
        } else {
            Some(format!("({raw})"))
        };
    }
    // MySQL prints a bit default as b'101' and a binary one as 0x0102; in
    // any other column the same characters are just a string's text.
    let kind = types::kind_of_catalog(data_type, "");
    let literal = match kind {
        TypeKind::Bit => lower.starts_with("b'"),
        TypeKind::Binary => lower.starts_with("0x"),
        kind => kind.is_numeric() && is_number(raw),
    };
    Some(
        if matches!(kind, TypeKind::Date | TypeKind::DateTime) && is_now(raw) {
            now(raw)
        } else if extra.contains("DEFAULT_GENERATED") {
            format!("({raw})")
        } else if literal {
            raw.to_string()
        } else {
            dialect.quote_string(raw)
        },
    )
}

/// Undoes the backslash escaping MySQL applies to the expressions it
/// stores in `information_schema` (generated columns, functional indexes
/// and checks): every quote and backslash in them is prefixed with one.
fn unescape(expression: &str) -> String {
    let mut out = String::with_capacity(expression.len());
    let mut chars = expression.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.extend(chars.next()),
            c => out.push(c),
        }
    }
    out
}

/// `expression` without the pair of parentheses that wraps all of it, if
/// there is one: MySQL prints `(a + b)` where MariaDB prints `a + b`.
fn strip_outer_parens(expression: &str) -> &str {
    let trimmed = expression.trim();
    if !(trimmed.starts_with('(') && trimmed.ends_with(')')) {
        return trimmed;
    }
    // The opening parenthesis must close at the very end, not before.
    let mut depth = 0usize;
    let mut quote = None;
    let mut chars = trimmed.char_indices();
    while let Some((index, c)) = chars.next() {
        match quote {
            Some(open) => {
                if c == '\\' && open != '`' {
                    chars.next();
                } else if c == open {
                    quote = None;
                }
            }
            None => match c {
                '\'' | '"' | '`' => quote = Some(c),
                '(' => depth += 1,
                ')' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 && index + 1 < trimmed.len() {
                        return trimmed;
                    }
                }
                _ => {}
            },
        }
    }
    trimmed[1..trimmed.len() - 1].trim()
}

/// One key part of an index.
struct IndexPart {
    /// Column name, or the expression of a functional key part.
    column: String,
    /// How the part is written in `CREATE INDEX`.
    definition: String,
}

async fn indexes(session: &MysqlSession, table: &TableRef) -> Result<Vec<IndexInfo>> {
    let dialect = session.dialect();
    // Functional key parts (MySQL 8.0.13) have no column name, only an
    // expression. MariaDB indexes generated columns instead.
    let expression = if !is_mariadb(session) && session.server().version_number >= (8, 0, 13) {
        "s.EXPRESSION"
    } else {
        "NULL"
    };
    let rows = session
        .query(&format!(
            "SELECT s.INDEX_NAME, s.NON_UNIQUE, s.INDEX_TYPE, s.COLUMN_NAME, s.SUB_PART, \
                    s.COLLATION, {expression} \
             FROM information_schema.STATISTICS s \
             WHERE s.TABLE_SCHEMA = {} AND s.TABLE_NAME = {} \
             ORDER BY s.INDEX_NAME, s.SEQ_IN_INDEX",
            lit(session, &table.schema),
            lit(session, &table.name),
        ))
        .await?;

    // (name, unique, method, parts), parts in key order.
    let mut grouped: Vec<(String, bool, String, Vec<IndexPart>)> = Vec::new();
    for row in &rows {
        let name = text(row, 0);
        let descending = if text(row, 5) == "D" { " DESC" } else { "" };
        let part = match opt(row, 3) {
            Some(column) => {
                let prefix = opt(row, 4).map_or_else(String::new, |length| format!("({length})"));
                IndexPart {
                    definition: format!("{}{prefix}{descending}", dialect.quote_ident(&column)),
                    column,
                }
            }
            None => {
                let expression = unescape(&text(row, 6));
                let expression = strip_outer_parens(&expression);
                IndexPart {
                    definition: format!("({expression}){descending}"),
                    column: expression.to_string(),
                }
            }
        };
        match grouped.last_mut() {
            Some(last) if last.0 == name => last.3.push(part),
            _ => grouped.push((name, text(row, 1) == "0", text(row, 2), vec![part])),
        }
    }

    let mut indexes: Vec<IndexInfo> = grouped
        .into_iter()
        .map(|(name, unique, method, parts)| {
            let primary = name == "PRIMARY";
            IndexInfo {
                definition: Some(index_definition(
                    dialect, table, &name, unique, &method, &parts,
                )),
                columns: parts.into_iter().map(|part| part.column).collect(),
                name,
                unique,
                primary,
                method,
                predicate: None,
            }
        })
        .collect();
    indexes.sort_by(|a, b| (!a.primary, &a.name).cmp(&(!b.primary, &b.name)));
    Ok(indexes)
}

/// The statement that creates an index. Neither engine prints one
/// (`SHOW CREATE TABLE` shows indexes inside the table), so it is built
/// from the catalog.
fn index_definition(
    dialect: &dyn Dialect,
    table: &TableRef,
    name: &str,
    unique: bool,
    method: &str,
    parts: &[IndexPart],
) -> String {
    let table = dialect.table_name(table);
    let parts = parts
        .iter()
        .map(|part| part.definition.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    if name == "PRIMARY" {
        return format!("ALTER TABLE {table} ADD PRIMARY KEY ({parts});");
    }
    let (flavour, using) = match method {
        "FULLTEXT" => ("FULLTEXT ", String::new()),
        "SPATIAL" => ("SPATIAL ", String::new()),
        _ if unique => ("UNIQUE ", format!(" USING {method}")),
        _ => ("", format!(" USING {method}")),
    };
    format!(
        "CREATE {flavour}INDEX {} ON {table} ({parts}){using};",
        dialect.quote_ident(name)
    )
}

/// The primary key and unique constraints, which on these engines are
/// exactly the primary and unique indexes.
fn key_constraints(session: &MysqlSession, indexes: &[IndexInfo]) -> Vec<ConstraintInfo> {
    let dialect = session.dialect();
    indexes
        .iter()
        .filter(|index| index.unique)
        .map(|index| {
            let columns = index
                .columns
                .iter()
                .map(|column| dialect.quote_ident(column))
                .collect::<Vec<_>>()
                .join(", ");
            ConstraintInfo {
                name: index.name.clone(),
                kind: if index.primary {
                    ConstraintKind::PrimaryKey
                } else {
                    ConstraintKind::Unique
                },
                definition: if index.primary {
                    format!("PRIMARY KEY ({columns})")
                } else {
                    format!("UNIQUE ({columns})")
                },
                columns: index.columns.clone(),
            }
        })
        .collect()
}

/// Check constraints, where the server has them and lists them with their
/// table: MySQL from 8.0.16, MariaDB from 10.3.10. Older MySQL servers
/// parsed and ignored `CHECK`.
async fn check_constraints(
    session: &MysqlSession,
    table: &TableRef,
) -> Result<Vec<ConstraintInfo>> {
    let mariadb = is_mariadb(session);
    let version = session.server().version_number;
    let schema = lit(session, &table.schema);
    let name = lit(session, &table.name);
    let sql = if mariadb && version >= (10, 3, 10) {
        // MariaDB names checks per table and says which table directly
        // (the TABLE_NAME column came with 10.3.10).
        format!(
            "SELECT c.CONSTRAINT_NAME, c.CHECK_CLAUSE \
             FROM information_schema.CHECK_CONSTRAINTS c \
             WHERE c.CONSTRAINT_SCHEMA = {schema} AND c.TABLE_NAME = {name}"
        )
    } else if !mariadb && version >= (8, 0, 16) {
        // MySQL names checks per database; the table is in TABLE_CONSTRAINTS.
        format!(
            "SELECT c.CONSTRAINT_NAME, c.CHECK_CLAUSE \
             FROM information_schema.CHECK_CONSTRAINTS c \
             JOIN information_schema.TABLE_CONSTRAINTS t \
               ON t.CONSTRAINT_SCHEMA = c.CONSTRAINT_SCHEMA AND t.CONSTRAINT_NAME = c.CONSTRAINT_NAME \
             WHERE t.CONSTRAINT_TYPE = 'CHECK' AND t.TABLE_SCHEMA = {schema} AND t.TABLE_NAME = {name}"
        )
    } else {
        return Ok(Vec::new());
    };
    let mut checks: Vec<ConstraintInfo> = session
        .query(&sql)
        .await?
        .iter()
        .map(|row| {
            let clause = if mariadb {
                text(row, 1)
            } else {
                unescape(&text(row, 1))
            };
            ConstraintInfo {
                name: text(row, 0),
                kind: ConstraintKind::Check,
                // The catalog does not say which columns a check reads.
                columns: Vec::new(),
                definition: format!("CHECK ({})", strip_outer_parens(&clause)),
            }
        })
        .collect();
    checks.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(checks)
}

/// Foreign keys matching `filter`, a condition over `k`
/// (`KEY_COLUMN_USAGE`). Both databases are carried, so references across
/// databases are reported as they are.
async fn foreign_keys(session: &MysqlSession, filter: &str) -> Result<Vec<ForeignKeyInfo>> {
    let rows = session
        .query(&format!(
            "SELECT k.CONSTRAINT_NAME, k.TABLE_SCHEMA, k.TABLE_NAME, k.COLUMN_NAME, \
                    k.REFERENCED_TABLE_SCHEMA, k.REFERENCED_TABLE_NAME, k.REFERENCED_COLUMN_NAME, \
                    r.UPDATE_RULE, r.DELETE_RULE \
             FROM information_schema.KEY_COLUMN_USAGE k \
             JOIN information_schema.REFERENTIAL_CONSTRAINTS r \
               ON r.CONSTRAINT_SCHEMA = k.CONSTRAINT_SCHEMA AND r.CONSTRAINT_NAME = k.CONSTRAINT_NAME \
              AND r.TABLE_NAME = k.TABLE_NAME \
             WHERE k.REFERENCED_TABLE_NAME IS NOT NULL AND {filter} \
             ORDER BY k.ORDINAL_POSITION"
        ))
        .await?;
    let mut keys: Vec<ForeignKeyInfo> = Vec::new();
    for row in &rows {
        let name = text(row, 0);
        let table = TableRef::new(text(row, 1), text(row, 2));
        let position = match keys
            .iter()
            .position(|key| key.name == name && key.table == table)
        {
            Some(position) => position,
            None => {
                keys.push(ForeignKeyInfo {
                    name,
                    table,
                    columns: Vec::new(),
                    referenced_table: TableRef::new(text(row, 4), text(row, 5)),
                    referenced_columns: Vec::new(),
                    on_update: text(row, 7),
                    on_delete: text(row, 8),
                });
                keys.len() - 1
            }
        };
        keys[position].columns.push(text(row, 3));
        keys[position].referenced_columns.push(text(row, 6));
    }
    keys.sort_by(|a, b| {
        (&a.table.schema, &a.table.name, &a.name).cmp(&(&b.table.schema, &b.table.name, &b.name))
    });
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::MysqlDialect;

    fn default_of(
        mariadb: bool,
        raw: Option<&str>,
        data_type: &str,
        extra: &str,
    ) -> Option<String> {
        normalize_default(
            mariadb,
            raw,
            data_type,
            extra,
            &MysqlDialect::new(Engine::MySql),
        )
    }

    #[test]
    fn string_literals_are_read_back_to_their_text() {
        assert_eq!(string_literal_value("''").as_deref(), Some(""));
        assert_eq!(
            string_literal_value("'standard'").as_deref(),
            Some("standard")
        );
        assert_eq!(string_literal_value("'it''s'").as_deref(), Some("it's"));
        assert_eq!(string_literal_value(r"'a\\b'").as_deref(), Some(r"a\b"));
        assert_eq!(string_literal_value(r"'a\'b'").as_deref(), Some("a'b"));
        assert_eq!(
            string_literal_value(r"'line\nnext\ttab'").as_deref(),
            Some("line\nnext\ttab")
        );
        // Not exactly one literal: left alone.
        for sql in [
            "42",
            "uuid()",
            "'a' 'b'",
            "'unterminated",
            r"'ends with escape\'",
            "b'101'",
        ] {
            assert_eq!(string_literal_value(sql), None, "{sql}");
        }
    }

    #[test]
    fn defaults_read_the_same_on_both_engines() {
        // (MySQL raw, MySQL EXTRA, MariaDB raw, DATA_TYPE, expected)
        for (mysql, extra, mariadb, data_type, expected) in [
            (
                Some("standard"),
                "",
                Some("'standard'"),
                "enum",
                Some("'standard'"),
            ),
            (Some(""), "", Some("''"), "set", Some("''")),
            (Some("1"), "", Some("1"), "tinyint", Some("1")),
            (Some("-5"), "", Some("-5"), "int", Some("-5")),
            (Some("0.00"), "", Some("0.00"), "decimal", Some("0.00")),
            (Some("1.5"), "", Some("1.5"), "double", Some("1.5")),
            // A number in a text column is a string.
            (Some("42"), "", Some("'42'"), "varchar", Some("'42'")),
            (
                Some("2020-01-02"),
                "",
                Some("'2020-01-02'"),
                "date",
                Some("'2020-01-02'"),
            ),
            // A default holding a backslash is written in the form that
            // does not depend on the backslash mode (see the dialect).
            (
                Some(r"it's \ x"),
                "",
                Some(r"'it''s \\ x'"),
                "varchar",
                Some("_utf8mb4 X'69742773205c2078'"),
            ),
            (
                Some("it's"),
                "",
                Some("'it''s'"),
                "varchar",
                Some("'it''s'"),
            ),
            // The string NULL is not the absence of a default.
            (Some("NULL"), "", Some("'NULL'"), "varchar", Some("'NULL'")),
            (None, "", Some("NULL"), "varchar", None),
            // NOT NULL without a default: NULL on both.
            (None, "", None, "varchar", None),
            (Some("b'101'"), "", Some("b'101'"), "bit", Some("b'101'")),
            (
                Some("CURRENT_TIMESTAMP"),
                "DEFAULT_GENERATED ON UPDATE CURRENT_TIMESTAMP",
                Some("current_timestamp()"),
                "timestamp",
                Some("CURRENT_TIMESTAMP"),
            ),
            (
                Some("CURRENT_TIMESTAMP(6)"),
                "DEFAULT_GENERATED",
                Some("current_timestamp(6)"),
                "datetime",
                Some("CURRENT_TIMESTAMP(6)"),
            ),
            // MySQL 5.7 has no DEFAULT_GENERATED marker.
            (
                Some("CURRENT_TIMESTAMP"),
                "",
                Some("current_timestamp()"),
                "timestamp",
                Some("CURRENT_TIMESTAMP"),
            ),
            (
                Some("uuid()"),
                "DEFAULT_GENERATED",
                Some("uuid()"),
                "char",
                Some("(uuid())"),
            ),
        ] {
            assert_eq!(
                default_of(false, mysql, data_type, extra).as_deref(),
                expected,
                "MySQL {mysql:?}"
            );
            assert_eq!(
                default_of(true, mariadb, data_type, "").as_deref(),
                expected,
                "MariaDB {mariadb:?}"
            );
        }
        // What looks like a literal is only one in a column of that type.
        for raw in ["b'c", "0x41", "current_timestamp()"] {
            assert_eq!(
                default_of(false, Some(raw), "varchar", "").as_deref(),
                Some(MysqlDialect::new(Engine::MySql).quote_string(raw).as_str()),
                "{raw}"
            );
        }
        assert_eq!(
            default_of(true, Some("'b''c'"), "enum", "").as_deref(),
            Some("'b''c'")
        );
        // Binary defaults keep the engine's spelling; both are literals.
        assert_eq!(
            default_of(false, Some("0x0102"), "varbinary", "").as_deref(),
            Some("0x0102")
        );
        assert_eq!(
            default_of(true, Some("x'0102'"), "varbinary", "").as_deref(),
            Some("x'0102'")
        );
    }

    #[test]
    fn mysql_expression_escaping_is_undone() {
        assert_eq!(unescape("(`n` * 2)"), "(`n` * 2)");
        assert_eq!(
            unescape(r"concat(`s`,_utf8mb4\'a\\\'b\')"),
            r"concat(`s`,_utf8mb4'a\'b')"
        );
        assert_eq!(unescape(r"(`s` <> _utf8mb4\'\')"), "(`s` <> _utf8mb4'')");
        assert_eq!(unescape("trailing\\"), "trailing");
        assert_eq!(unescape(""), "");
    }

    #[test]
    fn only_a_wrapping_pair_of_parentheses_is_removed() {
        assert_eq!(strip_outer_parens("(`price` >= 0)"), "`price` >= 0");
        assert_eq!(strip_outer_parens("`price` >= 0"), "`price` >= 0");
        assert_eq!(strip_outer_parens(" ( a ) "), "a");
        // Two groups: the first parenthesis closes before the end.
        assert_eq!(strip_outer_parens("(a) + (b)"), "(a) + (b)");
        assert_eq!(strip_outer_parens("((a) + (b))"), "(a) + (b)");
        assert_eq!(strip_outer_parens("lower(`s`)"), "lower(`s`)");
        // Parentheses inside strings and names do not count.
        assert_eq!(
            strip_outer_parens("(`a` = ')') or (b)"),
            "(`a` = ')') or (b)"
        );
        assert_eq!(strip_outer_parens("(`a` = '(')"), "`a` = '('");
        assert_eq!(strip_outer_parens(r"(`a` = '\')')"), r"`a` = '\')'");
        assert_eq!(strip_outer_parens("(`odd)name` > 1)"), "`odd)name` > 1");
        assert_eq!(strip_outer_parens("()"), "");
        assert_eq!(strip_outer_parens(""), "");
    }

    #[test]
    fn index_statements() {
        let d = MysqlDialect::new(Engine::MySql);
        let table = TableRef::new("shop", "products");
        let part = |column: &str, definition: &str| IndexPart {
            column: column.into(),
            definition: definition.into(),
        };
        assert_eq!(
            index_definition(
                &d,
                &table,
                "products_category_idx",
                false,
                "BTREE",
                &[part("category_id", "`category_id`")]
            ),
            "CREATE INDEX `products_category_idx` ON `shop`.`products` (`category_id`) USING BTREE;"
        );
        assert_eq!(
            index_definition(
                &d,
                &table,
                "u",
                true,
                "HASH",
                &[part("a", "`a`(5)"), part("b", "`b` DESC")]
            ),
            "CREATE UNIQUE INDEX `u` ON `shop`.`products` (`a`(5), `b` DESC) USING HASH;"
        );
        assert_eq!(
            index_definition(
                &d,
                &table,
                "ft",
                false,
                "FULLTEXT",
                &[part("name", "`name`")]
            ),
            "CREATE FULLTEXT INDEX `ft` ON `shop`.`products` (`name`);"
        );
        assert_eq!(
            index_definition(&d, &table, "sp", false, "SPATIAL", &[part("g", "`g`")]),
            "CREATE SPATIAL INDEX `sp` ON `shop`.`products` (`g`);"
        );
        assert_eq!(
            index_definition(
                &d,
                &table,
                "fx",
                false,
                "BTREE",
                &[part("lower(`s`)", "(lower(`s`))")]
            ),
            "CREATE INDEX `fx` ON `shop`.`products` ((lower(`s`))) USING BTREE;"
        );
        assert_eq!(
            index_definition(
                &d,
                &table,
                "PRIMARY",
                true,
                "BTREE",
                &[part("order_id", "`order_id`"), part("line_no", "`line_no`")]
            ),
            "ALTER TABLE `shop`.`products` ADD PRIMARY KEY (`order_id`, `line_no`);"
        );
    }

    #[test]
    fn event_schedules() {
        assert_eq!(
            event_schedule(
                "RECURRING",
                None,
                "1",
                "DAY",
                Some("2030-01-01 03:00:00"),
                None
            ),
            "EVERY 1 DAY STARTS 2030-01-01 03:00:00"
        );
        assert_eq!(
            event_schedule("RECURRING", None, "2", "HOUR", None, None),
            "EVERY 2 HOUR"
        );
        assert_eq!(
            event_schedule(
                "RECURRING",
                None,
                "1",
                "WEEK",
                Some("2030-01-01 00:00:00"),
                Some("2031-01-01 00:00:00")
            ),
            "EVERY 1 WEEK STARTS 2030-01-01 00:00:00 ENDS 2031-01-01 00:00:00"
        );
        assert_eq!(
            event_schedule("ONE TIME", Some("2030-01-01 03:00:00"), "", "", None, None),
            "AT 2030-01-01 03:00:00"
        );
    }

    #[test]
    fn system_databases_are_recognised() {
        for name in ["information_schema", "performance_schema", "mysql", "sys"] {
            assert!(is_system_database(name), "{name}");
            assert!(SYSTEM_DATABASES.contains(&format!("'{name}'")), "{name}");
        }
        assert!(is_system_database("INFORMATION_SCHEMA"));
        assert!(!is_system_database("kerno"));
        assert!(!is_system_database("mysql_backup"));
    }
}
