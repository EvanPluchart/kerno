//! What differs between engines in the SQL Kerno writes.

use crate::config::Engine;
use crate::error::{Error, Result};
use crate::plan::Plan;
use crate::result::TypeKind;
use crate::schema::{ColumnInfo, ObjectRef, RelationKind, RoutineKind, TableRef};
use crate::sql::LexRules;
use crate::sql::edits::CellValue;
use crate::sql::params::is_number;

/// The SQL dialect of an engine. Pure functions: no I/O.
pub trait Dialect: Send + Sync {
    /// Engine this dialect belongs to.
    fn engine(&self) -> Engine;

    /// Whether databases contain schemas (PostgreSQL) or tables directly (MySQL).
    fn has_schemas(&self) -> bool;

    /// Lexical rules of the engine.
    fn lex_rules(&self) -> LexRules;

    /// Quotes an identifier, always, so reserved words and odd characters are safe.
    fn quote_ident(&self, ident: &str) -> String;

    /// A string literal holding `text`.
    fn quote_string(&self, text: &str) -> String;

    /// A literal for raw bytes.
    fn quote_bytes(&self, bytes: &[u8]) -> String;

    /// `schema.name`, quoted.
    fn table_name(&self, table: &TableRef) -> String {
        format!(
            "{}.{}",
            self.quote_ident(&table.schema),
            self.quote_ident(&table.name)
        )
    }

    /// The clause that limits a `SELECT` to `limit` rows starting at `offset`.
    fn limit_offset(&self, limit: u64, offset: u64) -> String {
        if offset == 0 {
            format!("LIMIT {limit}")
        } else {
            format!("LIMIT {limit} OFFSET {offset}")
        }
    }

    /// Wraps `sql` so the server returns its plan as JSON. With `analyze`
    /// the statement is executed and actual timings are included.
    fn explain(&self, sql: &str, analyze: bool) -> String;

    /// Reads what the statement written by [`Dialect::explain`] returned:
    /// `output` is the text of its single cell, `analyze` the flag it was
    /// asked with. An error means the output has a shape this dialect does
    /// not know; the raw text is then all there is to show.
    fn parse_plan(&self, output: &str, analyze: bool) -> Result<Plan> {
        let _ = (output, analyze);
        Err(Error::Unsupported("reading query plans".into()))
    }

    /// A literal holding `value`, to store in `column` or compare with it.
    ///
    /// Text is written as a string literal and left to the server to
    /// convert to the column's type, except that plain numbers for numeric
    /// columns and the usual spellings of booleans are written bare.
    /// Dialects override this for types whose literal needs another form.
    fn literal(&self, value: &CellValue, column: &ColumnInfo) -> String {
        default_literal(self, value, column)
    }

    /// An `INSERT` that gives every column its default.
    fn insert_defaults(&self, table: &TableRef) -> String {
        format!("INSERT INTO {} DEFAULT VALUES", self.table_name(table))
    }

    /// The statement that renames `object` to `new_name`, in the schema it
    /// is in. `Unsupported` when the engine cannot rename that kind of
    /// object. The default is the standard `ALTER <kind> <name> RENAME TO`.
    fn rename_sql(&self, object: &ObjectRef, new_name: &str) -> Result<String> {
        default_rename_sql(self, object, new_name)
    }

    /// The statement that drops `object`, and only it: no `CASCADE` and no
    /// `IF EXISTS`, so the server says what depends on it or that it is
    /// gone. `Unsupported` when the engine has no such kind of object.
    fn drop_sql(&self, object: &ObjectRef) -> Result<String> {
        default_drop_sql(self, object)
    }

    /// The statement that removes every row of `table`.
    fn truncate_sql(&self, table: &TableRef) -> String {
        format!("TRUNCATE TABLE {}", self.table_name(table))
    }

    /// The statement that creates the schema `name` in the connected
    /// database. `Unsupported` on engines whose databases have no schemas.
    fn create_schema_sql(&self, name: &str) -> Result<String> {
        Ok(format!("CREATE SCHEMA {}", self.quote_ident(name)))
    }

    /// The statement that creates the database `name` with the server's
    /// defaults.
    fn create_database_sql(&self, name: &str) -> Result<String> {
        Ok(format!("CREATE DATABASE {}", self.quote_ident(name)))
    }
}

/// `<kind> <name>` as standard `ALTER` and `DROP` statements name `object`.
///
/// The argument list of a routine is the SQL the server printed for it
/// (see [`ObjectRef::Routine`]) and is written as it is.
fn object_target<D: Dialect + ?Sized>(dialect: &D, object: &ObjectRef) -> Result<String> {
    let qualified = |schema: &str, name: &str| {
        format!(
            "{}.{}",
            dialect.quote_ident(schema),
            dialect.quote_ident(name)
        )
    };
    Ok(match object {
        ObjectRef::Relation(table, kind) => {
            let keyword = match kind {
                RelationKind::Table | RelationKind::PartitionedTable => "TABLE",
                RelationKind::ForeignTable => "FOREIGN TABLE",
                RelationKind::View => "VIEW",
                RelationKind::MaterializedView => "MATERIALIZED VIEW",
            };
            format!("{keyword} {}", dialect.table_name(table))
        }
        ObjectRef::Routine {
            schema,
            name,
            kind,
            identity_arguments,
        } => {
            let keyword = match kind {
                RoutineKind::Function | RoutineKind::Window => "FUNCTION",
                RoutineKind::Procedure => "PROCEDURE",
                RoutineKind::Aggregate => "AGGREGATE",
            };
            format!(
                "{keyword} {}({identity_arguments})",
                qualified(schema, name)
            )
        }
        ObjectRef::Sequence(sequence) => format!("SEQUENCE {}", dialect.table_name(sequence)),
        ObjectRef::Type(user_type) => format!("TYPE {}", dialect.table_name(user_type)),
        ObjectRef::Trigger { table, name } => {
            format!("TRIGGER {}", qualified(&table.schema, name))
        }
        ObjectRef::Index { table, name } => format!("INDEX {}", qualified(&table.schema, name)),
        ObjectRef::Event(_) => return Err(Error::Unsupported("scheduled events".into())),
        ObjectRef::Schema(name) => format!("SCHEMA {}", dialect.quote_ident(name)),
        ObjectRef::Database(name) => format!("DATABASE {}", dialect.quote_ident(name)),
    })
}

/// The statement [`Dialect::rename_sql`] writes by default. A dialect that
/// overrides `rename_sql` for a few kinds of object calls this for the rest.
pub fn default_rename_sql<D: Dialect + ?Sized>(
    dialect: &D,
    object: &ObjectRef,
    new_name: &str,
) -> Result<String> {
    Ok(format!(
        "ALTER {} RENAME TO {}",
        object_target(dialect, object)?,
        dialect.quote_ident(new_name)
    ))
}

/// The statement [`Dialect::drop_sql`] writes by default. A dialect that
/// overrides `drop_sql` for a few kinds of object calls this for the rest.
pub fn default_drop_sql<D: Dialect + ?Sized>(dialect: &D, object: &ObjectRef) -> Result<String> {
    Ok(format!("DROP {}", object_target(dialect, object)?))
}

/// The literal [`Dialect::literal`] writes by default. A dialect that
/// overrides `literal` for a few types calls this for the rest.
pub fn default_literal<D: Dialect + ?Sized>(
    dialect: &D,
    value: &CellValue,
    column: &ColumnInfo,
) -> String {
    let is =
        |spellings: [&str; 2], text: &str| spellings.iter().any(|s| text.eq_ignore_ascii_case(s));
    match value {
        CellValue::Null => "NULL".into(),
        CellValue::Default => "DEFAULT".into(),
        CellValue::Bytes(bytes) => dialect.quote_bytes(bytes),
        CellValue::Text(text) => match column.kind {
            kind if kind.is_numeric() && is_number(text) => text.trim().to_string(),
            TypeKind::Boolean if is(["t", "true"], text) => "TRUE".into(),
            TypeKind::Boolean if is(["f", "false"], text) => "FALSE".into(),
            _ => dialect.quote_string(text),
        },
    }
}

/// Lower-case hex of `bytes`, for byte literals.
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

/// A plain dialect for unit tests of code that only needs quoting.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    pub(crate) struct AnsiDialect;

    impl Dialect for AnsiDialect {
        fn engine(&self) -> Engine {
            Engine::Postgres
        }
        fn has_schemas(&self) -> bool {
            true
        }
        fn lex_rules(&self) -> LexRules {
            LexRules::POSTGRES
        }
        fn quote_ident(&self, ident: &str) -> String {
            format!("\"{}\"", ident.replace('"', "\"\""))
        }
        fn quote_string(&self, text: &str) -> String {
            format!("'{}'", text.replace('\'', "''"))
        }
        fn quote_bytes(&self, bytes: &[u8]) -> String {
            format!("X'{}'", hex(bytes))
        }
        fn explain(&self, sql: &str, _: bool) -> String {
            format!("EXPLAIN {sql}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::AnsiDialect;
    use super::*;

    fn column(kind: TypeKind) -> ColumnInfo {
        ColumnInfo {
            name: "c".into(),
            data_type: String::new(),
            kind,
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
    fn default_literals_by_column_kind() {
        let d = AnsiDialect;
        let text = |value: &str| CellValue::Text(value.into());
        assert_eq!(d.literal(&CellValue::Null, &column(TypeKind::Text)), "NULL");
        assert_eq!(
            d.literal(&CellValue::Default, &column(TypeKind::Text)),
            "DEFAULT"
        );
        assert_eq!(
            d.literal(&CellValue::Bytes(vec![1, 255]), &column(TypeKind::Binary)),
            "X'01ff'"
        );
        // Numbers are bare only in numeric columns, and only when plain.
        assert_eq!(d.literal(&text("42"), &column(TypeKind::Integer)), "42");
        assert_eq!(
            d.literal(&text("-1.5e3"), &column(TypeKind::Float)),
            "-1.5e3"
        );
        assert_eq!(d.literal(&text("42"), &column(TypeKind::Text)), "'42'");
        assert_eq!(
            d.literal(&text("Infinity"), &column(TypeKind::Float)),
            "'Infinity'"
        );
        assert_eq!(d.literal(&text(""), &column(TypeKind::Integer)), "''");
        // Booleans in the forms the servers print them.
        assert_eq!(d.literal(&text("t"), &column(TypeKind::Boolean)), "TRUE");
        assert_eq!(
            d.literal(&text("False"), &column(TypeKind::Boolean)),
            "FALSE"
        );
        assert_eq!(d.literal(&text("yes"), &column(TypeKind::Boolean)), "'yes'");
        assert_eq!(d.literal(&text("t"), &column(TypeKind::Text)), "'t'");
        // Everything else is a string for the server to convert.
        assert_eq!(
            d.literal(&text("{1,2}"), &column(TypeKind::Array)),
            "'{1,2}'"
        );
        assert_eq!(
            d.literal(&text("2024-02-29"), &column(TypeKind::Date)),
            "'2024-02-29'"
        );
        assert_eq!(
            d.literal(&text("{\"a\": 'x'}"), &column(TypeKind::Json)),
            "'{\"a\": ''x''}'"
        );
    }

    #[test]
    fn insert_defaults_statement() {
        assert_eq!(
            AnsiDialect.insert_defaults(&TableRef::new("s", "t")),
            "INSERT INTO \"s\".\"t\" DEFAULT VALUES"
        );
    }

    fn routine(kind: RoutineKind, identity_arguments: &str) -> ObjectRef {
        ObjectRef::Routine {
            schema: "s".into(),
            name: "f".into(),
            kind,
            identity_arguments: identity_arguments.into(),
        }
    }

    /// One object of every kind, with the words that name it in `ALTER`
    /// and `DROP`.
    fn objects() -> Vec<(ObjectRef, &'static str)> {
        let t = || TableRef::new("s", "t");
        vec![
            (
                ObjectRef::Relation(t(), RelationKind::Table),
                "TABLE \"s\".\"t\"",
            ),
            (
                ObjectRef::Relation(t(), RelationKind::PartitionedTable),
                "TABLE \"s\".\"t\"",
            ),
            (
                ObjectRef::Relation(t(), RelationKind::ForeignTable),
                "FOREIGN TABLE \"s\".\"t\"",
            ),
            (
                ObjectRef::Relation(t(), RelationKind::View),
                "VIEW \"s\".\"t\"",
            ),
            (
                ObjectRef::Relation(t(), RelationKind::MaterializedView),
                "MATERIALIZED VIEW \"s\".\"t\"",
            ),
            (
                routine(RoutineKind::Function, "integer, text"),
                "FUNCTION \"s\".\"f\"(integer, text)",
            ),
            (routine(RoutineKind::Function, ""), "FUNCTION \"s\".\"f\"()"),
            (
                routine(RoutineKind::Window, "integer"),
                "FUNCTION \"s\".\"f\"(integer)",
            ),
            (
                routine(RoutineKind::Procedure, ""),
                "PROCEDURE \"s\".\"f\"()",
            ),
            (
                routine(RoutineKind::Aggregate, "integer"),
                "AGGREGATE \"s\".\"f\"(integer)",
            ),
            (ObjectRef::Sequence(t()), "SEQUENCE \"s\".\"t\""),
            (ObjectRef::Type(t()), "TYPE \"s\".\"t\""),
            (
                ObjectRef::Trigger {
                    table: t(),
                    name: "trg".into(),
                },
                "TRIGGER \"s\".\"trg\"",
            ),
            (
                ObjectRef::Index {
                    table: t(),
                    name: "t idx".into(),
                },
                "INDEX \"s\".\"t idx\"",
            ),
            (
                ObjectRef::Schema("my schema".into()),
                "SCHEMA \"my schema\"",
            ),
            (ObjectRef::Database("db".into()), "DATABASE \"db\""),
        ]
    }

    #[test]
    fn rename_and_drop_name_every_kind_of_object() {
        let d = AnsiDialect;
        for (object, target) in objects() {
            assert_eq!(
                d.rename_sql(&object, "Weird \"name\"").unwrap(),
                format!("ALTER {target} RENAME TO \"Weird \"\"name\"\"\""),
                "{object:?}"
            );
            assert_eq!(
                d.drop_sql(&object).unwrap(),
                format!("DROP {target}"),
                "{object:?}"
            );
        }
        // A new name is an identifier, never SQL.
        assert_eq!(
            d.rename_sql(&ObjectRef::Schema("a".into()), "b; DROP SCHEMA a")
                .unwrap(),
            "ALTER SCHEMA \"a\" RENAME TO \"b; DROP SCHEMA a\""
        );
    }

    #[test]
    fn scheduled_events_are_not_standard() {
        let event = ObjectRef::Event(TableRef::new("s", "e"));
        for result in [
            AnsiDialect.rename_sql(&event, "x"),
            AnsiDialect.drop_sql(&event),
        ] {
            let error = result.unwrap_err();
            assert!(matches!(error, Error::Unsupported(_)), "{error:?}");
            assert_eq!(
                error.to_string(),
                "not supported by this database: scheduled events"
            );
        }
    }

    #[test]
    fn truncate_and_create_statements() {
        let d = AnsiDialect;
        assert_eq!(
            d.truncate_sql(&TableRef::new("s", "order items")),
            "TRUNCATE TABLE \"s\".\"order items\""
        );
        assert_eq!(
            d.create_schema_sql("my \"schema\"").unwrap(),
            "CREATE SCHEMA \"my \"\"schema\"\"\""
        );
        assert_eq!(
            d.create_database_sql("my db").unwrap(),
            "CREATE DATABASE \"my db\""
        );
    }

    #[test]
    fn hex_encodes_every_byte() {
        assert_eq!(hex(&[]), "");
        assert_eq!(hex(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
    }
}
