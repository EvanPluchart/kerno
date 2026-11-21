//! SQLite's SQL: quoting, literals, and the statements Kerno writes. What
//! SQLite cannot do is refused in words that say why.

use kerno_core::dialect::{Dialect, IsolationLevel, column_list, default_literal, hex};
use kerno_core::plan::Plan;
use kerno_core::schema::{ColumnInfo, ObjectRef, RelationKind, TableRef};
use kerno_core::sql::LexRules;
use kerno_core::sql::edits::CellValue;
use kerno_core::table_draft::{ColumnDraft, IndexDraft, TableChange, TableDraft, present};
use kerno_core::{Engine, Error, Result, TypeKind};

/// The one database a session opens: the file itself. Attached databases
/// are not opened yet (D-102).
pub const MAIN: &str = "main";

/// Why no `EXPLAIN ANALYZE`.
pub const NO_ANALYZE: &str = "SQLite has no EXPLAIN ANALYZE: it shows the plan it would use \
     (Explain), never the time each step took.";

/// The SQLite dialect.
#[derive(Debug, Clone, Copy, Default)]
pub struct SqliteDialect;

fn refuse(why: impl Into<String>) -> Error {
    Error::Unsupported(why.into())
}

/// Why a change of the table's definition needs the table rebuilt.
fn rebuild(what: &str) -> Error {
    refuse(format!(
        "SQLite cannot {what} in place: its ALTER TABLE only renames the table, renames, adds and \
         drops columns. Rebuilding the table to do it is still to come in Kerno."
    ))
}

impl SqliteDialect {
    /// `name type [PRIMARY KEY AUTOINCREMENT] [DEFAULT e] [NOT NULL]`.
    fn column_sql(&self, column: &ColumnDraft, rowid_key: bool) -> String {
        let mut sql = format!(
            "{} {}",
            self.quote_ident(&column.name),
            column.data_type.trim()
        );
        if rowid_key {
            sql.push_str(" PRIMARY KEY AUTOINCREMENT");
        } else if let Some(default) = present(&column.default) {
            sql.push_str(&format!(" DEFAULT {default}"));
        }
        if !column.nullable && !rowid_key {
            sql.push_str(" NOT NULL");
        }
        sql
    }

    /// SQLite names the index's database, not the table's:
    /// `CREATE INDEX main.i ON t (…)`.
    fn create_index_sql(&self, table: &TableRef, index: &IndexDraft) -> String {
        format!(
            "CREATE {}INDEX {}.{} ON {} {}",
            if index.unique { "UNIQUE " } else { "" },
            self.quote_ident(&table.schema),
            self.quote_ident(&index.name),
            self.quote_ident(&table.name),
            column_list(self, &index.columns)
        )
    }

    fn qualified(&self, schema: &str, name: &str) -> String {
        format!("{}.{}", self.quote_ident(schema), self.quote_ident(name))
    }
}

impl Dialect for SqliteDialect {
    fn engine(&self) -> Engine {
        Engine::Sqlite
    }

    fn has_schemas(&self) -> bool {
        false
    }

    fn lex_rules(&self) -> LexRules {
        LexRules::SQLITE
    }

    fn quote_ident(&self, ident: &str) -> String {
        format!("\"{}\"", ident.replace('"', "\"\""))
    }

    /// SQLite strings have no escapes: only the quote is doubled.
    fn quote_string(&self, text: &str) -> String {
        format!("'{}'", text.replace('\'', "''"))
    }

    fn quote_bytes(&self, bytes: &[u8]) -> String {
        format!("X'{}'", hex(bytes))
    }

    fn type_for(&self, kind: TypeKind) -> &'static str {
        match kind {
            TypeKind::Integer => "INTEGER",
            TypeKind::Float => "REAL",
            TypeKind::Decimal => "NUMERIC",
            TypeKind::Boolean => "BOOLEAN",
            TypeKind::Binary => "BLOB",
            TypeKind::Date => "DATE",
            TypeKind::Time => "TIME",
            TypeKind::DateTime | TypeKind::DateTimeTz => "DATETIME",
            TypeKind::Json => "JSON",
            TypeKind::Text
            | TypeKind::Interval
            | TypeKind::Uuid
            | TypeKind::Array
            | TypeKind::Enum
            | TypeKind::Bit
            | TypeKind::Other => "TEXT",
        }
    }

    /// `EXPLAIN QUERY PLAN`; there is no `ANALYZE` (see
    /// [`Dialect::explain_unavailable`]).
    fn explain(&self, sql: &str, _analyze: bool) -> String {
        format!("EXPLAIN QUERY PLAN {sql}")
    }

    fn explain_unavailable(&self, analyze: bool) -> Option<String> {
        analyze.then(|| NO_ANALYZE.into())
    }

    fn parse_plan(&self, output: &str, _analyze: bool) -> Result<Plan> {
        crate::plan::parse(output)
    }

    /// The file itself is opened read-only (D-102): nothing to set.
    fn read_only_session_sql(&self) -> Option<&'static str> {
        None
    }

    fn cast_to_text(&self, expr: &str) -> String {
        format!("CAST({expr} AS TEXT)")
    }

    /// `DEFAULT` is no value in SQLite's `INSERT … VALUES` or `UPDATE … SET`:
    /// the column's declared default is written instead (`NULL` without one,
    /// which an `INTEGER PRIMARY KEY` turns into the next row id).
    fn literal(&self, value: &CellValue, column: &ColumnInfo) -> String {
        match value {
            CellValue::Default => match present(&column.default) {
                Some(default) => format!("({default})"),
                None => "NULL".into(),
            },
            value => default_literal(self, value, column),
        }
    }

    fn rename_sql(&self, object: &ObjectRef, new_name: &str) -> Result<String> {
        match object {
            ObjectRef::Relation(table, RelationKind::Table) => Ok(format!(
                "ALTER TABLE {} RENAME TO {}",
                self.table_name(table),
                self.quote_ident(new_name)
            )),
            ObjectRef::Relation(..) => Err(refuse(
                "SQLite cannot rename a view: drop it and create it again under the new name",
            )),
            ObjectRef::Index { .. } => Err(refuse(
                "SQLite cannot rename an index: drop it and create it again under the new name",
            )),
            ObjectRef::Trigger { .. } => Err(refuse(
                "SQLite cannot rename a trigger: drop it and create it again under the new name",
            )),
            ObjectRef::Database(_) | ObjectRef::Schema(_) => Err(refuse(
                "a SQLite database is its file: rename the file in the Finder or a terminal",
            )),
            ObjectRef::Routine { .. }
            | ObjectRef::Sequence(_)
            | ObjectRef::Type(_)
            | ObjectRef::Event(_) => Err(refuse("SQLite has no such kind of object")),
        }
    }

    fn drop_sql(&self, object: &ObjectRef) -> Result<String> {
        match object {
            ObjectRef::Relation(table, RelationKind::View) => {
                Ok(format!("DROP VIEW {}", self.table_name(table)))
            }
            ObjectRef::Relation(table, _) => Ok(format!("DROP TABLE {}", self.table_name(table))),
            ObjectRef::Index { table, name } => Ok(format!(
                "DROP INDEX {}",
                self.qualified(&table.schema, name)
            )),
            ObjectRef::Trigger { table, name } => Ok(format!(
                "DROP TRIGGER {}",
                self.qualified(&table.schema, name)
            )),
            ObjectRef::Database(_) | ObjectRef::Schema(_) => Err(refuse(
                "a SQLite database is its file: delete the file itself to drop it",
            )),
            ObjectRef::Routine { .. }
            | ObjectRef::Sequence(_)
            | ObjectRef::Type(_)
            | ObjectRef::Event(_) => Err(refuse("SQLite has no such kind of object")),
        }
    }

    /// SQLite keeps no estimate unless ANALYZE ran: the rows are counted.
    fn estimated_rows_sql(&self, table: &TableRef) -> String {
        format!("SELECT count(*) FROM {}", self.table_name(table))
    }

    fn cancel_session_sql(&self, _id: &str) -> Result<String> {
        Err(refuse("SQLite has no server sessions to stop"))
    }

    fn terminate_session_sql(&self, _id: &str) -> Result<String> {
        Err(refuse("SQLite has no server sessions to stop"))
    }

    /// SQLite has no `TRUNCATE`: an unqualified `DELETE` empties the table
    /// (and goes through the same confirmations).
    fn truncate_sql(&self, table: &TableRef) -> String {
        format!("DELETE FROM {}", self.table_name(table))
    }

    fn create_schema_sql(&self, _name: &str) -> Result<String> {
        Err(refuse("a SQLite database has no schemas"))
    }

    fn create_database_sql(&self, _name: &str) -> Result<String> {
        Err(refuse(
            "a SQLite database is a file: create one with New file… in the connection dialog",
        ))
    }

    fn create_database_with_sql(
        &self,
        name: &str,
        _options: &kerno_core::dialect::DatabaseOptions,
    ) -> Result<String> {
        self.create_database_sql(name)
    }

    fn create_table_sql(&self, draft: &TableDraft) -> Result<Vec<String>> {
        if present(&draft.comment).is_some()
            || draft.columns.iter().any(|c| present(&c.comment).is_some())
        {
            return Err(refuse("SQLite has no comments on tables or columns"));
        }
        let numbered: Vec<&ColumnDraft> = draft
            .columns
            .iter()
            .filter(|column| column.auto_increment.is_on())
            .collect();
        let rowid_key = match numbered.as_slice() {
            [] => None,
            [column]
                if draft.primary_key == [column.name.clone()]
                    && column.data_type.trim().eq_ignore_ascii_case("INTEGER") =>
            {
                Some(column.name.as_str())
            }
            _ => {
                return Err(refuse(
                    "in SQLite only an INTEGER PRIMARY KEY column numbers its rows: make it the \
                     table's only key column, of type INTEGER",
                ));
            }
        };
        let table = &draft.table;
        let mut items: Vec<String> = draft
            .columns
            .iter()
            .map(|column| self.column_sql(column, rowid_key == Some(column.name.as_str())))
            .collect();
        if !draft.primary_key.is_empty() && rowid_key.is_none() {
            items.push(format!(
                "PRIMARY KEY {}",
                column_list(self, &draft.primary_key)
            ));
        }
        for key in &draft.foreign_keys {
            if key.referenced_table.schema != table.schema {
                return Err(refuse(
                    "a SQLite foreign key refers to a table of the same database",
                ));
            }
            // REFERENCES takes a bare name: the table is in the same database.
            items.push(format!(
                "CONSTRAINT {} FOREIGN KEY {} REFERENCES {} {} ON UPDATE {} ON DELETE {}",
                self.quote_ident(&key.name),
                column_list(self, &key.columns),
                self.quote_ident(&key.referenced_table.name),
                column_list(self, &key.referenced_columns),
                key.on_update,
                key.on_delete
            ));
        }
        let mut statements = vec![format!(
            "CREATE TABLE {} (\n    {}\n)",
            self.table_name(table),
            items.join(",\n    ")
        )];
        statements.extend(
            draft
                .indexes
                .iter()
                .map(|index| self.create_index_sql(table, index)),
        );
        Ok(statements)
    }

    /// What SQLite's `ALTER TABLE` does (rename the table, rename, add and
    /// drop a column), and indexes, which are statements of their own.
    /// Everything else needs the table rebuilt and is refused.
    fn alter_table_sql(&self, table: &TableRef, changes: &[TableChange]) -> Result<Vec<String>> {
        let name = self.table_name(table);
        let alter = |action: String| format!("ALTER TABLE {name} {action}");
        let q = |ident: &str| self.quote_ident(ident);
        let mut statements = Vec::new();
        for change in changes {
            statements.push(match change {
                TableChange::RenameTable { to } => alter(format!("RENAME TO {}", q(to))),
                TableChange::RenameColumn { from, to } => {
                    alter(format!("RENAME COLUMN {} TO {}", q(from), q(to)))
                }
                TableChange::DropColumn(column) => alter(format!("DROP COLUMN {}", q(column))),
                TableChange::AddColumn(column) => {
                    if column.auto_increment.is_on() {
                        return Err(rebuild("add a column that numbers the rows"));
                    }
                    if present(&column.comment).is_some() {
                        return Err(refuse("SQLite has no comments on tables or columns"));
                    }
                    alter(format!("ADD COLUMN {}", self.column_sql(column, false)))
                }
                TableChange::AddIndex(index) => self.create_index_sql(table, index),
                TableChange::DropIndex(index) => {
                    format!("DROP INDEX {}", self.qualified(&table.schema, index))
                }
                TableChange::AlterColumn { name, from, to } => {
                    let was = ColumnDraft::from(from);
                    let what = if !was
                        .data_type
                        .trim()
                        .eq_ignore_ascii_case(to.data_type.trim())
                    {
                        format!("change the type of column “{name}”")
                    } else if was.nullable != to.nullable {
                        format!("change whether column “{name}” takes NULL")
                    } else if present(&was.default) != present(&to.default) {
                        format!("change the default of column “{name}”")
                    } else if present(&was.comment) != present(&to.comment) {
                        return Err(refuse("SQLite has no comments on tables or columns"));
                    } else {
                        format!("change column “{name}”")
                    };
                    return Err(rebuild(&what));
                }
                TableChange::SetTableComment(_) => {
                    return Err(refuse("SQLite has no comments on tables or columns"));
                }
                TableChange::AddForeignKey(_) | TableChange::DropForeignKey(_) => {
                    return Err(rebuild("add or drop a foreign key"));
                }
                TableChange::AddPrimaryKey(_) | TableChange::DropPrimaryKey(_) => {
                    return Err(rebuild("change the primary key"));
                }
                TableChange::MoveColumn { .. } => return Err(rebuild("move a column")),
            });
        }
        Ok(statements)
    }

    /// SQLite's transactions are serializable, and that is the only level.
    fn isolation_levels(&self) -> &'static [IsolationLevel] {
        &[IsolationLevel::Serializable]
    }

    /// Nothing to set: the one level is always in effect.
    fn set_isolation_sql(&self, _level: IsolationLevel) -> String {
        self.isolation_sql().into()
    }

    fn isolation_sql(&self) -> &'static str {
        "SELECT 'SERIALIZABLE'"
    }

    fn begin_sql(&self) -> &'static str {
        "BEGIN"
    }

    fn transactional_ddl(&self) -> bool {
        true
    }

    /// No stored functions, procedures or events: functions come from the
    /// application that opens the file.
    fn has_routines(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kerno_core::schema::ColumnGeneration;
    use kerno_core::table_draft::{AutoIncrement, ForeignKeyDraft};

    fn t() -> TableRef {
        TableRef::new(MAIN, "t")
    }

    fn info(name: &str, data_type: &str, default: Option<&str>) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: data_type.into(),
            kind: crate::types::declared_kind(data_type),
            nullable: true,
            default: default.map(Into::into),
            generation: None,
            on_update: None,
            invisible: false,
            primary_key_position: None,
            comment: None,
            collation: None,
            enum_values: Vec::new(),
        }
    }

    fn draft(name: &str, data_type: &str) -> ColumnDraft {
        ColumnDraft {
            original: None,
            name: name.into(),
            data_type: data_type.into(),
            nullable: true,
            default: None,
            comment: None,
            auto_increment: AutoIncrement::Off,
            on_update: None,
            invisible: false,
        }
    }

    #[test]
    fn quoting_and_literals() {
        let d = SqliteDialect;
        assert_eq!(d.quote_ident("a\"b"), "\"a\"\"b\"");
        assert_eq!(d.quote_string("it's \\n"), "'it''s \\n'");
        assert_eq!(d.quote_bytes(&[0, 255]), "X'00ff'");
        assert_eq!(d.table_name(&t()), "\"main\".\"t\"");
        let text = info("c", "TEXT", Some("'x'"));
        assert_eq!(d.literal(&CellValue::Default, &text), "('x')");
        assert_eq!(
            d.literal(&CellValue::Default, &info("c", "INT", None)),
            "NULL"
        );
        assert_eq!(
            d.literal(&CellValue::Text("12".into()), &info("n", "INTEGER", None)),
            "12"
        );
        assert_eq!(
            d.literal(&CellValue::Bytes(vec![1]), &info("b", "BLOB", None)),
            "X'01'"
        );
        assert_eq!(d.truncate_sql(&t()), "DELETE FROM \"main\".\"t\"");
        assert_eq!(d.explain("SELECT 1", false), "EXPLAIN QUERY PLAN SELECT 1");
        assert_eq!(d.explain_unavailable(true).as_deref(), Some(NO_ANALYZE));
        assert_eq!(d.explain_unavailable(false), None);
        assert_eq!(d.begin_sql(), "BEGIN");
    }

    #[test]
    fn what_can_be_renamed_and_dropped() {
        let d = SqliteDialect;
        let view = ObjectRef::Relation(TableRef::new(MAIN, "v"), RelationKind::View);
        let table = ObjectRef::Relation(t(), RelationKind::Table);
        let index = ObjectRef::Index {
            table: t(),
            name: "i".into(),
        };
        let trigger = ObjectRef::Trigger {
            table: t(),
            name: "tr".into(),
        };
        assert_eq!(
            d.rename_sql(&table, "u").unwrap(),
            "ALTER TABLE \"main\".\"t\" RENAME TO \"u\""
        );
        assert!(
            d.rename_sql(&view, "w")
                .unwrap_err()
                .to_string()
                .contains("drop it")
        );
        assert_eq!(d.drop_sql(&view).unwrap(), "DROP VIEW \"main\".\"v\"");
        assert_eq!(d.drop_sql(&table).unwrap(), "DROP TABLE \"main\".\"t\"");
        assert_eq!(d.drop_sql(&index).unwrap(), "DROP INDEX \"main\".\"i\"");
        assert_eq!(
            d.drop_sql(&trigger).unwrap(),
            "DROP TRIGGER \"main\".\"tr\""
        );
        assert!(d.drop_sql(&ObjectRef::Database(MAIN.into())).is_err());
        assert!(d.create_database_sql("x").is_err());
        assert!(d.create_schema_sql("x").is_err());
    }

    #[test]
    fn create_table_numbers_rows_with_an_integer_primary_key() {
        let d = SqliteDialect;
        let mut id = draft("id", "INTEGER");
        id.auto_increment = AutoIncrement::On;
        let mut name = draft("name", "TEXT");
        name.nullable = false;
        name.default = Some("'x'".into());
        let table = TableDraft {
            table: TableRef::new(MAIN, "kerno_test_t"),
            comment: None,
            columns: vec![id, name, draft("owner", "INTEGER")],
            indexes: vec![IndexDraft {
                original: None,
                name: "kerno_test_t_name".into(),
                columns: vec!["name".into()],
                unique: true,
            }],
            foreign_keys: vec![ForeignKeyDraft {
                original: None,
                name: "owner_fk".into(),
                columns: vec!["owner".into()],
                referenced_table: TableRef::new(MAIN, "people"),
                referenced_columns: vec!["id".into()],
                on_update: "NO ACTION".into(),
                on_delete: "CASCADE".into(),
            }],
            primary_key: vec!["id".into()],
        };
        assert_eq!(
            d.create_table_sql(&table).unwrap(),
            [
                "CREATE TABLE \"main\".\"kerno_test_t\" (\n    \
                 \"id\" INTEGER PRIMARY KEY AUTOINCREMENT,\n    \
                 \"name\" TEXT DEFAULT 'x' NOT NULL,\n    \
                 \"owner\" INTEGER,\n    \
                 CONSTRAINT \"owner_fk\" FOREIGN KEY (\"owner\") REFERENCES \"people\" (\"id\") \
                 ON UPDATE NO ACTION ON DELETE CASCADE\n)",
                "CREATE UNIQUE INDEX \"main\".\"kerno_test_t_name\" ON \"kerno_test_t\" (\"name\")",
            ]
        );
        // Numbering on a TEXT column, or beside another key column: refused.
        let mut text_key = table.clone();
        text_key.columns[0].data_type = "TEXT".into();
        assert!(d.create_table_sql(&text_key).is_err());
        let mut comment = table;
        comment.comment = Some("no".into());
        assert!(
            d.create_table_sql(&comment)
                .unwrap_err()
                .to_string()
                .contains("comments")
        );
    }

    #[test]
    fn alter_does_what_sqlite_does_and_says_why_not_otherwise() {
        let d = SqliteDialect;
        let done = d
            .alter_table_sql(
                &t(),
                &[
                    TableChange::RenameColumn {
                        from: "a".into(),
                        to: "b".into(),
                    },
                    TableChange::AddColumn(draft("c", "TEXT")),
                    TableChange::DropColumn("d".into()),
                    TableChange::RenameTable { to: "u".into() },
                ],
            )
            .unwrap();
        assert_eq!(
            done,
            [
                "ALTER TABLE \"main\".\"t\" RENAME COLUMN \"a\" TO \"b\"",
                "ALTER TABLE \"main\".\"t\" ADD COLUMN \"c\" TEXT",
                "ALTER TABLE \"main\".\"t\" DROP COLUMN \"d\"",
                "ALTER TABLE \"main\".\"t\" RENAME TO \"u\"",
            ]
        );
        let from = ColumnInfo {
            generation: None::<ColumnGeneration>,
            ..info("a", "INTEGER", None)
        };
        let why = |change: TableChange| d.alter_table_sql(&t(), &[change]).unwrap_err().to_string();
        let retyped = why(TableChange::AlterColumn {
            name: "a".into(),
            from: from.clone(),
            to: draft("a", "TEXT"),
        });
        assert!(
            retyped.contains("cannot change the type of column “a” in place"),
            "{retyped}"
        );
        assert!(why(TableChange::DropPrimaryKey("pk".into())).contains("primary key"));
        assert!(why(TableChange::DropForeignKey("fk".into())).contains("foreign key"));
        assert!(why(TableChange::SetTableComment(None)).contains("comments"));
    }
}
