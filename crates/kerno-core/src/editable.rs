//! Whether the result of a query can be edited as rows of a table, as
//! DBeaver edits a single-source result (`ResultSetViewer`,
//! `ResultSetUtils.bindAttributes`): its columns come from one table, which
//! is a table and not a view, and it holds that table's key.
//!
//! The decision is taken once per result, in three steps that each may
//! stop with the reason ([`ReadOnly`]): [`check`] on the statement and the
//! column origins the server sent, with no round trip; [`resolve`] on the
//! answer of [`crate::Dialect::origin_sql`], which names the table and its
//! kind; [`decide`] on the table's structure, with the key the table data
//! view uses ([`TableStructure::row_identity`]).

use std::fmt;
use std::sync::Arc;

use crate::driver::Session;
use crate::exec;
use crate::result::{Column, ColumnOrigin, ResultStore};
use crate::schema::{RelationKind, TableRef, TableStructure};
use crate::sql::{LexRules, NOT_A_SELECT, single_table_select};

/// Why a result is read-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadOnly {
    /// The connection, named, takes no write.
    Connection(String),
    /// Several statements, or something else than a `SELECT`.
    NotSelect,
    /// What the statement does that may repeat or merge rows, to follow
    /// "the statement" (see [`single_table_select`]).
    Shape(&'static str),
    /// Every column is an expression or a literal.
    NoTableColumn,
    /// Columns of several tables.
    SeveralTables,
    /// The table of the columns was not found (dropped since).
    Gone,
    /// Its columns come from a view or a materialized view.
    View(TableRef, RelationKind),
    /// The table has no primary key and no unique index on NOT NULL
    /// columns.
    NoKey(TableRef),
    /// A column of the key is not among the result's.
    KeyMissing { column: String, table: TableRef },
    /// The table could not be read: what the server said.
    Unread(String),
}

impl ReadOnly {
    /// A few words for the menus ("why not").
    pub fn short(&self) -> &'static str {
        match self {
            Self::Connection(_) => "read-only connection",
            Self::NotSelect => "not a single SELECT",
            Self::Shape(what) => what,
            Self::NoTableColumn => "no column of a table",
            Self::SeveralTables => "columns of several tables",
            Self::Gone => "table not found",
            Self::View(..) => "view",
            Self::NoKey(_) => "no key",
            Self::KeyMissing { .. } => "key not in the result",
            Self::Unread(_) => "table not read",
        }
    }
}

/// `schema.name`, as the reasons quote a table.
fn quoted(table: &TableRef) -> String {
    format!("`{}.{}`", table.schema, table.name)
}

impl fmt::Display for ReadOnly {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Read-only result: ")?;
        match self {
            Self::Connection(name) => write!(f, "“{name}” is a read-only connection."),
            Self::NotSelect => write!(f, "the statement {NOT_A_SELECT}."),
            Self::Shape(what) => write!(f, "the statement {what}."),
            Self::NoTableColumn => f.write_str("no column comes from a table."),
            Self::SeveralTables => f.write_str("its columns come from several tables."),
            Self::Gone => f.write_str("the table of its columns was not found."),
            Self::View(table, RelationKind::MaterializedView) => write!(
                f,
                "{} is a materialized view; its rows are read-only.",
                quoted(table)
            ),
            Self::View(table, _) => {
                write!(
                    f,
                    "{} is a view; its rows are read-only here.",
                    quoted(table)
                )
            }
            Self::NoKey(table) => write!(
                f,
                "{} has no primary or unique key, so a row cannot be told from another.",
                quoted(table)
            ),
            Self::KeyMissing { column, table } => write!(
                f,
                "the key column `{column}` of {} is not in the result.",
                quoted(table)
            ),
            Self::Unread(error) => write!(f, "its table could not be read: {error}."),
        }
    }
}

/// The one table a result's columns come from, as the server named it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// PostgreSQL: the table's OID.
    Oid(u32),
    /// MySQL and MariaDB: the table itself.
    Named(TableRef),
}

/// The first step, without a round trip: the statement is a plain `SELECT`
/// ([`single_table_select`]) and the columns that come from a table all
/// come from the same one. `statement` is the statement as written, not a
/// filter wrapped around it.
pub fn check(statement: &str, rules: LexRules, columns: &[Column]) -> Result<Source, ReadOnly> {
    let shape = single_table_select(statement, rules);
    if shape == Some(NOT_A_SELECT) {
        return Err(ReadOnly::NotSelect);
    }
    let mut sources = columns.iter().filter_map(|column| {
        Some(match column.origin.as_ref()? {
            ColumnOrigin::Oid { table, .. } => Source::Oid(*table),
            ColumnOrigin::Named { schema, table, .. } => {
                Source::Named(TableRef::new(schema.as_str(), table.as_str()))
            }
        })
    });
    let source = sources.next().ok_or(ReadOnly::NoTableColumn)?;
    if sources.any(|other| other != source) {
        return Err(ReadOnly::SeveralTables);
    }
    match shape {
        Some(what) => Err(ReadOnly::Shape(what)),
        None => Ok(source),
    }
}

/// The table a result's columns come from: its name, its kind, and the
/// table column each column of the result shows (`None`: an expression).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub table: TableRef,
    pub kind: RelationKind,
    pub columns: Vec<Option<String>>,
}

/// The kind of a relation as PostgreSQL's `relkind` letters it; MySQL's
/// lookup answers with the same letters.
fn relation_kind(letter: &str) -> RelationKind {
    match letter {
        "p" => RelationKind::PartitionedTable,
        "f" => RelationKind::ForeignTable,
        "v" => RelationKind::View,
        "m" => RelationKind::MaterializedView,
        _ => RelationKind::Table,
    }
}

/// The second step: names the table of `source` from the rows of
/// [`crate::Dialect::origin_sql`] (schema, table, kind letter, column
/// number, column name; one row per column of the table).
pub fn resolve(
    columns: &[Column],
    source: &Source,
    rows: &ResultStore,
) -> Result<Origin, ReadOnly> {
    if rows.row_count() == 0 || rows.columns().len() < 5 {
        return Err(ReadOnly::Gone);
    }
    let text = |row: usize, column: usize| rows.text(row, column).unwrap_or_default();
    let table = TableRef::new(text(0, 0), text(0, 1));
    let kind = relation_kind(text(0, 2));
    let columns = columns
        .iter()
        .map(|column| match (column.origin.as_ref()?, source) {
            (ColumnOrigin::Oid { table, column }, Source::Oid(oid)) if table == oid => {
                let number = column.to_string();
                (0..rows.row_count())
                    .find(|&row| text(row, 3) == number)
                    .map(|row| text(row, 4).to_string())
            }
            (ColumnOrigin::Named { column, .. }, Source::Named(_)) => Some(column.clone()),
            _ => None,
        })
        .collect();
    Ok(Origin {
        table,
        kind,
        columns,
    })
}

/// What editing a result as rows of one table needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Editable {
    pub table: TableRef,
    /// The columns that address a row.
    pub identity: Vec<String>,
    /// The table column each column of the result is written to: `None`
    /// for an expression and for a column shown a second time.
    pub columns: Vec<Option<String>>,
}

/// The last step: the origin is a table, not a view, with a key, and the
/// result holds every column of that key.
pub fn decide(origin: Origin, structure: &TableStructure) -> Result<Editable, ReadOnly> {
    let Origin {
        table,
        kind,
        columns,
    } = origin;
    if !kind.is_table() {
        return Err(ReadOnly::View(table, kind));
    }
    let Some(identity) = structure.row_identity() else {
        return Err(ReadOnly::NoKey(table));
    };
    // A table column shown twice is written through its first showing.
    let mut seen: Vec<&str> = Vec::new();
    let columns: Vec<Option<String>> = columns
        .iter()
        .map(|name| {
            let name = name.as_deref()?;
            let known = structure.columns.iter().any(|info| info.name == name);
            (known && !seen.contains(&name)).then(|| {
                seen.push(name);
                name.to_string()
            })
        })
        .collect();
    if let Some(column) = identity.iter().find(|key| !seen.contains(&key.as_str())) {
        return Err(ReadOnly::KeyMissing {
            column: column.clone(),
            table,
        });
    }
    Ok(Editable {
        table,
        identity,
        columns,
    })
}

/// The steps after [`check`], on `session`, a session in the result's
/// database (PostgreSQL's OIDs are the database's): the lookup of the
/// table, its structure, the decision. With the editable result, the
/// table's structure.
///
/// Must be called from within the Tokio runtime.
pub async fn lookup(
    session: &Arc<dyn Session>,
    columns: &[Column],
    source: &Source,
) -> Result<(Editable, TableStructure), ReadOnly> {
    let unread = |error: crate::Error| ReadOnly::Unread(error.to_string());
    let sql = session
        .dialect()
        .origin_sql(source)
        .ok_or(ReadOnly::NoTableColumn)?;
    let mut answer = exec::collect(session.clone(), sql)
        .await
        .map_err(unread)?
        .results
        .into_iter()
        .next()
        .unwrap_or_default();
    let mut rows = ResultStore::new(answer.columns.clone());
    for batch in answer.batches.drain(..) {
        rows.push(batch);
    }
    let origin = resolve(columns, source, &rows)?;
    let structure = session
        .table_structure(&origin.table)
        .await
        .map_err(unread)?;
    Ok((decide(origin, &structure)?, structure))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::{BatchBuilder, TypeKind};
    use crate::schema::{ColumnInfo, IndexInfo};

    fn plain(name: &str) -> Column {
        Column::new(name, "int", TypeKind::Integer)
    }

    fn pg(name: &str, table: u32, column: i16) -> Column {
        plain(name).with_origin(Some(ColumnOrigin::Oid { table, column }))
    }

    fn my(name: &str, table: &str, column: &str) -> Column {
        plain(name).with_origin(Some(ColumnOrigin::Named {
            schema: "kerno".into(),
            table: table.into(),
            column: column.into(),
        }))
    }

    fn info(name: &str, nullable: bool, key: Option<u32>) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: "int".into(),
            kind: TypeKind::Integer,
            nullable,
            default: None,
            generation: None,
            on_update: None,
            invisible: false,
            primary_key_position: key,
            comment: None,
            collation: None,
            enum_values: Vec::new(),
        }
    }

    /// `id` (primary key), `name`, `email` (NOT NULL, unique when asked).
    fn structure(primary: bool, unique_email: bool) -> TableStructure {
        TableStructure {
            columns: vec![
                info("id", false, primary.then_some(1)),
                info("name", true, None),
                info("email", false, None),
            ],
            indexes: unique_email
                .then(|| IndexInfo {
                    name: "u_email".into(),
                    columns: vec!["email".into()],
                    unique: true,
                    primary: false,
                    backs_constraint: false,
                    method: "btree".into(),
                    predicate: None,
                    definition: None,
                })
                .into_iter()
                .collect(),
            ..TableStructure::default()
        }
    }

    /// The answer of the origin lookup for table 42 `shop.customers`.
    fn lookup(kind: &str) -> ResultStore {
        let mut store = ResultStore::new(
            ["schema", "table", "kind", "number", "name"]
                .map(|name| Column::new(name, "text", TypeKind::Text))
                .into(),
        );
        let mut batch = BatchBuilder::new(5);
        for (number, name) in [("1", "id"), ("2", "name"), ("3", "email")] {
            for cell in ["shop", "customers", kind, number, name] {
                batch.push_text(cell);
            }
        }
        store.push(batch.finish());
        store
    }

    fn customers() -> TableRef {
        TableRef::new("shop", "customers")
    }

    #[test]
    fn the_statement_and_the_origins_come_first() {
        let rules = LexRules::POSTGRES;
        let one = [pg("id", 42, 1), pg("n", 42, 2), plain("e")];
        let cases: Vec<(&str, Vec<Column>, Result<Source, ReadOnly>)> = vec![
            (
                "select id, name n, 1 e from t",
                one.to_vec(),
                Ok(Source::Oid(42)),
            ),
            (
                "select 1, 2",
                vec![plain("a"), plain("b")],
                Err(ReadOnly::NoTableColumn),
            ),
            (
                "select c.id, o.id from c join o on o.c = c.id",
                vec![pg("id", 42, 1), pg("id", 43, 1)],
                Err(ReadOnly::SeveralTables),
            ),
            // One table's columns, joined to another: rows may repeat.
            (
                "select c.id from c join o on o.c = c.id",
                vec![pg("id", 42, 1)],
                Err(ReadOnly::Shape("joins tables")),
            ),
            (
                "delete from t returning id",
                one.to_vec(),
                Err(ReadOnly::NotSelect),
            ),
            (
                "select distinct id from t",
                vec![pg("id", 42, 1)],
                Err(ReadOnly::Shape("uses DISTINCT")),
            ),
            (
                "select * from v",
                vec![
                    my("id", "customers", "id"),
                    my("name", "customers", "full_name"),
                ],
                Ok(Source::Named(TableRef::new("kerno", "customers"))),
            ),
            (
                "select c.id, o.id from c join o",
                vec![my("id", "customers", "id"), my("id", "orders", "id")],
                Err(ReadOnly::SeveralTables),
            ),
        ];
        for (sql, columns, expected) in cases {
            assert_eq!(check(sql, rules, &columns), expected, "{sql}");
        }
    }

    #[test]
    fn origins_are_named_by_the_lookup() {
        let columns = [pg("n", 42, 2), plain("e"), pg("key", 42, 1), pg("x", 7, 1)];
        let origin = resolve(&columns, &Source::Oid(42), &lookup("r")).unwrap();
        assert_eq!(origin.table, customers());
        assert_eq!(origin.kind, RelationKind::Table);
        assert_eq!(
            origin.columns,
            [Some("name".into()), None, Some("id".into()), None]
        );
        // MySQL names its columns itself; the lookup gives the kind.
        let columns = [my("n", "customers", "full_name"), plain("e")];
        let source = Source::Named(customers());
        let origin = resolve(&columns, &source, &lookup("v")).unwrap();
        assert_eq!(origin.kind, RelationKind::View);
        assert_eq!(origin.columns, [Some("full_name".into()), None]);
        // No row: the table is gone.
        let empty = ResultStore::new(lookup("r").columns().clone());
        assert_eq!(resolve(&columns, &source, &empty), Err(ReadOnly::Gone));
    }

    /// What [`decide`] gives: the table column of each result column.
    type Verdict = Result<Vec<Option<&'static str>>, ReadOnly>;

    #[test]
    fn a_table_with_its_key_in_the_result_is_editable() {
        let origin = |kind, columns: &[Option<&str>]| Origin {
            table: customers(),
            kind,
            columns: columns
                .iter()
                .map(|name| name.map(str::to_string))
                .collect(),
        };
        let table = RelationKind::Table;
        let missing = |column: &str| ReadOnly::KeyMissing {
            column: column.into(),
            table: customers(),
        };
        let cases: Vec<(Origin, TableStructure, Verdict)> = vec![
            (
                origin(table, &[Some("id"), Some("name"), None]),
                structure(true, false),
                Ok(vec![Some("id"), Some("name"), None]),
            ),
            // A column shown twice is written once.
            (
                origin(table, &[Some("name"), Some("id"), Some("name")]),
                structure(true, false),
                Ok(vec![Some("name"), Some("id"), None]),
            ),
            (
                origin(table, &[Some("name")]),
                structure(true, false),
                Err(missing("id")),
            ),
            // Without a primary key, a NOT NULL unique column is the key.
            (
                origin(table, &[Some("email"), Some("name")]),
                structure(false, true),
                Ok(vec![Some("email"), Some("name")]),
            ),
            (
                origin(table, &[Some("id"), Some("name")]),
                structure(false, true),
                Err(missing("email")),
            ),
            (
                origin(table, &[Some("id")]),
                structure(false, false),
                Err(ReadOnly::NoKey(customers())),
            ),
            (
                origin(RelationKind::View, &[Some("id")]),
                structure(true, false),
                Err(ReadOnly::View(customers(), RelationKind::View)),
            ),
            (
                origin(RelationKind::PartitionedTable, &[Some("id")]),
                structure(true, false),
                Ok(vec![Some("id")]),
            ),
        ];
        for (origin, structure, expected) in cases {
            let decided = decide(origin.clone(), &structure).map(|editable| editable.columns);
            let expected = expected.map(|columns| {
                columns
                    .into_iter()
                    .map(|name| name.map(str::to_string))
                    .collect::<Vec<_>>()
            });
            assert_eq!(decided, expected, "{origin:?}");
        }
    }

    #[test]
    fn every_reason_is_one_sentence() {
        let reasons = [
            (
                ReadOnly::Connection("Prod".into()),
                "Read-only result: “Prod” is a read-only connection.",
            ),
            (
                ReadOnly::NotSelect,
                "Read-only result: the statement is not a single SELECT.",
            ),
            (
                ReadOnly::Shape("joins tables"),
                "Read-only result: the statement joins tables.",
            ),
            (
                ReadOnly::NoTableColumn,
                "Read-only result: no column comes from a table.",
            ),
            (
                ReadOnly::SeveralTables,
                "Read-only result: its columns come from several tables.",
            ),
            (
                ReadOnly::View(customers(), RelationKind::View),
                "Read-only result: `shop.customers` is a view; its rows are read-only here.",
            ),
            (
                ReadOnly::NoKey(customers()),
                "Read-only result: `shop.customers` has no primary or unique key, so a row \
                 cannot be told from another.",
            ),
            (
                ReadOnly::KeyMissing {
                    column: "id".into(),
                    table: customers(),
                },
                "Read-only result: the key column `id` of `shop.customers` is not in the result.",
            ),
        ];
        for (reason, sentence) in reasons {
            assert_eq!(reason.to_string(), sentence);
            assert!(!reason.short().is_empty());
        }
    }
}
