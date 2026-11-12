//! What differs between engines in the SQL Kerno writes.

use crate::config::Engine;
use crate::error::{Error, Result};
use crate::plan::Plan;
use crate::result::TypeKind;
use crate::schema::{ColumnGeneration, ColumnInfo, ObjectRef, RelationKind, RoutineKind, TableRef};
use crate::sql::LexRules;
use crate::sql::edits::CellValue;
use crate::sql::params::is_number;
use crate::table_draft::{ColumnDraft, ForeignKeyDraft, TableChange, TableDraft, present};

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

    /// `expr` as text, for matching a pattern against a column of any
    /// type. The default is the standard cast to a character string of no
    /// given length.
    fn cast_to_text(&self, expr: &str) -> String {
        format!("CAST({expr} AS CHAR)")
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

    /// The statements that create the table `draft` describes: `CREATE
    /// TABLE` with its primary and foreign keys, then its indexes, then its
    /// comments. Types and defaults are written as typed: they are SQL the
    /// user wrote, shown before anything runs.
    fn create_table_sql(&self, draft: &TableDraft) -> Result<Vec<String>> {
        Ok(default_create_table_sql(self, draft))
    }

    /// The statements that apply `changes` (see
    /// [`crate::table_draft::changes`]) to `table`, in order. The default is
    /// one standard `ALTER TABLE` per action, `CREATE INDEX` / `DROP INDEX`,
    /// `COMMENT ON`, and identity columns for auto-increment.
    fn alter_table_sql(&self, table: &TableRef, changes: &[TableChange]) -> Result<Vec<String>> {
        default_alter_table_sql(self, table, changes)
    }
}

/// `("a", "b")`: names quoted, in a parenthesised list.
pub fn column_list<D: Dialect + ?Sized>(dialect: &D, names: &[String]) -> String {
    let names: Vec<String> = names.iter().map(|n| dialect.quote_ident(n)).collect();
    format!("({})", names.join(", "))
}

/// `CONSTRAINT n FOREIGN KEY (…) REFERENCES s.t (…) ON UPDATE … ON DELETE …`.
pub fn foreign_key_sql<D: Dialect + ?Sized>(dialect: &D, key: &ForeignKeyDraft) -> String {
    format!(
        "CONSTRAINT {} FOREIGN KEY {} REFERENCES {} {} ON UPDATE {} ON DELETE {}",
        dialect.quote_ident(&key.name),
        column_list(dialect, &key.columns),
        dialect.table_name(&key.referenced_table),
        column_list(dialect, &key.referenced_columns),
        key.on_update,
        key.on_delete
    )
}

/// `name type [DEFAULT e] [GENERATED BY DEFAULT AS IDENTITY] [NOT NULL]`.
fn column_sql<D: Dialect + ?Sized>(dialect: &D, column: &ColumnDraft) -> String {
    let mut sql = format!(
        "{} {}",
        dialect.quote_ident(&column.name),
        column.data_type.trim()
    );
    if column.auto_increment {
        sql.push_str(" GENERATED BY DEFAULT AS IDENTITY");
    } else if let Some(default) = present(&column.default) {
        sql.push_str(&format!(" DEFAULT {default}"));
    }
    if !column.nullable {
        sql.push_str(" NOT NULL");
    }
    sql
}

/// The literal of a comment for `COMMENT ON`, `NULL` to remove it.
fn comment_literal<D: Dialect + ?Sized>(dialect: &D, comment: &Option<String>) -> String {
    present(comment).map_or_else(|| "NULL".into(), |text| dialect.quote_string(text))
}

fn column_comment_sql<D: Dialect + ?Sized>(
    dialect: &D,
    table: &TableRef,
    column: &str,
    comment: &Option<String>,
) -> String {
    format!(
        "COMMENT ON COLUMN {}.{} IS {}",
        dialect.table_name(table),
        dialect.quote_ident(column),
        comment_literal(dialect, comment)
    )
}

fn create_index_sql<D: Dialect + ?Sized>(
    dialect: &D,
    table: &TableRef,
    index: &crate::table_draft::IndexDraft,
) -> String {
    format!(
        "CREATE {}INDEX {} ON {} {}",
        if index.unique { "UNIQUE " } else { "" },
        dialect.quote_ident(&index.name),
        dialect.table_name(table),
        column_list(dialect, &index.columns)
    )
}

/// What [`Dialect::create_table_sql`] writes by default.
fn default_create_table_sql<D: Dialect + ?Sized>(dialect: &D, draft: &TableDraft) -> Vec<String> {
    let table = &draft.table;
    let mut items: Vec<String> = draft
        .columns
        .iter()
        .map(|column| column_sql(dialect, column))
        .collect();
    if !draft.primary_key.is_empty() {
        items.push(format!(
            "PRIMARY KEY {}",
            column_list(dialect, &draft.primary_key)
        ));
    }
    items.extend(
        draft
            .foreign_keys
            .iter()
            .map(|k| foreign_key_sql(dialect, k)),
    );
    let mut statements = vec![format!(
        "CREATE TABLE {} (\n    {}\n)",
        dialect.table_name(table),
        items.join(",\n    ")
    )];
    statements.extend(
        draft
            .indexes
            .iter()
            .map(|index| create_index_sql(dialect, table, index)),
    );
    if present(&draft.comment).is_some() {
        statements.push(format!(
            "COMMENT ON TABLE {} IS {}",
            dialect.table_name(table),
            comment_literal(dialect, &draft.comment)
        ));
    }
    for column in draft
        .columns
        .iter()
        .filter(|c| present(&c.comment).is_some())
    {
        statements.push(column_comment_sql(
            dialect,
            table,
            &column.name,
            &column.comment,
        ));
    }
    statements
}

/// What [`Dialect::alter_table_sql`] writes by default.
fn default_alter_table_sql<D: Dialect + ?Sized>(
    dialect: &D,
    table: &TableRef,
    changes: &[TableChange],
) -> Result<Vec<String>> {
    let name = dialect.table_name(table);
    let alter = |action: String| format!("ALTER TABLE {name} {action}");
    let q = |ident: &str| dialect.quote_ident(ident);
    let mut statements = Vec::new();
    for change in changes {
        match change {
            TableChange::RenameTable { to } => {
                statements.push(alter(format!("RENAME TO {}", q(to))));
            }
            TableChange::SetTableComment(comment) => statements.push(format!(
                "COMMENT ON TABLE {name} IS {}",
                comment_literal(dialect, comment)
            )),
            TableChange::DropForeignKey(key) | TableChange::DropPrimaryKey(key) => {
                statements.push(alter(format!("DROP CONSTRAINT {}", q(key))));
            }
            TableChange::DropIndex(index) => {
                statements.push(dialect.drop_sql(&ObjectRef::Index {
                    table: table.clone(),
                    name: index.clone(),
                })?)
            }
            TableChange::DropColumn(column) => {
                statements.push(alter(format!("DROP COLUMN {}", q(column))));
            }
            TableChange::RenameColumn { from, to } => {
                statements.push(alter(format!("RENAME COLUMN {} TO {}", q(from), q(to))));
            }
            TableChange::AddColumn(column) => {
                statements.push(alter(format!("ADD COLUMN {}", column_sql(dialect, column))));
                if present(&column.comment).is_some() {
                    statements.push(column_comment_sql(
                        dialect,
                        table,
                        &column.name,
                        &column.comment,
                    ));
                }
            }
            TableChange::AlterColumn {
                name: column,
                from,
                to,
            } => {
                let was = ColumnDraft::from(from);
                let alter_column =
                    |action: &str| alter(format!("ALTER COLUMN {} {action}", q(column)));
                // Identity off first: the default and NULL it may allow
                // come after. A `serial` is a default on a sequence.
                if was.auto_increment && !to.auto_increment {
                    statements.push(alter_column(
                        if from.generation == Some(ColumnGeneration::AutoIncrement) {
                            "DROP DEFAULT"
                        } else {
                            "DROP IDENTITY"
                        },
                    ));
                }
                if !was
                    .data_type
                    .trim()
                    .eq_ignore_ascii_case(to.data_type.trim())
                {
                    statements.push(alter_column(&format!("TYPE {}", to.data_type.trim())));
                }
                if to.auto_increment {
                    // An identity column has no default.
                    if !was.auto_increment && present(&was.default).is_some() {
                        statements.push(alter_column("DROP DEFAULT"));
                    }
                } else if present(&was.default) != present(&to.default) {
                    statements.push(alter_column(&match present(&to.default) {
                        Some(default) => format!("SET DEFAULT {default}"),
                        None => "DROP DEFAULT".into(),
                    }));
                }
                if was.nullable != to.nullable {
                    statements.push(alter_column(if to.nullable {
                        "DROP NOT NULL"
                    } else {
                        "SET NOT NULL"
                    }));
                }
                // After NOT NULL, which an identity needs.
                if !was.auto_increment && to.auto_increment {
                    statements.push(alter_column("ADD GENERATED BY DEFAULT AS IDENTITY"));
                }
                if present(&was.comment) != present(&to.comment) {
                    statements.push(column_comment_sql(dialect, table, column, &to.comment));
                }
            }
            TableChange::AddPrimaryKey(columns) => statements.push(alter(format!(
                "ADD PRIMARY KEY {}",
                column_list(dialect, columns)
            ))),
            TableChange::AddIndex(index) => {
                statements.push(create_index_sql(dialect, table, index));
            }
            TableChange::AddForeignKey(key) => {
                statements.push(alter(format!("ADD {}", foreign_key_sql(dialect, key))));
            }
        }
    }
    Ok(statements)
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
            on_update: None,
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

    fn draft_column(name: &str, data_type: &str, nullable: bool) -> ColumnDraft {
        ColumnDraft {
            original: None,
            name: name.into(),
            data_type: data_type.into(),
            nullable,
            default: None,
            comment: None,
            auto_increment: false,
            on_update: None,
        }
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn foreign_key() -> ForeignKeyDraft {
        ForeignKeyDraft {
            original: None,
            name: "order customer".into(),
            columns: strings(&["customer_id"]),
            referenced_table: TableRef::new("s", "customers"),
            referenced_columns: strings(&["id"]),
            on_update: "NO ACTION".into(),
            on_delete: "SET NULL".into(),
        }
    }

    #[test]
    fn create_table_writes_keys_inline_then_indexes_and_comments() {
        let mut draft = TableDraft::new(TableRef::new("s", "order items"));
        draft.comment = Some("it's ours".into());
        draft.columns.push(ColumnDraft {
            default: Some("'new'".into()),
            comment: Some("state".into()),
            ..draft_column("status", "varchar(20)", false)
        });
        draft.columns.push(ColumnDraft {
            default: Some("   ".into()),
            ..draft_column("customer_id", "integer", true)
        });
        draft.indexes = vec![crate::table_draft::IndexDraft {
            original: None,
            name: "by status".into(),
            columns: strings(&["status", "customer_id"]),
            unique: true,
        }];
        draft.foreign_keys = vec![foreign_key()];
        assert_eq!(
            AnsiDialect.create_table_sql(&draft).unwrap(),
            strings(&[
                "CREATE TABLE \"s\".\"order items\" (\n    \
                 \"id\" bigint GENERATED BY DEFAULT AS IDENTITY NOT NULL,\n    \
                 \"status\" varchar(20) DEFAULT 'new' NOT NULL,\n    \
                 \"customer_id\" integer,\n    \
                 PRIMARY KEY (\"id\"),\n    \
                 CONSTRAINT \"order customer\" FOREIGN KEY (\"customer_id\") \
                 REFERENCES \"s\".\"customers\" (\"id\") ON UPDATE NO ACTION ON DELETE SET NULL\n)",
                "CREATE UNIQUE INDEX \"by status\" ON \"s\".\"order items\" (\"status\", \"customer_id\")",
                "COMMENT ON TABLE \"s\".\"order items\" IS 'it''s ours'",
                "COMMENT ON COLUMN \"s\".\"order items\".\"status\" IS 'state'",
            ])
        );
        // No key, no index, no comment: the CREATE TABLE alone.
        let bare = TableDraft {
            columns: vec![draft_column("a", "text", true)],
            primary_key: Vec::new(),
            ..TableDraft::new(TableRef::new("s", "t"))
        };
        assert_eq!(
            AnsiDialect.create_table_sql(&bare).unwrap(),
            strings(&["CREATE TABLE \"s\".\"t\" (\n    \"a\" text\n)"])
        );
    }

    fn info(name: &str, data_type: &str, nullable: bool) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: data_type.into(),
            nullable,
            ..column(TypeKind::Integer)
        }
    }

    /// The statements one `AlterColumn` from `from` to `to` gives.
    fn alter_column(from: &ColumnInfo, to: ColumnDraft) -> Vec<String> {
        AnsiDialect
            .alter_table_sql(
                &TableRef::new("s", "t"),
                &[TableChange::AlterColumn {
                    name: to.name.clone(),
                    from: from.clone(),
                    to,
                }],
            )
            .unwrap()
    }

    #[test]
    fn alter_table_writes_one_statement_per_action_in_order() {
        let t = TableRef::new("s", "t");
        let index = crate::table_draft::IndexDraft {
            original: None,
            name: "i".into(),
            columns: strings(&["a"]),
            unique: false,
        };
        let changes = [
            TableChange::DropForeignKey("fk".into()),
            TableChange::DropIndex("old index".into()),
            TableChange::DropPrimaryKey("t_pkey".into()),
            TableChange::DropColumn("gone".into()),
            TableChange::RenameColumn {
                from: "a".into(),
                to: "b".into(),
            },
            TableChange::AddColumn(ColumnDraft {
                comment: Some("new".into()),
                default: Some("0".into()),
                ..draft_column("n", "integer", false)
            }),
            TableChange::AddPrimaryKey(strings(&["b", "n"])),
            TableChange::AddIndex(index),
            TableChange::AddForeignKey(foreign_key()),
            TableChange::SetTableComment(None),
            TableChange::SetTableComment(Some("x".into())),
            TableChange::RenameTable {
                to: "Orders".into(),
            },
        ];
        assert_eq!(
            AnsiDialect.alter_table_sql(&t, &changes).unwrap(),
            strings(&[
                "ALTER TABLE \"s\".\"t\" DROP CONSTRAINT \"fk\"",
                "DROP INDEX \"s\".\"old index\"",
                "ALTER TABLE \"s\".\"t\" DROP CONSTRAINT \"t_pkey\"",
                "ALTER TABLE \"s\".\"t\" DROP COLUMN \"gone\"",
                "ALTER TABLE \"s\".\"t\" RENAME COLUMN \"a\" TO \"b\"",
                "ALTER TABLE \"s\".\"t\" ADD COLUMN \"n\" integer DEFAULT 0 NOT NULL",
                "COMMENT ON COLUMN \"s\".\"t\".\"n\" IS 'new'",
                "ALTER TABLE \"s\".\"t\" ADD PRIMARY KEY (\"b\", \"n\")",
                "CREATE INDEX \"i\" ON \"s\".\"t\" (\"a\")",
                "ALTER TABLE \"s\".\"t\" ADD CONSTRAINT \"order customer\" FOREIGN KEY (\"customer_id\") \
                 REFERENCES \"s\".\"customers\" (\"id\") ON UPDATE NO ACTION ON DELETE SET NULL",
                "COMMENT ON TABLE \"s\".\"t\" IS NULL",
                "COMMENT ON TABLE \"s\".\"t\" IS 'x'",
                "ALTER TABLE \"s\".\"t\" RENAME TO \"Orders\"",
            ])
        );
        assert_eq!(
            AnsiDialect.alter_table_sql(&t, &[]).unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn altering_a_column_writes_only_what_differs() {
        let prefix = "ALTER TABLE \"s\".\"t\" ALTER COLUMN \"c\" ";
        let with = |actions: &[&str]| -> Vec<String> {
            actions.iter().map(|a| format!("{prefix}{a}")).collect()
        };
        let mut from = info("c", "integer", true);
        from.default = Some("1".into());
        from.comment = Some("old".into());
        let same = ColumnDraft::from(&from);

        assert_eq!(
            alter_column(
                &from,
                ColumnDraft {
                    data_type: "bigint".into(),
                    ..same.clone()
                }
            ),
            with(&["TYPE bigint"])
        );
        assert_eq!(
            alter_column(
                &from,
                ColumnDraft {
                    nullable: false,
                    default: None,
                    ..same.clone()
                }
            ),
            with(&["DROP DEFAULT", "SET NOT NULL"])
        );
        assert_eq!(
            alter_column(
                &from,
                ColumnDraft {
                    default: Some("now()".into()),
                    comment: None,
                    ..same.clone()
                }
            ),
            vec![
                format!("{prefix}SET DEFAULT now()"),
                "COMMENT ON COLUMN \"s\".\"t\".\"c\" IS NULL".to_string(),
            ]
        );
        // Identity on: no default, NOT NULL first.
        assert_eq!(
            alter_column(
                &from,
                ColumnDraft {
                    nullable: false,
                    default: None,
                    auto_increment: true,
                    ..same.clone()
                }
            ),
            with(&[
                "DROP DEFAULT",
                "SET NOT NULL",
                "ADD GENERATED BY DEFAULT AS IDENTITY"
            ])
        );
        // Identity off, then a default and NULL allowed.
        let mut identity = info("c", "integer", false);
        identity.generation = Some(ColumnGeneration::IdentityAlways);
        assert_eq!(
            alter_column(
                &identity,
                ColumnDraft {
                    auto_increment: false,
                    nullable: true,
                    default: Some("0".into()),
                    ..ColumnDraft::from(&identity)
                }
            ),
            with(&["DROP IDENTITY", "SET DEFAULT 0", "DROP NOT NULL"])
        );
        // A serial is a default on a sequence.
        let mut serial = info("c", "integer", false);
        serial.generation = Some(ColumnGeneration::AutoIncrement);
        serial.default = Some("nextval('t_c_seq'::regclass)".into());
        assert_eq!(
            alter_column(
                &serial,
                ColumnDraft {
                    auto_increment: false,
                    ..ColumnDraft::from(&serial)
                }
            ),
            with(&["DROP DEFAULT"])
        );
        // A renamed column is altered under its new name.
        let renamed = ColumnDraft {
            name: "d".into(),
            nullable: false,
            ..same
        };
        assert_eq!(
            alter_column(&from, renamed),
            vec!["ALTER TABLE \"s\".\"t\" ALTER COLUMN \"d\" SET NOT NULL".to_string()]
        );
    }

    #[test]
    fn hex_encodes_every_byte() {
        assert_eq!(hex(&[]), "");
        assert_eq!(hex(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
    }
}
