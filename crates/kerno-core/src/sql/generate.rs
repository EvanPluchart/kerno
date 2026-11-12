//! Statement templates for a table: the `SELECT`, `INSERT`, `UPDATE` and
//! `DELETE` the navigator writes into a query tab.
//!
//! Values are `:name` parameters (see [`super::params`]), one per column,
//! which Kerno prompts for before the statement runs. Renaming, dropping
//! and creating objects is on [`Dialect`], because engines disagree on it.

use crate::dialect::Dialect;
use crate::schema::{ColumnGeneration, ColumnInfo, TableRef, TableStructure};

/// Above this many items a column list is written one item per line.
const INLINE_ITEMS: usize = 4;

/// Whether the server computes the column, so that it cannot be written.
fn is_computed(column: &ColumnInfo) -> bool {
    matches!(
        column.generation,
        Some(
            ColumnGeneration::Stored(_)
                | ColumnGeneration::Virtual(_)
                | ColumnGeneration::IdentityAlways
        )
    )
}

/// A parameter name for each of `names`, in order: the name itself when it
/// is one `:name` can carry (`[A-Za-z_][A-Za-z0-9_]*`), otherwise the name
/// with every other character replaced by `_`. A name already taken gets a
/// numeric suffix, so two columns never share a parameter.
fn placeholders<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut taken: Vec<String> = Vec::new();
    for name in names {
        let mut base: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        if !base.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
            base.insert(0, '_');
        }
        let mut candidate = base.clone();
        let mut suffix = 2;
        while taken.contains(&candidate) {
            candidate = format!("{base}_{suffix}");
            suffix += 1;
        }
        taken.push(candidate);
    }
    taken
}

/// `items` separated by commas: on one line when there are few, otherwise
/// one per line, indented, between `open` and `close`.
fn list(items: &[String], open: &str, close: &str) -> String {
    if items.len() <= INLINE_ITEMS {
        return format!("{open}{}{close}", items.join(", "));
    }
    let end = if close.is_empty() {
        String::new()
    } else {
        format!("\n{close}")
    };
    format!("{}\n    {}{end}", open.trim_end(), items.join(",\n    "))
}

/// The `WHERE` clause that addresses one row by its identity, with the
/// statement's terminator. Without a row identity the condition is left
/// for the user to write: what comes out is not a statement the server
/// accepts, so it can never run against every row.
fn where_clause(dialect: &dyn Dialect, structure: &TableStructure, params: &[String]) -> String {
    let conditions: Vec<String> = structure
        .row_identity()
        .unwrap_or_default()
        .iter()
        .filter_map(|name| {
            let index = structure.columns.iter().position(|c| &c.name == name)?;
            Some(format!(
                "{} = :{}",
                dialect.quote_ident(name),
                params[index]
            ))
        })
        .collect();
    if conditions.is_empty() {
        "WHERE -- add a condition\n;".into()
    } else {
        format!("WHERE {};", conditions.join("\n  AND "))
    }
}

/// A `SELECT` of `columns` from `table`, limited to `limit` rows. Without
/// columns it selects `*`.
pub fn select_template(
    dialect: &dyn Dialect,
    table: &TableRef,
    columns: &[ColumnInfo],
    limit: u64,
) -> String {
    let names: Vec<String> = columns
        .iter()
        .map(|column| dialect.quote_ident(&column.name))
        .collect();
    let columns = if names.is_empty() {
        "SELECT *".into()
    } else {
        list(&names, "SELECT ", "")
    };
    format!(
        "{columns}\nFROM {}\n{};",
        dialect.table_name(table),
        dialect.limit_offset(limit, 0)
    )
}

/// An `INSERT` into `table` with one `:name` parameter per column. Columns
/// the server computes (generated columns, `GENERATED ALWAYS` identities)
/// are left out; those with a default are kept, for the user to remove.
pub fn insert_template(dialect: &dyn Dialect, table: &TableRef, columns: &[ColumnInfo]) -> String {
    let params = placeholders(columns.iter().map(|column| column.name.as_str()));
    let (names, values): (Vec<String>, Vec<String>) = columns
        .iter()
        .zip(&params)
        .filter(|(column, _)| !is_computed(column))
        .map(|(column, param)| (dialect.quote_ident(&column.name), format!(":{param}")))
        .unzip();
    if names.is_empty() {
        return format!("{};", dialect.insert_defaults(table));
    }
    format!(
        "INSERT INTO {} {}\n{};",
        dialect.table_name(table),
        list(&names, "(", ")"),
        list(&values, "VALUES (", ")")
    )
}

/// An `UPDATE` of one row of `table`: every column that can be written and
/// is not part of the row identity is set to its `:name` parameter, and the
/// row is addressed by its identity (see [`TableStructure::row_identity`]).
///
/// A table whose columns are all part of its key has its key columns set
/// instead, from `:new_...` parameters. Without a row identity the `WHERE`
/// clause holds a comment in place of a condition, which the server
/// rejects: the template never updates every row.
pub fn update_template(
    dialect: &dyn Dialect,
    table: &TableRef,
    structure: &TableStructure,
) -> String {
    let columns = &structure.columns;
    let identity = structure.row_identity().unwrap_or_default();
    let new_names: Vec<String> = columns
        .iter()
        .map(|column| format!("new_{}", column.name))
        .collect();
    // The parameters of the columns, then those of their new values.
    let params = placeholders(
        columns
            .iter()
            .map(|column| column.name.as_str())
            .chain(new_names.iter().map(String::as_str)),
    );
    let assignments = |key: bool, offset: usize| -> Vec<String> {
        columns
            .iter()
            .enumerate()
            .filter(|(_, column)| !is_computed(column) && identity.contains(&column.name) == key)
            .map(|(index, column)| {
                format!(
                    "{} = :{}",
                    dialect.quote_ident(&column.name),
                    params[offset + index]
                )
            })
            .collect()
    };
    let mut set = assignments(false, 0);
    if set.is_empty() {
        set = assignments(true, columns.len());
    }
    let set = if set.is_empty() {
        "-- add an assignment".into()
    } else {
        set.join(",\n    ")
    };
    format!(
        "UPDATE {}\nSET {set}\n{}",
        dialect.table_name(table),
        where_clause(dialect, structure, &params)
    )
}

/// A `DELETE` of one row of `table`, addressed by its identity. Without a
/// row identity the `WHERE` clause holds a comment in place of a condition,
/// which the server rejects: the template never deletes every row.
pub fn delete_template(
    dialect: &dyn Dialect,
    table: &TableRef,
    structure: &TableStructure,
) -> String {
    let params = placeholders(structure.columns.iter().map(|column| column.name.as_str()));
    format!(
        "DELETE FROM {}\n{}",
        dialect.table_name(table),
        where_clause(dialect, structure, &params)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::test_support::AnsiDialect;
    use crate::result::TypeKind;
    use crate::schema::IndexInfo;
    use crate::sql::{LexRules, find_params};

    fn column(name: &str) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: "text".into(),
            kind: TypeKind::Text,
            nullable: false,
            default: None,
            generation: None,
            primary_key_position: None,
            comment: None,
            collation: None,
            enum_values: Vec::new(),
        }
    }

    fn key(name: &str, position: u32) -> ColumnInfo {
        ColumnInfo {
            primary_key_position: Some(position),
            ..column(name)
        }
    }

    fn generated(name: &str, generation: ColumnGeneration) -> ColumnInfo {
        ColumnInfo {
            generation: Some(generation),
            ..column(name)
        }
    }

    fn table() -> TableRef {
        TableRef::new("shop", "order items")
    }

    fn structure(columns: Vec<ColumnInfo>) -> TableStructure {
        TableStructure {
            columns,
            ..TableStructure::default()
        }
    }

    /// The `:name` parameters Kerno would prompt for.
    fn params(sql: &str) -> Vec<String> {
        find_params(sql, LexRules::POSTGRES)
            .iter()
            .map(|param| param.key.label())
            .collect()
    }

    #[test]
    fn select_lists_few_columns_on_one_line() {
        assert_eq!(
            select_template(
                &AnsiDialect,
                &table(),
                &[column("id"), column("order"), column("full name")],
                100
            ),
            "SELECT \"id\", \"order\", \"full name\"\n\
             FROM \"shop\".\"order items\"\n\
             LIMIT 100;"
        );
    }

    #[test]
    fn select_lists_many_columns_one_per_line() {
        let columns: Vec<_> = ["a", "b", "c", "d", "Weird \"name\""]
            .into_iter()
            .map(column)
            .collect();
        assert_eq!(
            select_template(&AnsiDialect, &table(), &columns, 1000),
            "SELECT\n    \"a\",\n    \"b\",\n    \"c\",\n    \"d\",\n    \"Weird \"\"name\"\"\"\n\
             FROM \"shop\".\"order items\"\n\
             LIMIT 1000;"
        );
    }

    #[test]
    fn select_without_columns_takes_them_all() {
        assert_eq!(
            select_template(&AnsiDialect, &TableRef::new("s", "t"), &[], 50),
            "SELECT *\nFROM \"s\".\"t\"\nLIMIT 50;"
        );
    }

    #[test]
    fn insert_skips_computed_columns_and_names_a_parameter_per_column() {
        let columns = [
            generated("id", ColumnGeneration::IdentityAlways),
            column("order"),
            column("full name"),
            generated("total", ColumnGeneration::Stored("a + b".into())),
            generated("label", ColumnGeneration::Virtual("upper(x)".into())),
        ];
        let sql = insert_template(&AnsiDialect, &table(), &columns);
        assert_eq!(
            sql,
            "INSERT INTO \"shop\".\"order items\" (\"order\", \"full name\")\n\
             VALUES (:order, :full_name);"
        );
        assert_eq!(params(&sql), [":order", ":full_name"]);
    }

    #[test]
    fn insert_keeps_columns_that_have_a_default_and_wraps_long_lists() {
        let columns = [
            generated("id", ColumnGeneration::AutoIncrement),
            generated("n", ColumnGeneration::IdentityByDefault),
            column("a"),
            column("b"),
            column("c"),
        ];
        assert_eq!(
            insert_template(&AnsiDialect, &TableRef::new("s", "t"), &columns),
            "INSERT INTO \"s\".\"t\" (\n    \"id\",\n    \"n\",\n    \"a\",\n    \"b\",\n    \"c\"\n)\n\
             VALUES (\n    :id,\n    :n,\n    :a,\n    :b,\n    :c\n);"
        );
    }

    #[test]
    fn insert_with_nothing_to_give_inserts_defaults() {
        let t = TableRef::new("s", "t");
        let computed = [generated("id", ColumnGeneration::IdentityAlways)];
        for columns in [&computed[..], &[]] {
            assert_eq!(
                insert_template(&AnsiDialect, &t, columns),
                "INSERT INTO \"s\".\"t\" DEFAULT VALUES;"
            );
        }
    }

    #[test]
    fn parameter_names_are_valid_and_unique() {
        let columns: Vec<_> = [
            "full name",
            "full_name",
            "Weird \"name\"",
            "1st",
            "caf\u{e9}",
            "",
            "_",
            "price$",
        ]
        .into_iter()
        .map(column)
        .collect();
        let sql = insert_template(&AnsiDialect, &TableRef::new("s", "t"), &columns);
        let expected = [
            ":full_name",
            ":full_name_2",
            ":Weird__name_",
            ":_1st",
            ":caf_",
            ":_",
            ":__2",
            ":price_",
        ];
        assert_eq!(params(&sql), expected);
        assert!(sql.ends_with(&format!("VALUES (\n    {}\n);", expected.join(",\n    "))));
    }

    #[test]
    fn update_sets_the_other_columns_and_addresses_the_row_by_its_key() {
        let structure = structure(vec![
            key("line_no", 2),
            key("order_id", 1),
            column("order"),
            column("full name"),
            generated("total", ColumnGeneration::Stored("a + b".into())),
        ]);
        let sql = update_template(&AnsiDialect, &table(), &structure);
        assert_eq!(
            sql,
            "UPDATE \"shop\".\"order items\"\n\
             SET \"order\" = :order,\n    \"full name\" = :full_name\n\
             WHERE \"order_id\" = :order_id\n  AND \"line_no\" = :line_no;"
        );
        assert_eq!(
            params(&sql),
            [":order", ":full_name", ":order_id", ":line_no"]
        );
    }

    #[test]
    fn update_and_delete_use_a_unique_index_when_there_is_no_primary_key() {
        let structure = TableStructure {
            columns: vec![column("email"), column("name")],
            indexes: vec![IndexInfo {
                name: "u".into(),
                columns: vec!["email".into()],
                unique: true,
                primary: false,
                backs_constraint: false,
                method: "btree".into(),
                predicate: None,
                definition: None,
            }],
            ..TableStructure::default()
        };
        let t = TableRef::new("s", "t");
        assert_eq!(
            update_template(&AnsiDialect, &t, &structure),
            "UPDATE \"s\".\"t\"\nSET \"name\" = :name\nWHERE \"email\" = :email;"
        );
        assert_eq!(
            delete_template(&AnsiDialect, &t, &structure),
            "DELETE FROM \"s\".\"t\"\nWHERE \"email\" = :email;"
        );
    }

    #[test]
    fn update_of_a_table_that_is_all_key_changes_the_key() {
        // `id` and `new_id`: the new values must not share a parameter with
        // the old ones.
        let structure = structure(vec![key("id", 1), key("new_id", 2)]);
        let sql = update_template(&AnsiDialect, &TableRef::new("s", "t"), &structure);
        assert_eq!(
            sql,
            "UPDATE \"s\".\"t\"\n\
             SET \"id\" = :new_id_2,\n    \"new_id\" = :new_new_id\n\
             WHERE \"id\" = :id\n  AND \"new_id\" = :new_id;"
        );
        assert_eq!(params(&sql), [":new_id_2", ":new_new_id", ":id", ":new_id"]);
    }

    #[test]
    fn without_a_row_identity_the_where_clause_is_left_to_the_user() {
        let structure = structure(vec![column("a"), column("full name")]);
        let t = TableRef::new("s", "t");
        let update = update_template(&AnsiDialect, &t, &structure);
        assert_eq!(
            update,
            "UPDATE \"s\".\"t\"\n\
             SET \"a\" = :a,\n    \"full name\" = :full_name\n\
             WHERE -- add a condition\n;"
        );
        let delete = delete_template(&AnsiDialect, &t, &structure);
        assert_eq!(
            delete,
            "DELETE FROM \"s\".\"t\"\nWHERE -- add a condition\n;"
        );
        // The comment hides nothing: no parameter, and no statement that
        // would run against every row.
        assert_eq!(params(&delete), Vec::<String>::new());
    }

    #[test]
    fn update_with_nothing_to_set_says_so() {
        let t = TableRef::new("s", "t");
        assert_eq!(
            update_template(&AnsiDialect, &t, &TableStructure::default()),
            "UPDATE \"s\".\"t\"\nSET -- add an assignment\nWHERE -- add a condition\n;"
        );
    }

    #[test]
    fn delete_addresses_the_row_by_its_key() {
        let structure = structure(vec![key("id", 1), column("order")]);
        assert_eq!(
            delete_template(&AnsiDialect, &table(), &structure),
            "DELETE FROM \"shop\".\"order items\"\nWHERE \"id\" = :id;"
        );
    }
}
