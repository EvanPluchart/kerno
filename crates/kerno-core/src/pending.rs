//! The changes made to a page of table data and not applied yet: edited
//! cells, deleted rows and new rows. They become [`RowChange`]s, which
//! [`crate::sql::edit_statements`] writes as SQL.
//!
//! Rows are numbered as the grid shows them: the fetched rows first, in the
//! order they came, then the new rows.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::{Error, Result};
use crate::result::{ResultStore, TypeKind};
use crate::sql::{CellValue, RowChange};

/// Pending changes to the fetched page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pending {
    /// New values of cells of fetched rows, by row and column.
    cells: BTreeMap<(usize, usize), CellValue>,
    /// Fetched rows to delete.
    deleted: BTreeSet<usize>,
    /// New rows: a value per column, `Default` where none was given.
    inserted: Vec<Vec<CellValue>>,
}

/// A fetched cell as a value to compare with or to copy.
fn fetched(store: &ResultStore, row: usize, column: usize) -> CellValue {
    match store.cell(row, column) {
        None => CellValue::Null,
        Some(bytes) if store.columns()[column].kind == TypeKind::Binary => {
            CellValue::Bytes(bytes.to_vec())
        }
        Some(bytes) => CellValue::Text(String::from_utf8_lossy(bytes).into_owned()),
    }
}

impl Pending {
    /// Whether nothing is waiting to be applied.
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty() && self.deleted.is_empty() && self.inserted.is_empty()
    }

    /// How many rows are touched.
    pub fn count(&self) -> usize {
        let edited: BTreeSet<usize> = self
            .cells
            .keys()
            .map(|(row, _)| *row)
            .filter(|row| !self.deleted.contains(row))
            .collect();
        edited.len() + self.deleted.len() + self.inserted.len()
    }

    /// Number of new rows; they follow the fetched ones.
    pub fn inserted(&self) -> usize {
        self.inserted.len()
    }

    /// Forgets every change.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Gives a cell a new value. Giving a fetched cell the value it has
    /// forgets its edit.
    pub fn set(&mut self, store: &ResultStore, row: usize, column: usize, value: CellValue) {
        if column >= store.columns().len() {
            return;
        }
        let fetched_rows = store.row_count();
        if row < fetched_rows {
            if fetched(store, row, column) == value {
                self.cells.remove(&(row, column));
            } else {
                self.cells.insert((row, column), value);
            }
        } else if let Some(new) = self.inserted.get_mut(row - fetched_rows) {
            new[column] = value;
        }
    }

    /// The pending value of a cell: an edit of a fetched row, or the value
    /// of a new row. `None` for a fetched cell left as it is.
    pub fn value(&self, fetched_rows: usize, row: usize, column: usize) -> Option<&CellValue> {
        if row < fetched_rows {
            self.cells.get(&(row, column))
        } else {
            self.inserted.get(row - fetched_rows)?.get(column)
        }
    }

    /// Whether a row is new.
    pub fn is_new(&self, fetched_rows: usize, row: usize) -> bool {
        row >= fetched_rows && row - fetched_rows < self.inserted.len()
    }

    /// Whether a fetched row is to be deleted.
    pub fn is_deleted(&self, row: usize) -> bool {
        self.deleted.contains(&row)
    }

    /// Marks rows for deletion, last first so that taking new rows away
    /// does not renumber the ones still to take. A new row is simply
    /// forgotten.
    pub fn delete(&mut self, fetched_rows: usize, rows: impl IntoIterator<Item = usize>) {
        let mut rows: Vec<usize> = rows.into_iter().collect();
        rows.sort_unstable_by(|a, b| b.cmp(a));
        rows.dedup();
        for row in rows {
            if row < fetched_rows {
                self.deleted.insert(row);
            } else if row - fetched_rows < self.inserted.len() {
                self.inserted.remove(row - fetched_rows);
            }
        }
    }

    /// Takes a fetched row back to what was read: no edit, no deletion.
    pub fn revert(&mut self, row: usize) {
        self.deleted.remove(&row);
        self.cells.retain(|(edited, _), _| *edited != row);
    }

    /// Adds a row with every column at its default. Returns its number.
    pub fn add_row(&mut self, store: &ResultStore) -> usize {
        self.inserted
            .push(vec![CellValue::Default; store.columns().len()]);
        store.row_count() + self.inserted.len() - 1
    }

    /// Adds a copy of a row as it shows (pending edits included). The
    /// columns in `generated` are left to the server: a copied key would
    /// collide, and a computed column takes no value. Returns its number.
    pub fn duplicate(
        &mut self,
        store: &ResultStore,
        row: usize,
        generated: &[usize],
    ) -> Option<usize> {
        let fetched_rows = store.row_count();
        if row >= fetched_rows + self.inserted.len() {
            return None;
        }
        let copy = (0..store.columns().len())
            .map(|column| {
                if generated.contains(&column) {
                    CellValue::Default
                } else if let Some(pending) = self.value(fetched_rows, row, column) {
                    pending.clone()
                } else {
                    fetched(store, row, column)
                }
            })
            .collect();
        self.inserted.push(copy);
        Some(fetched_rows + self.inserted.len() - 1)
    }

    /// The changes, in the order they are applied: deletions, then edits,
    /// then new rows (so a deleted key can be given again). `identity`
    /// names the columns that address a row; they must be among the
    /// fetched columns.
    pub fn changes(&self, store: &ResultStore, identity: &[String]) -> Result<Vec<RowChange>> {
        let columns = store.columns();
        let key_columns: Vec<usize> = identity
            .iter()
            .map(|name| {
                columns
                    .iter()
                    .position(|column| &column.name == name)
                    .ok_or_else(|| {
                        Error::Config(format!(
                            "the key column \"{name}\" is not among the fetched columns"
                        ))
                    })
            })
            .collect::<Result<_>>()?;
        let needs_key = !self.deleted.is_empty() || !self.cells.is_empty();
        if needs_key && key_columns.is_empty() {
            return Err(Error::Config(
                "rows cannot be addressed: the table has no primary or unique key".into(),
            ));
        }
        // The values the row had when it was read, whatever was typed since.
        let key = |row: usize| -> Vec<(String, CellValue)> {
            key_columns
                .iter()
                .map(|&column| (columns[column].name.clone(), fetched(store, row, column)))
                .collect()
        };
        let mut changes = Vec::with_capacity(self.count());
        for &row in &self.deleted {
            changes.push(RowChange::Delete { key: key(row) });
        }
        let mut edited: BTreeMap<usize, Vec<(String, CellValue)>> = BTreeMap::new();
        for (&(row, column), value) in &self.cells {
            if !self.deleted.contains(&row) {
                edited
                    .entry(row)
                    .or_default()
                    .push((columns[column].name.clone(), value.clone()));
            }
        }
        for (row, set) in edited {
            changes.push(RowChange::Update { key: key(row), set });
        }
        for new in &self.inserted {
            changes.push(RowChange::Insert {
                values: new
                    .iter()
                    .enumerate()
                    .filter(|(_, value)| **value != CellValue::Default)
                    .map(|(column, value)| (columns[column].name.clone(), value.clone()))
                    .collect(),
            });
        }
        Ok(changes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::{BatchBuilder, Column};

    /// `id`, `name`, `photo` with rows (1, 'ann', NULL) and (2, NULL, 0x00ff).
    fn store() -> ResultStore {
        let mut store = ResultStore::new(
            vec![
                Column::new("id", "int4", TypeKind::Integer),
                Column::new("name", "text", TypeKind::Text),
                Column::new("photo", "bytea", TypeKind::Binary),
            ]
            .into(),
        );
        let mut batch = BatchBuilder::new(3);
        batch.push_text("1");
        batch.push_text("ann");
        batch.push_null();
        batch.push_text("2");
        batch.push_null();
        batch.push_bytes(&[0, 255]);
        store.push(batch.finish());
        store
    }

    fn text(value: &str) -> CellValue {
        CellValue::Text(value.into())
    }

    fn id() -> Vec<String> {
        vec!["id".to_string()]
    }

    #[test]
    fn an_edit_back_to_the_fetched_value_is_no_edit() {
        let store = store();
        let mut pending = Pending::default();
        assert!(pending.is_empty());
        pending.set(&store, 0, 1, text("bob"));
        assert_eq!(pending.value(2, 0, 1), Some(&text("bob")));
        assert_eq!(pending.count(), 1);
        pending.set(&store, 0, 1, text("ann"));
        assert!(pending.is_empty());
        // NULL and bytes compare with what was fetched too.
        pending.set(&store, 0, 2, CellValue::Null);
        pending.set(&store, 1, 1, CellValue::Null);
        pending.set(&store, 1, 2, CellValue::Bytes(vec![0, 255]));
        assert!(pending.is_empty());
        // The empty string is not NULL.
        pending.set(&store, 1, 1, text(""));
        assert_eq!(pending.count(), 1);
        // Outside the page: ignored.
        pending.set(&store, 9, 1, text("x"));
        pending.set(&store, 0, 9, text("x"));
        assert_eq!(pending.count(), 1);
    }

    #[test]
    fn changes_address_rows_by_the_key_they_were_read_with() {
        let store = store();
        let mut pending = Pending::default();
        // The key itself is edited: the row is still found by the old one.
        pending.set(&store, 0, 0, text("10"));
        pending.set(&store, 0, 1, CellValue::Null);
        pending.delete(2, [1]);
        // An edit of a row that is deleted anyway is dropped.
        pending.set(&store, 1, 1, text("gone"));
        let new = pending.add_row(&store);
        assert_eq!(new, 2);
        pending.set(&store, new, 1, text("cy"));
        assert!(pending.is_new(2, new) && !pending.is_new(2, 1));
        assert_eq!(pending.value(2, new, 0), Some(&CellValue::Default));
        assert_eq!(pending.count(), 3);
        assert_eq!(
            pending.changes(&store, &id()).unwrap(),
            vec![
                RowChange::Delete {
                    key: vec![("id".into(), text("2"))]
                },
                RowChange::Update {
                    key: vec![("id".into(), text("1"))],
                    set: vec![("id".into(), text("10")), ("name".into(), CellValue::Null)],
                },
                RowChange::Insert {
                    values: vec![("name".into(), text("cy"))]
                },
            ]
        );
    }

    #[test]
    fn rows_are_added_copied_taken_away_and_reverted() {
        let store = store();
        let mut pending = Pending::default();
        pending.set(&store, 1, 1, text("bea"));
        // A copy takes the row as it shows, without its generated key.
        let copy = pending.duplicate(&store, 1, &[0]).unwrap();
        assert_eq!(copy, 2);
        assert_eq!(pending.duplicate(&store, 7, &[]), None);
        let empty = pending.add_row(&store);
        assert_eq!((empty, pending.inserted()), (3, 2));
        assert_eq!(
            pending.changes(&store, &id()).unwrap()[1..],
            [
                RowChange::Insert {
                    values: vec![
                        ("name".into(), text("bea")),
                        ("photo".into(), CellValue::Bytes(vec![0, 255])),
                    ]
                },
                // Every column at its default.
                RowChange::Insert { values: vec![] },
            ]
        );
        // Deleting new rows forgets them; fetched ones are marked.
        pending.delete(2, [2, 3, 0]);
        assert_eq!(pending.inserted(), 0);
        assert!(pending.is_deleted(0) && !pending.is_deleted(1));
        pending.revert(0);
        pending.revert(1);
        assert!(pending.is_empty());
        pending.add_row(&store);
        pending.clear();
        assert!(pending.is_empty());
    }

    #[test]
    fn without_a_key_only_new_rows_can_be_written() {
        let store = store();
        let mut pending = Pending::default();
        pending.add_row(&store);
        assert_eq!(pending.changes(&store, &[]).unwrap().len(), 1);
        pending.set(&store, 0, 1, text("x"));
        assert!(
            pending
                .changes(&store, &[])
                .unwrap_err()
                .to_string()
                .contains("no primary or unique key")
        );
        assert!(
            pending
                .changes(&store, &["missing".to_string()])
                .unwrap_err()
                .to_string()
                .contains("not among the fetched columns")
        );
    }
}
