//! Statement templates for a table: the `SELECT`, `INSERT`, `UPDATE` and
//! `DELETE` the navigator writes into a query tab.
//!
//! Values are `:name` parameters (see [`super::params`]), one per column,
//! which Kerno prompts for before the statement runs. Renaming, dropping
//! and creating objects is on [`Dialect`], because engines disagree on it.

use super::edits::{CellValue, RowChange, edit_statements, where_clause as key_condition};
use crate::dialect::Dialect;
use crate::error::{Error, Result};
use crate::pending::fetched;
use crate::result::ResultStore;
use crate::schema::{ColumnGeneration, ColumnInfo, RoutineKind, TableRef, TableStructure};

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

/// What "Generate SQL ▸" writes for rows of a table: DBeaver's result-set
/// generators (`SQLGeneratorSelectFromData`, `…SelectManyFromData`,
/// `…InsertFromData`, `…UpdateFromData`, `…DeleteFromData` in plugin
/// model.sql), with the row's values as literals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowSql {
    /// One `SELECT … WHERE key = …` per row.
    SelectByKey,
    /// One `SELECT … WHERE key IN (…)` for every row.
    SelectIn,
    Insert,
    /// Every other column set to the row's value, by key.
    Update,
    /// By key.
    Delete,
}

impl RowSql {
    pub const ALL: [RowSql; 5] = [
        RowSql::SelectByKey,
        RowSql::SelectIn,
        RowSql::Insert,
        RowSql::Update,
        RowSql::Delete,
    ];

    pub fn label(self) -> &'static str {
        match self {
            RowSql::SelectByKey => "SELECT by key",
            RowSql::SelectIn => "SELECT … IN",
            RowSql::Insert => "INSERT",
            RowSql::Update => "UPDATE",
            RowSql::Delete => "DELETE",
        }
    }

    /// Whether the statement finds rows by their key.
    pub fn needs_key(self) -> bool {
        self != RowSql::Insert
    }
}

/// The statements of `kind` for the fetched rows `rows` of `store`, rows
/// of `table` whose columns are `columns`, each ending with `;` on a line
/// of its own. `identity` names the columns that address a row (see
/// [`TableStructure::row_identity`]); without one, only `Insert` can be
/// written. Columns the server computes are not written.
pub fn rows_sql(
    kind: RowSql,
    dialect: &dyn Dialect,
    table: &TableRef,
    columns: &[ColumnInfo],
    identity: &[String],
    store: &ResultStore,
    rows: &[usize],
) -> Result<String> {
    let fetched_columns = store.columns();
    let index_of = |name: &str| {
        fetched_columns
            .iter()
            .position(|column| column.name == name)
            .ok_or_else(|| Error::Config(format!("the column \"{name}\" was not fetched")))
    };
    let key_of = |row: usize| -> Result<Vec<(String, CellValue)>> {
        identity
            .iter()
            .map(|name| Ok((name.clone(), fetched(store, row, index_of(name)?))))
            .collect()
    };
    // The columns that take a value, as fetched.
    let writable: Vec<(usize, &str)> = fetched_columns
        .iter()
        .enumerate()
        .filter(|(_, fetched)| {
            columns
                .iter()
                .any(|column| column.name == fetched.name && !is_computed(column))
        })
        .map(|(index, column)| (index, column.name.as_str()))
        .collect();
    // The row's values of the writable columns that are in the key
    // (`Some(true)`), out of it, or all of them.
    let values_of = |row: usize, key: Option<bool>| -> Vec<(String, CellValue)> {
        writable
            .iter()
            .filter(|(_, name)| key.is_none_or(|key| identity.iter().any(|n| n == name) == key))
            .map(|(index, name)| (name.to_string(), fetched(store, row, *index)))
            .collect()
    };
    let name = dialect.table_name(table);
    let statements = match kind {
        RowSql::SelectByKey => rows
            .iter()
            .map(|&row| {
                let condition = key_condition(dialect, table, columns, &key_of(row)?)?;
                Ok(format!("SELECT * FROM {name} WHERE {condition}"))
            })
            .collect::<Result<Vec<_>>>()?,
        RowSql::SelectIn => {
            if identity.is_empty() {
                // As `edits::where_clause` says it.
                key_condition(dialect, table, columns, &[])?;
            }
            let literal = |(name, value): &(String, CellValue)| {
                let info = columns.iter().find(|column| &column.name == name);
                match info {
                    Some(info) => Ok(dialect.literal(value, info)),
                    None => Err(Error::Config(format!(
                        "{} has no column \"{name}\"",
                        table.name
                    ))),
                }
            };
            let tuples = rows
                .iter()
                .map(|&row| {
                    let values = key_of(row)?
                        .iter()
                        .map(literal)
                        .collect::<Result<Vec<_>>>()?;
                    Ok(if values.len() == 1 {
                        values[0].clone()
                    } else {
                        format!("({})", values.join(", "))
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let names: Vec<String> = identity.iter().map(|n| dialect.quote_ident(n)).collect();
            let names = if names.len() == 1 {
                names[0].clone()
            } else {
                format!("({})", names.join(", "))
            };
            vec![format!(
                "SELECT * FROM {name} WHERE {names} IN ({})",
                tuples.join(", ")
            )]
        }
        RowSql::Insert | RowSql::Update | RowSql::Delete => {
            let changes = rows
                .iter()
                .map(|&row| {
                    Ok(match kind {
                        RowSql::Insert => RowChange::Insert {
                            values: values_of(row, None),
                        },
                        RowSql::Update => {
                            let mut set = values_of(row, Some(false));
                            // A table that is all key sets its key, as
                            // DBeaver does.
                            if set.is_empty() {
                                set = values_of(row, Some(true));
                            }
                            RowChange::Update {
                                key: key_of(row)?,
                                set,
                            }
                        }
                        _ => RowChange::Delete { key: key_of(row)? },
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            edit_statements(dialect, table, columns, &changes)?
        }
    };
    Ok(statements
        .iter()
        .map(|statement| format!("{statement};\n"))
        .collect())
}

/// How a routine takes an argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    In,
    Out,
    InOut,
    Variadic,
}

/// `arguments` as the engine prints them (`pg_get_function_arguments`, or
/// MySQL's `IN name TYPE` list): each argument's mode and name, `None` for
/// an unnamed PostgreSQL argument. Defaults are dropped.
fn arguments_of(arguments: &str) -> Vec<(Mode, Option<String>)> {
    // Split on the commas outside parentheses and quotes (`numeric(10,2)`).
    let mut parts = Vec::new();
    let (mut depth, mut quote, mut start) = (0i32, None, 0);
    for (at, c) in arguments.char_indices() {
        match (quote, c) {
            (Some(open), c) if c == open => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"' | '`') => quote = Some(c),
            (None, '(') => depth += 1,
            (None, ')') => depth -= 1,
            (None, ',') if depth == 0 => {
                parts.push(&arguments[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    parts.push(&arguments[start..]);
    // Types of more than one word, which an unnamed argument starts with.
    const TYPES: [&str; 8] = [
        "double precision",
        "character varying",
        "bit varying",
        "timestamp with",
        "timestamp without",
        "time with",
        "time without",
        "interval ",
    ];
    parts
        .into_iter()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| {
            let first = part.split_whitespace().next().unwrap_or_default();
            let (mode, rest) = match first.to_ascii_uppercase().as_str() {
                "IN" => (Mode::In, &part[2..]),
                "OUT" => (Mode::Out, &part[3..]),
                "INOUT" => (Mode::InOut, &part[5..]),
                "VARIADIC" => (Mode::Variadic, &part[8..]),
                _ => (Mode::In, part),
            };
            let rest = rest.trim_start();
            let name = if let Some(quoted) = rest.strip_prefix('"') {
                // `"my arg" integer`: `""` is a quote inside the name.
                let mut name = String::new();
                let mut chars = quoted.chars().peekable();
                while let Some(c) = chars.next() {
                    match c {
                        '"' if chars.peek() == Some(&'"') => {
                            chars.next();
                            name.push('"');
                        }
                        '"' => break,
                        c => name.push(c),
                    }
                }
                Some(name)
            } else {
                let lower = rest.to_ascii_lowercase();
                let words = rest.split_whitespace().count();
                let unnamed = words < 2 || TYPES.iter().any(|t| lower.starts_with(t));
                (!unnamed).then(|| {
                    rest.split_whitespace()
                        .next()
                        .unwrap_or_default()
                        .to_string()
                })
            };
            (mode, name)
        })
        .collect()
}

/// The statement that calls a routine, with a `:name` parameter for each
/// value it takes, for Kerno to prompt for (DBeaver's `procedureCall`
/// generator, `SQLDialect.generateStoredProcedureCall`). `arguments` is the
/// list as the engine prints it ([`crate::schema::RoutineInfo::arguments`]).
///
/// PostgreSQL: `CALL s.p(:a, NULL)` for a procedure (an `OUT` argument is
/// given as `NULL`), `SELECT * FROM s.f(:a)` for a function (its `OUT`
/// arguments are its result columns). MySQL: `SELECT db.f(:a)`, and `CALL
/// db.p(:a, @b)` where `OUT` and `INOUT` arguments are user variables (the
/// server refuses anything else), set first for `INOUT` and read after.
pub fn call_routine(
    dialect: &dyn Dialect,
    schema: &str,
    name: &str,
    kind: RoutineKind,
    arguments: &str,
) -> String {
    let routine = dialect.table_name(&TableRef::new(schema, name));
    let arguments = arguments_of(arguments);
    let names: Vec<String> = arguments
        .iter()
        .enumerate()
        .map(|(index, (_, name))| name.clone().unwrap_or_else(|| format!("arg{}", index + 1)))
        .collect();
    let params = placeholders(names.iter().map(String::as_str));
    let procedure = kind == RoutineKind::Procedure;
    let mysql = !dialect.has_schemas();
    let mut before = Vec::new();
    let mut after = Vec::new();
    let values: Vec<String> = arguments
        .iter()
        .zip(&params)
        .filter_map(|((mode, _), param)| match mode {
            Mode::Out if !procedure => None,
            Mode::Out | Mode::InOut if mysql => {
                if *mode == Mode::InOut {
                    before.push(format!("SET @{param} = :{param};\n"));
                }
                after.push(format!("@{param}"));
                Some(format!("@{param}"))
            }
            Mode::Out => Some("NULL".into()),
            _ => Some(format!(":{param}")),
        })
        .collect();
    let values = values.join(", ");
    let call = match (procedure, mysql) {
        (true, _) => format!("CALL {routine}({values});\n"),
        (false, true) => format!("SELECT {routine}({values});\n"),
        (false, false) => format!("SELECT * FROM {routine}({values});\n"),
    };
    let read = if after.is_empty() {
        String::new()
    } else {
        format!("SELECT {};\n", after.join(", "))
    };
    format!("{}{call}{read}", before.concat())
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
            on_update: None,
            invisible: false,
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

    /// MySQL's quoting and its lack of schemas, for what differs.
    struct Backticks;

    impl Dialect for Backticks {
        fn engine(&self) -> crate::config::Engine {
            crate::config::Engine::MySql
        }
        fn has_schemas(&self) -> bool {
            false
        }
        fn lex_rules(&self) -> LexRules {
            LexRules::MYSQL
        }
        fn quote_ident(&self, ident: &str) -> String {
            format!("`{}`", ident.replace('`', "``"))
        }
        fn quote_string(&self, text: &str) -> String {
            format!("'{}'", text.replace('\'', "\\'"))
        }
        fn quote_bytes(&self, bytes: &[u8]) -> String {
            format!("X'{}'", crate::dialect::hex(bytes))
        }
        fn explain(&self, sql: &str, _: bool) -> String {
            format!("EXPLAIN {sql}")
        }
        fn type_for(&self, _: TypeKind) -> &'static str {
            "text"
        }
        fn estimated_rows_sql(&self, _: &TableRef) -> String {
            String::new()
        }
    }

    fn typed(name: &str, kind: TypeKind) -> ColumnInfo {
        ColumnInfo {
            kind,
            ..column(name)
        }
    }

    /// `id` (key), `name`, `photo` (binary) and `total` (computed), with
    /// rows (1, 'ann''s', NULL, 9) and (2, NULL, 0x00ff, 9).
    fn rows() -> (Vec<ColumnInfo>, ResultStore) {
        use crate::result::{BatchBuilder, Column};
        let columns = vec![
            ColumnInfo {
                primary_key_position: Some(1),
                ..typed("id", TypeKind::Integer)
            },
            column("name"),
            typed("photo", TypeKind::Binary),
            ColumnInfo {
                generation: Some(ColumnGeneration::Stored("1".into())),
                ..typed("total", TypeKind::Integer)
            },
        ];
        let mut store = ResultStore::new(
            vec![
                Column::new("id", "int4", TypeKind::Integer),
                Column::new("name", "text", TypeKind::Text),
                Column::new("photo", "bytea", TypeKind::Binary),
                Column::new("total", "int4", TypeKind::Integer),
            ]
            .into(),
        );
        let mut batch = BatchBuilder::new(4);
        for (id, name, photo) in [
            ("1", Some("ann's"), None),
            ("2", None, Some(&[0u8, 255][..])),
        ] {
            batch.push_text(id);
            match name {
                Some(name) => batch.push_text(name),
                None => batch.push_null(),
            }
            match photo {
                Some(photo) => batch.push_bytes(photo),
                None => batch.push_null(),
            }
            batch.push_text("9");
        }
        store.push(batch.finish());
        (columns, store)
    }

    fn generate(kind: RowSql, dialect: &dyn Dialect, rows_of: &[usize]) -> Result<String> {
        let (columns, store) = rows();
        let t = TableRef::new("shop", "people");
        rows_sql(kind, dialect, &t, &columns, &["id".into()], &store, rows_of)
    }

    #[test]
    fn rows_become_statements_with_their_values() {
        let pg = |kind| generate(kind, &AnsiDialect, &[0, 1]).unwrap();
        assert_eq!(
            pg(RowSql::SelectByKey),
            "SELECT * FROM \"shop\".\"people\" WHERE \"id\" = 1;\n\
             SELECT * FROM \"shop\".\"people\" WHERE \"id\" = 2;\n"
        );
        assert_eq!(
            pg(RowSql::SelectIn),
            "SELECT * FROM \"shop\".\"people\" WHERE \"id\" IN (1, 2);\n"
        );
        // The computed column is left to the server; NULL and bytes are literals.
        assert_eq!(
            pg(RowSql::Insert),
            "INSERT INTO \"shop\".\"people\" (\"id\", \"name\", \"photo\") VALUES (1, 'ann''s', NULL);\n\
             INSERT INTO \"shop\".\"people\" (\"id\", \"name\", \"photo\") VALUES (2, NULL, X'00ff');\n"
        );
        assert_eq!(
            pg(RowSql::Update),
            "UPDATE \"shop\".\"people\" SET \"name\" = 'ann''s', \"photo\" = NULL WHERE \"id\" = 1;\n\
             UPDATE \"shop\".\"people\" SET \"name\" = NULL, \"photo\" = X'00ff' WHERE \"id\" = 2;\n"
        );
        assert_eq!(
            pg(RowSql::Delete),
            "DELETE FROM \"shop\".\"people\" WHERE \"id\" = 1;\n\
             DELETE FROM \"shop\".\"people\" WHERE \"id\" = 2;\n"
        );
        // MySQL's quoting, one row.
        assert_eq!(
            generate(RowSql::Delete, &Backticks, &[1]).unwrap(),
            "DELETE FROM `shop`.`people` WHERE `id` = 2;\n"
        );
        assert_eq!(
            generate(RowSql::Insert, &Backticks, &[0]).unwrap(),
            "INSERT INTO `shop`.`people` (`id`, `name`, `photo`) VALUES (1, 'ann\\'s', NULL);\n"
        );
    }

    #[test]
    fn rows_without_a_key_can_only_be_inserted() {
        let (columns, store) = rows();
        let t = TableRef::new("s", "t");
        for kind in RowSql::ALL {
            let written = rows_sql(kind, &AnsiDialect, &t, &columns, &[], &store, &[0]);
            assert_eq!(written.is_ok(), !kind.needs_key(), "{kind:?}");
        }
    }

    #[test]
    fn a_composite_key_matches_tuples() {
        let (mut columns, store) = rows();
        columns[1].primary_key_position = Some(2);
        let key = ["id".to_string(), "name".to_string()];
        let t = TableRef::new("s", "t");
        let written = |kind| rows_sql(kind, &AnsiDialect, &t, &columns, &key, &store, &[0, 1]);
        assert_eq!(
            written(RowSql::SelectIn).unwrap(),
            "SELECT * FROM \"s\".\"t\" WHERE (\"id\", \"name\") IN ((1, 'ann''s'), (2, NULL));\n"
        );
        // A NULL in the key is matched with IS NULL.
        assert!(
            written(RowSql::Delete)
                .unwrap()
                .ends_with("WHERE \"id\" = 2 AND \"name\" IS NULL;\n")
        );
    }

    #[test]
    fn a_postgresql_routine_is_called_with_a_parameter_per_value() {
        let call = |kind, arguments| call_routine(&AnsiDialect, "shop", "f", kind, arguments);
        assert_eq!(
            call(RoutineKind::Function, "p_customer_id bigint"),
            "SELECT * FROM \"shop\".\"f\"(:p_customer_id);\n"
        );
        // OUT arguments are the result of a function, NULL in a procedure's call.
        let arguments = "a integer, OUT b text, INOUT c numeric(10,2), VARIADIC d integer[]";
        assert_eq!(
            call(RoutineKind::Function, arguments),
            "SELECT * FROM \"shop\".\"f\"(:a, :c, :d);\n"
        );
        assert_eq!(
            call(RoutineKind::Procedure, arguments),
            "CALL \"shop\".\"f\"(:a, NULL, :c, :d);\n"
        );
        // Unnamed arguments, types of several words, defaults, quoted names.
        assert_eq!(
            call(
                RoutineKind::Function,
                "integer, double precision, \"My, \"\"arg\"\" \" text DEFAULT 'a, b'::text, \
                 at timestamp with time zone"
            ),
            "SELECT * FROM \"shop\".\"f\"(:arg1, :arg2, :My___arg__, :at);\n"
        );
        assert_eq!(call(RoutineKind::Procedure, ""), "CALL \"shop\".\"f\"();\n");
        let found = params(&call(RoutineKind::Procedure, "p_older_than interval"));
        assert_eq!(found, [":p_older_than"]);
    }

    #[test]
    fn a_mysql_procedure_gives_variables_for_what_it_returns() {
        let call = |kind, arguments| call_routine(&Backticks, "kerno", "p", kind, arguments);
        assert_eq!(
            call(
                RoutineKind::Procedure,
                "IN p_customer_id bigint unsigned, OUT p_count int"
            ),
            "CALL `kerno`.`p`(:p_customer_id, @p_count);\nSELECT @p_count;\n"
        );
        assert_eq!(
            call(RoutineKind::Procedure, "INOUT total decimal(10,2)"),
            "SET @total = :total;\nCALL `kerno`.`p`(@total);\nSELECT @total;\n"
        );
        assert_eq!(
            call(RoutineKind::Function, "p_customer_id bigint unsigned"),
            "SELECT `kerno`.`p`(:p_customer_id);\n"
        );
    }
}
