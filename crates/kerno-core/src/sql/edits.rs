//! Pending edits of table data: turning them into `INSERT`, `UPDATE` and
//! `DELETE` statements, and applying those atomically.
//!
//! The statements carry their values as literals, so what the user reviews
//! is exactly what runs. Rows are addressed by their identity columns (see
//! [`crate::schema::TableStructure::row_identity`]) with the values they had
//! when they were read; every statement must change exactly one row, or the
//! whole apply is rolled back.

use std::sync::Arc;

use crate::dialect::Dialect;
use crate::driver::{Session, TxState};
use crate::error::{DbError, Error, Result};
use crate::exec;
use crate::schema::{ColumnInfo, TableRef};

/// A value to store in a cell, or to match a cell against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CellValue {
    /// SQL `NULL`.
    Null,
    /// The column's default (`DEFAULT`). Only meaningful when storing.
    Default,
    /// A value in the server's text form.
    Text(String),
    /// Raw bytes, for binary columns.
    Bytes(Vec<u8>),
}

/// One pending change to a table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowChange {
    /// Insert a row. Columns not listed take their default.
    Insert {
        /// `(column, value)` pairs.
        values: Vec<(String, CellValue)>,
    },
    /// Change cells of one existing row.
    Update {
        /// The row's identity columns with their original values.
        key: Vec<(String, CellValue)>,
        /// `(column, new value)` pairs.
        set: Vec<(String, CellValue)>,
    },
    /// Delete one existing row.
    Delete {
        /// The row's identity columns with their original values.
        key: Vec<(String, CellValue)>,
    },
}

fn column<'a>(columns: &'a [ColumnInfo], name: &str, table: &TableRef) -> Result<&'a ColumnInfo> {
    columns
        .iter()
        .find(|column| column.name == name)
        .ok_or_else(|| {
            Error::Config(format!(
                "table {}.{} has no column \"{name}\"",
                table.schema, table.name
            ))
        })
}

/// The `WHERE` clause that addresses one row by its identity.
pub(super) fn where_clause(
    dialect: &dyn Dialect,
    table: &TableRef,
    columns: &[ColumnInfo],
    key: &[(String, CellValue)],
) -> Result<String> {
    if key.is_empty() {
        // Never emit an UPDATE or DELETE that addresses every row.
        return Err(Error::Config(format!(
            "rows of {}.{} cannot be addressed: the table has no primary or unique key",
            table.schema, table.name
        )));
    }
    let mut conditions = Vec::with_capacity(key.len());
    for (name, value) in key {
        let info = column(columns, name, table)?;
        let ident = dialect.quote_ident(name);
        conditions.push(match value {
            CellValue::Null => format!("{ident} IS NULL"),
            CellValue::Default => {
                return Err(Error::Config(format!(
                    "DEFAULT is not a value column \"{name}\" can be matched against"
                )));
            }
            value => format!("{ident} = {}", dialect.literal(value, info)),
        });
    }
    Ok(conditions.join(" AND "))
}

/// The statements that carry out `changes`, one per change, in the order
/// given. An `Update` that sets nothing produces no statement.
pub fn edit_statements(
    dialect: &dyn Dialect,
    table: &TableRef,
    columns: &[ColumnInfo],
    changes: &[RowChange],
) -> Result<Vec<String>> {
    let name = dialect.table_name(table);
    let mut statements = Vec::with_capacity(changes.len());
    for change in changes {
        match change {
            RowChange::Insert { values } if values.is_empty() => {
                statements.push(dialect.insert_defaults(table));
            }
            RowChange::Insert { values } => {
                let mut names = Vec::with_capacity(values.len());
                let mut literals = Vec::with_capacity(values.len());
                for (column_name, value) in values {
                    let info = column(columns, column_name, table)?;
                    names.push(dialect.quote_ident(column_name));
                    literals.push(dialect.literal(value, info));
                }
                statements.push(format!(
                    "INSERT INTO {name} ({}) VALUES ({})",
                    names.join(", "),
                    literals.join(", ")
                ));
            }
            RowChange::Update { set, .. } if set.is_empty() => {}
            RowChange::Update { key, set } => {
                let mut assignments = Vec::with_capacity(set.len());
                for (column_name, value) in set {
                    let info = column(columns, column_name, table)?;
                    assignments.push(format!(
                        "{} = {}",
                        dialect.quote_ident(column_name),
                        dialect.literal(value, info)
                    ));
                }
                statements.push(format!(
                    "UPDATE {name} SET {} WHERE {}",
                    assignments.join(", "),
                    where_clause(dialect, table, columns, key)?
                ));
            }
            RowChange::Delete { key } => {
                statements.push(format!(
                    "DELETE FROM {name} WHERE {}",
                    where_clause(dialect, table, columns, key)?
                ));
            }
        }
    }
    Ok(statements)
}

/// Why an apply did not go through. Nothing was changed: the transaction
/// was rolled back.
#[derive(Debug)]
pub struct ApplyFailure {
    /// Index of the statement that failed, when one did.
    pub statement: Option<usize>,
    /// What went wrong.
    pub error: Error,
}

impl std::fmt::Display for ApplyFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.statement {
            Some(index) => write!(f, "statement {} failed: {}", index + 1, self.error),
            None => self.error.fmt(f),
        }
    }
}

impl std::error::Error for ApplyFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Runs edit statements in one transaction: all of them take effect, or
/// none. Each statement must change exactly one row; a statement that
/// changes none (the row was edited or deleted by someone else since it was
/// read) or several aborts the apply.
///
/// The session must not already be in a transaction. Must be called from
/// within the Tokio runtime.
pub async fn apply_edits(
    session: &Arc<dyn Session>,
    statements: &[String],
) -> std::result::Result<(), ApplyFailure> {
    let whole = |error: Error| ApplyFailure {
        statement: None,
        error,
    };
    if session.tx_state() != TxState::Idle {
        return Err(whole(Error::Config(
            "a transaction is already open in this session; commit or roll it back first".into(),
        )));
    }
    if statements.is_empty() {
        return Ok(());
    }
    exec::collect(session.clone(), "START TRANSACTION")
        .await
        .map_err(whole)?;

    let mut failure = None;
    for (index, sql) in statements.iter().enumerate() {
        let error = match exec::collect(session.clone(), sql.as_str()).await {
            Ok(collected) => {
                let changed: u64 = collected
                    .results
                    .iter()
                    .filter_map(|result| result.rows_affected)
                    .sum();
                if changed == 1 {
                    continue;
                }
                Error::Db(Box::new(DbError::message(if changed == 0 {
                    "no row was changed: the row was modified or deleted by someone else since it was read"
                        .to_string()
                } else {
                    format!("{changed} rows would be changed instead of one")
                })))
            }
            Err(error) => error,
        };
        failure = Some(ApplyFailure {
            statement: Some(index),
            error,
        });
        break;
    }

    match failure {
        None => exec::collect(session.clone(), "COMMIT")
            .await
            .map(|_| ())
            .map_err(whole),
        Some(failure) => {
            if let Err(error) = exec::collect(session.clone(), "ROLLBACK").await {
                tracing::warn!(%error, "rollback after a failed apply did not succeed");
            }
            Err(failure)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::test_support::AnsiDialect;
    use crate::result::TypeKind;

    fn col(name: &str, kind: TypeKind) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: String::new(),
            kind,
            nullable: true,
            default: None,
            generation: None,
            on_update: None,
            invisible: false,
            primary_key_position: None,
            comment: None,
            collation: None,
            enum_values: Vec::new(),
        }
    }

    fn fixture() -> (TableRef, Vec<ColumnInfo>) {
        (
            TableRef::new("shop", "order items"),
            vec![
                col("order_id", TypeKind::Integer),
                col("line_no", TypeKind::Integer),
                col("note", TypeKind::Text),
                col("price", TypeKind::Decimal),
                col("paid", TypeKind::Boolean),
                col("blob", TypeKind::Binary),
                col("sku", TypeKind::Text),
            ],
        )
    }

    fn text(value: &str) -> CellValue {
        CellValue::Text(value.into())
    }

    fn key() -> Vec<(String, CellValue)> {
        vec![
            ("order_id".into(), text("7")),
            ("line_no".into(), text("2")),
        ]
    }

    fn generate(changes: &[RowChange]) -> Vec<String> {
        let (table, columns) = fixture();
        edit_statements(&AnsiDialect, &table, &columns, changes).unwrap()
    }

    #[test]
    fn update_sets_cells_of_the_row_addressed_by_its_key() {
        let statements = generate(&[RowChange::Update {
            key: key(),
            set: vec![
                ("note".into(), text("it's \"fine\"")),
                ("price".into(), text("12.50")),
                ("paid".into(), text("t")),
                ("sku".into(), CellValue::Null),
                ("blob".into(), CellValue::Bytes(vec![0xde, 0xad])),
            ],
        }]);
        assert_eq!(
            statements,
            [
                "UPDATE \"shop\".\"order items\" SET \"note\" = 'it''s \"fine\"', \"price\" = 12.50, \
              \"paid\" = TRUE, \"sku\" = NULL, \"blob\" = X'dead' \
              WHERE \"order_id\" = 7 AND \"line_no\" = 2"
            ]
        );
    }

    #[test]
    fn insert_lists_given_columns_and_uses_defaults_for_the_rest() {
        let statements = generate(&[
            RowChange::Insert {
                values: vec![
                    ("order_id".into(), text("7")),
                    ("line_no".into(), CellValue::Default),
                    ("note".into(), text("")),
                    ("sku".into(), CellValue::Null),
                ],
            },
            RowChange::Insert { values: vec![] },
        ]);
        assert_eq!(
            statements,
            [
                "INSERT INTO \"shop\".\"order items\" (\"order_id\", \"line_no\", \"note\", \"sku\") \
                 VALUES (7, DEFAULT, '', NULL)",
                "INSERT INTO \"shop\".\"order items\" DEFAULT VALUES",
            ]
        );
    }

    #[test]
    fn delete_addresses_one_row() {
        assert_eq!(
            generate(&[RowChange::Delete { key: key() }]),
            ["DELETE FROM \"shop\".\"order items\" WHERE \"order_id\" = 7 AND \"line_no\" = 2"]
        );
    }

    #[test]
    fn a_key_value_that_is_null_is_matched_with_is_null() {
        let statements = generate(&[RowChange::Delete {
            key: vec![
                ("order_id".into(), text("7")),
                ("sku".into(), CellValue::Null),
            ],
        }]);
        assert_eq!(
            statements,
            ["DELETE FROM \"shop\".\"order items\" WHERE \"order_id\" = 7 AND \"sku\" IS NULL"]
        );
    }

    #[test]
    fn numbers_that_are_not_plain_numbers_are_quoted() {
        // The server decides what 'NaN' or garbage means for the column; a
        // bare word would be read as an identifier or worse.
        let statements = generate(&[RowChange::Update {
            key: vec![("order_id".into(), text("7 OR 1=1"))],
            set: vec![
                ("price".into(), text("NaN")),
                ("line_no".into(), text("1; DROP TABLE x")),
            ],
        }]);
        assert_eq!(
            statements,
            [
                "UPDATE \"shop\".\"order items\" SET \"price\" = 'NaN', \"line_no\" = '1; DROP TABLE x' \
              WHERE \"order_id\" = '7 OR 1=1'"
            ]
        );
    }

    #[test]
    fn changes_keep_their_order_and_empty_updates_vanish() {
        let statements = generate(&[
            RowChange::Delete { key: key() },
            RowChange::Update {
                key: key(),
                set: vec![],
            },
            RowChange::Insert {
                values: vec![("note".into(), text("n"))],
            },
        ]);
        assert_eq!(statements.len(), 2);
        assert!(statements[0].starts_with("DELETE"));
        assert!(statements[1].starts_with("INSERT"));
    }

    #[test]
    fn a_row_without_identity_is_never_updated_or_deleted() {
        let (table, columns) = fixture();
        for change in [
            RowChange::Delete { key: vec![] },
            RowChange::Update {
                key: vec![],
                set: vec![("note".into(), text("x"))],
            },
        ] {
            let error = edit_statements(&AnsiDialect, &table, &columns, &[change]).unwrap_err();
            assert!(
                error.to_string().contains("no primary or unique key"),
                "{error}"
            );
        }
    }

    #[test]
    fn unknown_columns_and_default_in_a_key_are_refused() {
        let (table, columns) = fixture();
        let unknown = RowChange::Insert {
            values: vec![("nope".into(), text("x"))],
        };
        let error = edit_statements(&AnsiDialect, &table, &columns, &[unknown]).unwrap_err();
        assert!(error.to_string().contains("\"nope\""), "{error}");

        let default_key = RowChange::Delete {
            key: vec![("order_id".into(), CellValue::Default)],
        };
        assert!(edit_statements(&AnsiDialect, &table, &columns, &[default_key]).is_err());
    }
}
