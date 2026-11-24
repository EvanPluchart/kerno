//! Where the columns of a result lead (K59): the foreign keys of the tables
//! its columns come from, whatever the statement (a join, a subset, an
//! alias), as far as the server says where each column comes from.

use std::collections::HashMap;
use std::sync::Arc;

use crate::driver::Session;
use crate::exec;
use crate::result::{Column, ColumnOrigin, ResultStore};
use crate::schema::{ForeignKeyInfo, TableRef};
use crate::sql::Catalog;

/// The table and the table column each column of a result shows, from what
/// the server sent with it ([`Column::origin`]): MySQL, MariaDB and SQLite
/// name them; PostgreSQL numbers them, named here on `session` (in the
/// result's database: the numbers are its own) with one query for every
/// table of the result. `None` for an expression, a literal, or a table no
/// longer there.
///
/// Must be called from within the Tokio runtime.
pub async fn named_origins(
    session: &Arc<dyn Session>,
    columns: &[Column],
) -> crate::Result<Vec<Option<(TableRef, String)>>> {
    let mut tables: Vec<u32> = Vec::new();
    for column in columns {
        if let Some(ColumnOrigin::Oid { table, .. }) = column.origin
            && !tables.contains(&table)
        {
            tables.push(table);
        }
    }
    let mut names: HashMap<(u32, i16), (TableRef, String)> = HashMap::new();
    if let Some(sql) = session.dialect().origin_names_sql(&tables) {
        let mut answer = exec::collect(session.clone(), sql)
            .await?
            .results
            .into_iter()
            .next()
            .unwrap_or_default();
        let mut rows = ResultStore::new(answer.columns.clone());
        for batch in answer.batches.drain(..) {
            rows.push(batch);
        }
        let text = |row: usize, column: usize| rows.text(row, column).unwrap_or_default();
        for row in 0..rows.row_count() {
            let (Ok(table), Ok(number)) = (text(row, 0).parse(), text(row, 3).parse()) else {
                continue;
            };
            names.insert(
                (table, number),
                (
                    TableRef::new(text(row, 1), text(row, 2)),
                    text(row, 4).into(),
                ),
            );
        }
    }
    Ok(named(columns, &names))
}

/// The table and the table column each column of a result shows: those
/// the server named, and those PostgreSQL numbered as `names` names them.
pub fn named(
    columns: &[Column],
    names: &HashMap<(u32, i16), (TableRef, String)>,
) -> Vec<Option<(TableRef, String)>> {
    columns
        .iter()
        .map(|column| match column.origin.as_ref()? {
            ColumnOrigin::Oid { table, column } => names.get(&(*table, *column)).cloned(),
            ColumnOrigin::Named {
                schema,
                table,
                column,
            } => Some((
                TableRef::new(schema.as_str(), table.as_str()),
                column.clone(),
            )),
        })
        .collect()
}

impl Catalog {
    /// The foreign keys of `table` the catalog holds: none when it does not
    /// know the table (made after it was read, or a system table).
    pub fn foreign_keys(&self, table: &TableRef) -> Vec<ForeignKeyInfo> {
        let relation = self
            .schemas
            .iter()
            .find(|schema| schema.name == table.schema)
            .and_then(|schema| {
                schema
                    .relations
                    .iter()
                    .find(|relation| relation.name == table.name)
            });
        relation.map_or_else(Vec::new, |relation| {
            relation
                .foreign_keys
                .iter()
                .map(|key| ForeignKeyInfo {
                    name: String::new(),
                    table: table.clone(),
                    columns: key.columns.clone(),
                    referenced_table: TableRef::new(
                        key.referenced_schema.as_str(),
                        key.referenced_table.as_str(),
                    ),
                    referenced_columns: key.referenced_columns.clone(),
                    on_update: String::new(),
                    on_delete: String::new(),
                })
                .collect()
        })
    }
}

/// A foreign key of a table some columns of a result come from, with the
/// column of the result that shows each of its columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyLink {
    pub key: ForeignKeyInfo,
    /// Per column of the key, in key order: the column of the result that
    /// shows it, `None` where the result does not.
    pub sources: Vec<Option<usize>>,
}

impl KeyLink {
    /// The first column of the key the result lacks: the key cannot be
    /// followed without it.
    pub fn missing(&self) -> Option<&str> {
        self.sources
            .iter()
            .zip(&self.key.columns)
            .find(|(source, _)| source.is_none())
            .map(|(_, column)| column.as_str())
    }

    /// Whether the result's column `source` shows a column of the key.
    pub fn holds(&self, source: usize) -> bool {
        self.sources.contains(&Some(source))
    }
}

/// The foreign keys a result's columns are in. `origins` is, per column of
/// the result, the table and the table column it shows (`None`: an
/// expression, or a column the server names no table for); `keys_of` gives a
/// table's foreign keys, none when they are not known.
///
/// The keys come table by table, in the order the result first shows a
/// column of each, and in the table's own order within one: a table's data
/// numbers them as its structure lists them. A key is listed when the
/// result shows at least one of its columns. A table read twice (a
/// self-join) cannot be told apart by origins: each column of a key is taken
/// from the first column of the result that shows it.
pub fn key_links(
    origins: &[Option<(TableRef, String)>],
    keys_of: impl Fn(&TableRef) -> Vec<ForeignKeyInfo>,
) -> Vec<KeyLink> {
    let mut tables: Vec<&TableRef> = Vec::new();
    for (table, _) in origins.iter().flatten() {
        if !tables.contains(&table) {
            tables.push(table);
        }
    }
    let source_of = |table: &TableRef, column: &str| {
        origins.iter().position(|origin| {
            origin
                .as_ref()
                .is_some_and(|(of, name)| of == table && name == column)
        })
    };
    let mut links = Vec::new();
    for table in tables {
        for key in keys_of(table) {
            let sources: Vec<Option<usize>> = key
                .columns
                .iter()
                .map(|column| source_of(table, column))
                .collect();
            if sources.iter().any(Option::is_some) {
                links.push(KeyLink { key, sources });
            }
        }
    }
    links
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(table: &str, columns: &[&str], to: &str, referenced: &[&str]) -> ForeignKeyInfo {
        ForeignKeyInfo {
            name: format!("{table}_fk"),
            table: TableRef::new("s", table),
            columns: columns.iter().map(|c| c.to_string()).collect(),
            referenced_table: TableRef::new("s", to),
            referenced_columns: referenced.iter().map(|c| c.to_string()).collect(),
            on_update: String::new(),
            on_delete: String::new(),
        }
    }

    fn from(table: &str, column: &str) -> Option<(TableRef, String)> {
        Some((TableRef::new("s", table), column.to_string()))
    }

    /// `orders (customer_id → customers, (pa, pb) → pairs)`, `customers`
    /// with none, `lines (order_id → orders)`.
    fn keys() -> Vec<(TableRef, Vec<ForeignKeyInfo>)> {
        vec![
            (
                TableRef::new("s", "orders"),
                vec![
                    key("orders", &["customer_id"], "customers", &["id"]),
                    key("orders", &["pa", "pb"], "pairs", &["a", "b"]),
                ],
            ),
            (TableRef::new("s", "customers"), Vec::new()),
            (
                TableRef::new("s", "lines"),
                vec![key("lines", &["order_id"], "orders", &["id"])],
            ),
        ]
    }

    fn links(origins: &[Option<(TableRef, String)>]) -> Vec<KeyLink> {
        let keys = keys();
        key_links(origins, |table| {
            keys.iter()
                .find(|(of, _)| of == table)
                .map_or_else(Vec::new, |(_, keys)| keys.clone())
        })
    }

    #[test]
    fn a_join_marks_the_columns_its_tables_keys_hold() {
        // select o.id, o.customer_id, c.full_name from orders o join customers c
        let found = links(&[
            from("orders", "id"),
            from("orders", "customer_id"),
            from("customers", "full_name"),
        ]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].key.referenced_table.name, "customers");
        assert_eq!(found[0].sources, [Some(1)]);
        assert_eq!(found[0].missing(), None);
        assert!(found[0].holds(1) && !found[0].holds(0));
    }

    #[test]
    fn a_key_of_two_columns_says_the_one_missing() {
        let found = links(&[from("orders", "pb"), None, from("lines", "order_id")]);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].key.referenced_table.name, "pairs");
        assert_eq!(found[0].sources, [None, Some(0)]);
        assert_eq!(found[0].missing(), Some("pa"));
        // Table by table, in the order the result first shows them.
        assert_eq!(found[1].key.referenced_table.name, "orders");
        assert_eq!(found[1].sources, [Some(2)]);
        let whole = links(&[from("orders", "pa"), from("orders", "pb")]);
        assert_eq!(whole[0].sources, [Some(0), Some(1)]);
        assert_eq!(whole[0].missing(), None);
    }

    #[test]
    fn a_catalogs_keys_are_a_tables_foreign_keys() {
        use crate::schema::RelationKind;
        use crate::sql::{CatalogForeignKey, CatalogRelation, CatalogSchema};
        let catalog = Catalog {
            schemas: vec![CatalogSchema {
                name: "s".into(),
                relations: vec![CatalogRelation {
                    name: "orders".into(),
                    kind: RelationKind::Table,
                    columns: Vec::new(),
                    foreign_keys: vec![CatalogForeignKey {
                        columns: vec!["customer_id".into()],
                        referenced_schema: "s".into(),
                        referenced_table: "customers".into(),
                        referenced_columns: vec!["id".into()],
                    }],
                }],
                routines: Vec::new(),
            }],
            search_path: Vec::new(),
        };
        let keys = catalog.foreign_keys(&TableRef::new("s", "orders"));
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].table, TableRef::new("s", "orders"));
        assert_eq!(keys[0].columns, ["customer_id"]);
        assert_eq!(keys[0].referenced_table, TableRef::new("s", "customers"));
        assert_eq!(keys[0].referenced_columns, ["id"]);
        assert!(catalog.foreign_keys(&TableRef::new("s", "gone")).is_empty());
        assert!(
            catalog
                .foreign_keys(&TableRef::new("t", "orders"))
                .is_empty()
        );
    }

    #[test]
    fn expressions_unknown_tables_and_self_joins() {
        assert!(links(&[None, None]).is_empty());
        assert!(links(&[from("elsewhere", "customer_id")]).is_empty());
        // A table shown twice: its first showing of each column counts, and
        // the key is listed once.
        let found = links(&[from("orders", "customer_id"), from("orders", "customer_id")]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].sources, [Some(0)]);
        // A table's keys keep its own order.
        let both = links(&[from("orders", "pa"), from("orders", "customer_id")]);
        let to: Vec<&str> = both
            .iter()
            .map(|link| link.key.referenced_table.name.as_str())
            .collect();
        assert_eq!(to, ["customers", "pairs"]);
    }
}
