//! The SQL dialect of MySQL and MariaDB.

use std::sync::atomic::{AtomicBool, Ordering};

use kerno_core::dialect::{Dialect, column_list, default_literal, foreign_key_sql, hex};
use kerno_core::maintenance::{Maintenance, MaintenanceSql, TruncateOptions};
use kerno_core::schema::{
    ColumnGeneration, ColumnInfo, ObjectRef, RelationKind, RoutineKind, TableRef,
};
use kerno_core::sql::params::is_number;
use kerno_core::sql::{CellValue, LexRules};
use kerno_core::table_draft::{ColumnDraft, IndexDraft, TableChange, TableDraft, present};
use kerno_core::{Engine, Error, Plan, Result, TypeKind};

/// The MySQL / MariaDB dialect. The two engines differ only in how a plan
/// with actual timings is asked for (see [`Dialect::explain`]).
#[derive(Debug)]
pub struct MysqlDialect {
    engine: Engine,
    /// The session's `sql_mode` contains `NO_BACKSLASH_ESCAPES`: a backslash
    /// in a string literal is then an ordinary character. A session keeps
    /// this in step with the server (see `MysqlSession`) so that the lexer
    /// splits what the user types correctly. Literals Kerno writes do not
    /// depend on it (see `quote_string`).
    no_backslash_escapes: AtomicBool,
}

impl MysqlDialect {
    /// The dialect of `engine` under the default `sql_mode`, where the
    /// backslash is the escape character of string literals.
    pub fn new(engine: Engine) -> Self {
        Self {
            engine,
            no_backslash_escapes: AtomicBool::new(false),
        }
    }

    /// Records whether the session runs with `NO_BACKSLASH_ESCAPES`.
    pub(crate) fn set_no_backslash_escapes(&self, on: bool) {
        self.no_backslash_escapes.store(on, Ordering::Release);
    }

    fn backslash_escapes(&self) -> bool {
        !self.no_backslash_escapes.load(Ordering::Acquire)
    }

    /// Sequences exist on MariaDB only.
    fn require_sequences(&self) -> Result<()> {
        if self.engine == Engine::MariaDb {
            Ok(())
        } else {
            Err(unsupported("sequences"))
        }
    }
}

fn unsupported(what: &str) -> Error {
    Error::Unsupported(what.into())
}

impl MysqlDialect {
    /// The full definition of `column`, as `CREATE TABLE`, `ADD`, `MODIFY`
    /// and `CHANGE COLUMN` take it:
    /// `name type [COLLATE c] NOT NULL|NULL [DEFAULT e] [AUTO_INCREMENT] [ON UPDATE e] [INVISIBLE]
    /// [COMMENT '…']`.
    ///
    /// `from` is the column on the server, for `MODIFY` and `CHANGE`: these
    /// replace the whole definition, so a collation other than the table's
    /// is carried over while the type stays, and a generated column is
    /// refused (its expression would be lost).
    fn column_sql(&self, column: &ColumnDraft, from: Option<&ColumnInfo>) -> Result<String> {
        let data_type = column.data_type.trim();
        let mut sql = format!("{} {data_type}", self.quote_ident(&column.name));
        if let Some(from) = from {
            if matches!(
                from.generation,
                Some(ColumnGeneration::Stored(_) | ColumnGeneration::Virtual(_))
            ) {
                return Err(unsupported(&format!(
                    "changing the generated column {} from the table form; \
                     write the ALTER TABLE statement instead",
                    self.quote_ident(&from.name)
                )));
            }
            if let Some(collation) = &from.collation
                && from.data_type.eq_ignore_ascii_case(data_type)
            {
                sql.push_str(&format!(" COLLATE {collation}"));
            }
        }
        sql.push_str(if column.nullable {
            " NULL"
        } else {
            " NOT NULL"
        });
        if column.auto_increment.is_on() {
            sql.push_str(" AUTO_INCREMENT");
        } else {
            if let Some(default) = present(&column.default) {
                sql.push_str(&format!(" DEFAULT {default}"));
            }
            if let Some(on_update) = present(&column.on_update) {
                sql.push_str(&format!(" ON UPDATE {on_update}"));
            }
        }
        if column.invisible {
            sql.push_str(" INVISIBLE");
        }
        if let Some(comment) = present(&column.comment) {
            sql.push_str(&format!(" COMMENT {}", self.quote_string(comment)));
        }
        Ok(sql)
    }

    /// `[UNIQUE] INDEX name (…)`.
    fn index_sql(&self, index: &IndexDraft) -> String {
        format!(
            "{}INDEX {} {}",
            if index.unique { "UNIQUE " } else { "" },
            self.quote_ident(&index.name),
            column_list(self, &index.columns)
        )
    }
}

/// `ALTER TABLE t` and its clauses, one per line.
fn alter_statement(table: &str, clauses: &[String]) -> String {
    format!("ALTER TABLE {table}\n    {}", clauses.join(",\n    "))
}

/// Whether `text` is the digits of a bit string, as BIT cells are presented.
fn is_bit_string(text: &str) -> bool {
    text.bytes().all(|byte| matches!(byte, b'0' | b'1'))
}

/// A session id as `CONNECTION_ID()`: nothing else reaches the SQL.
fn thread_id(id: &str) -> Result<u64> {
    id.trim()
        .parse()
        .map_err(|_| Error::Config(format!("\"{id}\" is not a session id")))
}

impl Dialect for MysqlDialect {
    fn engine(&self) -> Engine {
        self.engine
    }

    fn has_schemas(&self) -> bool {
        false
    }

    fn lex_rules(&self) -> LexRules {
        LexRules {
            backslash_escapes: self.backslash_escapes(),
            ..LexRules::MYSQL
        }
    }

    fn quote_ident(&self, ident: &str) -> String {
        format!("`{}`", ident.replace('`', "``"))
    }

    fn quote_string(&self, text: &str) -> String {
        // A backslash is an escape character or an ordinary one depending
        // on the session's sql_mode (NO_BACKSLASH_ESCAPES), and guessing
        // wrong lets a value such as `\'` end its own literal. Text holding
        // a backslash or a NUL is therefore written as hexadecimal with a
        // character set, which reads the same in both modes; a quote is
        // doubled, which also reads the same in both.
        if text.contains(['\\', '\0']) {
            return format!("_utf8mb4 X'{}'", hex(text.as_bytes()));
        }
        format!("'{}'", text.replace('\'', "''"))
    }

    fn quote_bytes(&self, bytes: &[u8]) -> String {
        format!("X'{}'", hex(bytes))
    }

    /// The plan as JSON: `EXPLAIN FORMAT=JSON` on both engines.
    ///
    /// With `analyze` the engines part ways. MariaDB has
    /// `ANALYZE FORMAT=JSON`, the same document with `r_*` actual values.
    /// MySQL's `EXPLAIN ANALYZE` prints a text tree (8.0.18+); its JSON form
    /// exists from 8.3 only under `explain_json_format_version = 2`, a
    /// session setting a pure function cannot apply, so MySQL returns the
    /// tree: one row, one column of text.
    fn explain(&self, sql: &str, analyze: bool) -> String {
        match (analyze, self.engine) {
            (false, _) => format!("EXPLAIN FORMAT=JSON {sql}"),
            (true, Engine::MariaDb) => format!("ANALYZE FORMAT=JSON {sql}"),
            (true, _) => format!("EXPLAIN ANALYZE {sql}"),
        }
    }

    /// MySQL's `EXPLAIN ANALYZE` is a tree of text; the rest is JSON.
    fn parse_plan(&self, output: &str, analyze: bool) -> Result<Plan> {
        match (analyze, self.engine) {
            (true, Engine::MySql) => crate::plan::parse_tree(output),
            _ => crate::plan::parse_json_plan(output),
        }
    }

    /// As the default, except for what MySQL spells differently:
    /// - `BIT` cells are bit strings (`10101010`) and are written `b'...'`;
    ///   a quoted string would be stored as its character codes;
    /// - `TINYINT(1)` cells (shown as booleans) hold numbers and are
    ///   written bare, like any other number.
    fn literal(&self, value: &CellValue, column: &ColumnInfo) -> String {
        match (value, column.kind) {
            (CellValue::Text(text), TypeKind::Bit) if is_bit_string(text) => format!("b'{text}'"),
            (CellValue::Text(text), TypeKind::Boolean) if is_number(text) => {
                text.trim().to_string()
            }
            _ => default_literal(self, value, column),
        }
    }

    fn type_for(&self, kind: TypeKind) -> &'static str {
        match kind {
            TypeKind::Integer => "bigint",
            TypeKind::Float => "double",
            TypeKind::Decimal => "decimal(38,10)",
            TypeKind::Boolean => "tinyint(1)",
            TypeKind::Date => "date",
            TypeKind::DateTime | TypeKind::DateTimeTz => "datetime",
            TypeKind::Uuid => "char(36)",
            TypeKind::Json => "json",
            TypeKind::Binary => "longblob",
            _ => "text",
        }
    }

    fn cancel_session_sql(&self, id: &str) -> Result<String> {
        Ok(format!("KILL QUERY {}", thread_id(id)?))
    }

    fn terminate_session_sql(&self, id: &str) -> Result<String> {
        Ok(format!("KILL CONNECTION {}", thread_id(id)?))
    }

    /// `TABLE_ROWS`: InnoDB's sampled estimate, which MySQL caches for
    /// `information_schema_stats_expiry` seconds.
    fn estimated_rows_sql(&self, table: &TableRef) -> String {
        format!(
            "SELECT TABLE_ROWS FROM information_schema.TABLES \
             WHERE TABLE_SCHEMA = {} AND TABLE_NAME = {}",
            self.quote_string(&table.schema),
            self.quote_string(&table.name)
        )
    }

    /// Neither engine has `DEFAULT VALUES`; both accept empty lists.
    fn insert_defaults(&self, table: &TableRef) -> String {
        format!("INSERT INTO {} () VALUES ()", self.table_name(table))
    }

    /// `RENAME TABLE` for tables, views and MariaDB's sequences,
    /// `ALTER EVENT` and `ALTER TABLE ... RENAME INDEX` for the rest.
    /// Routines, triggers and databases cannot be renamed on these engines.
    ///
    /// The new name of a table or event is written with its database: a
    /// bare name would move the object to the session's default database.
    fn rename_sql(&self, object: &ObjectRef, new_name: &str) -> Result<String> {
        let renamed = |from: &TableRef| {
            format!(
                "{} TO {}",
                self.table_name(from),
                self.table_name(&TableRef::new(from.schema.clone(), new_name))
            )
        };
        match object {
            ObjectRef::Relation(_, RelationKind::MaterializedView) => {
                Err(unsupported("materialized views"))
            }
            ObjectRef::Relation(table, _) => Ok(format!("RENAME TABLE {}", renamed(table))),
            ObjectRef::Sequence(sequence) => {
                self.require_sequences()?;
                Ok(format!("RENAME TABLE {}", renamed(sequence)))
            }
            ObjectRef::Event(event) => {
                let (from, to) = (
                    self.table_name(event),
                    self.table_name(&TableRef::new(event.schema.clone(), new_name)),
                );
                Ok(format!("ALTER EVENT {from} RENAME TO {to}"))
            }
            ObjectRef::Index { table, name } => Ok(format!(
                "ALTER TABLE {} RENAME INDEX {} TO {}",
                self.table_name(table),
                self.quote_ident(name),
                self.quote_ident(new_name)
            )),
            ObjectRef::Routine { .. } => Err(unsupported(
                "renaming a function or procedure; drop it and create it again under the new name",
            )),
            ObjectRef::Trigger { .. } => Err(unsupported(
                "renaming a trigger; drop it and create it again under the new name",
            )),
            ObjectRef::Schema(_) | ObjectRef::Database(_) => Err(unsupported(
                "renaming a database; create the new one and move the tables into it with RENAME TABLE",
            )),
            ObjectRef::Type(_) => Err(unsupported("user-defined types")),
        }
    }

    /// Ported from DBeaver's MySQL tools (`ext.mysql/.../tasks/`
    /// `MySQLToolTableAnalyze.java`, `MySQLToolTableCheck.java`,
    /// `MySQLToolTableOptimize.java`, `MySQLToolTableRepair.java`,
    /// `MySQLToolTableTruncate.java`). Each answers with rows (`Table`,
    /// `Op`, `Msg_type`, `Msg_text`) but `TRUNCATE`, and each commits the
    /// open transaction first. DBeaver's "force" truncate (foreign key
    /// checks off around it) is left out.
    fn maintenance_sql(&self, tool: &Maintenance, target: &ObjectRef) -> Result<MaintenanceSql> {
        let ObjectRef::Relation(table, RelationKind::Table | RelationKind::PartitionedTable) =
            target
        else {
            return Err(unsupported("this tool on this kind of object"));
        };
        let name = self.table_name(table);
        let sql = match tool {
            Maintenance::Analyze => format!("ANALYZE TABLE {name}"),
            Maintenance::Check(None) => format!("CHECK TABLE {name}"),
            Maintenance::Check(Some(option)) => format!("CHECK TABLE {name} {}", option.sql()),
            Maintenance::Optimize => format!("OPTIMIZE TABLE {name}"),
            Maintenance::Repair(o) => format!(
                "REPAIR TABLE {name}{}{}{}",
                if o.quick { " QUICK" } else { "" },
                if o.extended { " EXTENDED" } else { "" },
                if o.use_frm { " USE_FRM" } else { "" }
            ),
            Maintenance::Truncate(o) if *o == TruncateOptions::default() => {
                self.truncate_sql(table)
            }
            Maintenance::Truncate(_) => {
                return Err(unsupported("ONLY, RESTART IDENTITY and CASCADE"));
            }
            _ => return Err(unsupported("this tool")),
        };
        Ok(MaintenanceSql {
            sql,
            returns_rows: !matches!(tool, Maintenance::Truncate(_)),
            autocommit: true,
        })
    }

    fn drop_sql(&self, object: &ObjectRef) -> Result<String> {
        let qualified = |schema: &str, name: &str| {
            format!("{}.{}", self.quote_ident(schema), self.quote_ident(name))
        };
        Ok(match object {
            ObjectRef::Relation(_, RelationKind::MaterializedView) => {
                return Err(unsupported("materialized views"));
            }
            ObjectRef::Relation(view, RelationKind::View) => {
                format!("DROP VIEW {}", self.table_name(view))
            }
            ObjectRef::Relation(table, _) => format!("DROP TABLE {}", self.table_name(table)),
            ObjectRef::Routine {
                schema, name, kind, ..
            } => {
                let keyword = match kind {
                    RoutineKind::Procedure => "PROCEDURE",
                    // MariaDB's stored aggregate functions are functions too.
                    RoutineKind::Function | RoutineKind::Aggregate => "FUNCTION",
                    RoutineKind::Window => {
                        return Err(unsupported("user-defined window functions"));
                    }
                };
                // No argument list: these engines have no overloading.
                format!("DROP {keyword} {}", qualified(schema, name))
            }
            ObjectRef::Sequence(sequence) => {
                self.require_sequences()?;
                format!("DROP SEQUENCE {}", self.table_name(sequence))
            }
            // A trigger is named within the database of its table.
            ObjectRef::Trigger { table, name } => {
                format!("DROP TRIGGER {}", qualified(&table.schema, name))
            }
            ObjectRef::Event(event) => format!("DROP EVENT {}", self.table_name(event)),
            ObjectRef::Index { table, name } => format!(
                "DROP INDEX {} ON {}",
                self.quote_ident(name),
                self.table_name(table)
            ),
            // A database is the schema of these engines.
            ObjectRef::Schema(name) | ObjectRef::Database(name) => {
                format!("DROP DATABASE {}", self.quote_ident(name))
            }
            ObjectRef::Type(_) => return Err(unsupported("user-defined types")),
        })
    }

    /// Databases hold tables directly on these engines.
    fn create_schema_sql(&self, _name: &str) -> Result<String> {
        Err(unsupported(
            "schemas inside a database; create a database instead",
        ))
    }

    /// `SHOW CREATE` prints a plain `CREATE`: `OR REPLACE` goes after it,
    /// for views on both engines and for routines on MariaDB. MySQL cannot
    /// replace a routine: it is dropped first (`IF EXISTS`, so the script
    /// runs again after a definition that failed). The definition keeps
    /// its `DELIMITER` lines; the editor splits on them.
    fn replace_source_sql(&self, object: &ObjectRef, ddl: &str) -> Result<String> {
        let or_replace = || Ok(ddl.replacen("CREATE ", "CREATE OR REPLACE ", 1));
        match object {
            ObjectRef::Relation(_, RelationKind::View) => or_replace(),
            ObjectRef::Routine {
                schema, name, kind, ..
            } if matches!(kind, RoutineKind::Function | RoutineKind::Procedure) => {
                if self.engine == Engine::MariaDb {
                    return or_replace();
                }
                let keyword = if *kind == RoutineKind::Procedure {
                    "PROCEDURE"
                } else {
                    "FUNCTION"
                };
                Ok(format!(
                    "DROP {keyword} IF EXISTS {};\n\n{}",
                    self.table_name(&TableRef::new(schema.clone(), name.clone())),
                    ddl.trim_start()
                ))
            }
            _ => Err(unsupported("editing the source of this kind of object")),
        }
    }

    /// One `CREATE TABLE` holding everything: keys, indexes and comments
    /// are written inline.
    fn create_table_sql(&self, draft: &TableDraft) -> Result<Vec<String>> {
        let mut items = draft
            .columns
            .iter()
            .map(|column| self.column_sql(column, None))
            .collect::<Result<Vec<_>>>()?;
        if !draft.primary_key.is_empty() {
            items.push(format!(
                "PRIMARY KEY {}",
                column_list(self, &draft.primary_key)
            ));
        }
        items.extend(draft.indexes.iter().map(|index| self.index_sql(index)));
        items.extend(draft.foreign_keys.iter().map(|k| foreign_key_sql(self, k)));
        let comment = present(&draft.comment).map_or_else(String::new, |c| {
            format!(" COMMENT = {}", self.quote_string(c))
        });
        Ok(vec![format!(
            "CREATE TABLE {} (\n    {}\n){comment}",
            self.table_name(&draft.table),
            items.join(",\n    ")
        )])
    }

    /// Foreign keys are dropped by a first statement; everything else is
    /// one `ALTER TABLE`, which the server checks as a whole and applies
    /// all or nothing. One statement is needed: an auto-increment column
    /// must stay a key, so its primary key cannot be dropped, nor such a
    /// column added, by a statement of its own. The first one is apart
    /// because a foreign key cannot be dropped and added again under the
    /// same name in one statement.
    ///
    /// A column both renamed and altered is one `CHANGE COLUMN`: in a single
    /// statement neither name can be modified after `RENAME COLUMN`.
    fn alter_table_sql(&self, table: &TableRef, changes: &[TableChange]) -> Result<Vec<String>> {
        let name = self.table_name(table);
        let q = |ident: &str| self.quote_ident(ident);
        let altered = |column: &str| {
            changes.iter().find_map(|change| match change {
                TableChange::AlterColumn { name, from, to } if name == column => Some((from, to)),
                _ => None,
            })
        };
        let is_renamed = |column: &str| {
            changes.iter().any(
                |change| matches!(change, TableChange::RenameColumn { to, .. } if to == column),
            )
        };

        let mut statements = Vec::new();
        let dropped_keys: Vec<String> = changes
            .iter()
            .filter_map(|change| match change {
                TableChange::DropForeignKey(key) => Some(format!("DROP FOREIGN KEY {}", q(key))),
                _ => None,
            })
            .collect();
        if !dropped_keys.is_empty() {
            statements.push(alter_statement(&name, &dropped_keys));
        }

        let mut clauses = Vec::new();
        for change in changes {
            match change {
                TableChange::DropForeignKey(_) => {}
                TableChange::DropIndex(index) => clauses.push(format!("DROP INDEX {}", q(index))),
                TableChange::DropPrimaryKey(_) => clauses.push("DROP PRIMARY KEY".into()),
                TableChange::DropColumn(column) => {
                    clauses.push(format!("DROP COLUMN {}", q(column)));
                }
                TableChange::RenameColumn { from, to } => clauses.push(match altered(to) {
                    Some((column, draft)) => format!(
                        "CHANGE COLUMN {} {}",
                        q(from),
                        self.column_sql(draft, Some(column))?
                    ),
                    None => format!("RENAME COLUMN {} TO {}", q(from), q(to)),
                }),
                TableChange::AddColumn(column) => {
                    clauses.push(format!("ADD COLUMN {}", self.column_sql(column, None)?));
                }
                TableChange::AlterColumn { name, from, to } => {
                    if !is_renamed(name) {
                        clauses.push(format!(
                            "MODIFY COLUMN {}",
                            self.column_sql(to, Some(from))?
                        ));
                    }
                }
                TableChange::AddPrimaryKey(columns) => {
                    clauses.push(format!("ADD PRIMARY KEY {}", column_list(self, columns)));
                }
                TableChange::AddIndex(index) => {
                    clauses.push(format!("ADD {}", self.index_sql(index)))
                }
                TableChange::AddForeignKey(key) => {
                    clauses.push(format!("ADD {}", foreign_key_sql(self, key)));
                }
                // MySQL has no NULL comment: an empty one is none.
                TableChange::SetTableComment(comment) => clauses.push(format!(
                    "COMMENT = {}",
                    self.quote_string(present(comment).unwrap_or_default())
                )),
                // With the database: a bare name would move the table to
                // the session's default database.
                TableChange::RenameTable { to } => clauses.push(format!(
                    "RENAME TO {}",
                    self.table_name(&TableRef::new(table.schema.clone(), to.clone()))
                )),
            }
        }
        if !clauses.is_empty() {
            statements.push(alter_statement(&name, &clauses));
        }
        Ok(statements)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kerno_core::table_draft::AutoIncrement;

    fn mysql() -> MysqlDialect {
        MysqlDialect::new(Engine::MySql)
    }

    fn column(kind: TypeKind) -> ColumnInfo {
        ColumnInfo {
            name: "c".into(),
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

    #[test]
    fn identifiers_are_always_quoted() {
        let d = mysql();
        assert_eq!(d.quote_ident("orders"), "`orders`");
        assert_eq!(d.quote_ident("select"), "`select`");
        assert_eq!(d.quote_ident("odd `name`"), "`odd ``name```");
        assert_eq!(
            d.table_name(&TableRef::new("shop", "order items")),
            "`shop`.`order items`"
        );
        assert!(!d.has_schemas());
        assert_eq!(MysqlDialect::new(Engine::MariaDb).engine(), Engine::MariaDb);
    }

    #[test]
    fn strings_double_quotes_and_never_depend_on_the_backslash_mode() {
        let d = mysql();
        assert_eq!(d.quote_string(""), "''");
        assert_eq!(d.quote_string("it's"), "'it''s'");
        assert_eq!(d.quote_string("multi\nline \"x\""), "'multi\nline \"x\"'");
        assert_eq!(
            d.quote_string("caf\u{e9} \u{1F600}"),
            "'caf\u{e9} \u{1F600}'"
        );
        // A backslash or a NUL: hexadecimal, so no quote or backslash of the
        // value is ever read as SQL, whatever sql_mode says.
        assert_eq!(d.quote_string(r"a\b"), "_utf8mb4 X'615c62'");
        assert_eq!(d.quote_string("path\\"), "_utf8mb4 X'706174685c'");
        assert_eq!(
            d.quote_string(r"\' OR 1=1 -- "),
            "_utf8mb4 X'5c27204f5220313d31202d2d20'"
        );
        assert_eq!(d.quote_string("nul\0byte"), "_utf8mb4 X'6e756c0062797465'");
        assert_eq!(d.quote_string("\u{e9}\\"), "_utf8mb4 X'c3a95c'");

        // The spelling is the same in both modes.
        for text in ["plain", "it's", r"a\b'c", "x\0"] {
            let default_mode = d.quote_string(text);
            d.set_no_backslash_escapes(true);
            assert_eq!(d.quote_string(text), default_mode, "{text:?}");
            d.set_no_backslash_escapes(false);
        }
        // The lexer still follows the mode, to split what the user types.
        assert!(d.lex_rules().backslash_escapes);
        d.set_no_backslash_escapes(true);
        assert!(!d.lex_rules().backslash_escapes);
    }

    #[test]
    fn bytes_and_paging() {
        let d = mysql();
        assert_eq!(d.quote_bytes(&[0xde, 0xad, 0x00]), "X'dead00'");
        assert_eq!(d.quote_bytes(&[]), "X''");
        assert_eq!(d.limit_offset(100, 0), "LIMIT 100");
        assert_eq!(d.limit_offset(100, 300), "LIMIT 100 OFFSET 300");
    }

    #[test]
    fn explain_forms() {
        let mysql = mysql();
        let mariadb = MysqlDialect::new(Engine::MariaDb);
        assert_eq!(
            mysql.explain("select 1", false),
            "EXPLAIN FORMAT=JSON select 1"
        );
        assert_eq!(
            mariadb.explain("select 1", false),
            "EXPLAIN FORMAT=JSON select 1"
        );
        assert_eq!(mysql.explain("select 1", true), "EXPLAIN ANALYZE select 1");
        assert_eq!(
            mariadb.explain("select 1", true),
            "ANALYZE FORMAT=JSON select 1"
        );
    }

    #[test]
    fn literals_by_column_kind() {
        let d = mysql();
        let text = |value: &str| CellValue::Text(value.into());
        assert_eq!(d.literal(&CellValue::Null, &column(TypeKind::Bit)), "NULL");
        assert_eq!(
            d.literal(&CellValue::Default, &column(TypeKind::Text)),
            "DEFAULT"
        );
        assert_eq!(
            d.literal(&CellValue::Bytes(vec![1, 255]), &column(TypeKind::Binary)),
            "X'01ff'"
        );

        // BIT: a bit-value literal, whatever the width.
        assert_eq!(
            d.literal(&text("10101010"), &column(TypeKind::Bit)),
            "b'10101010'"
        );
        assert_eq!(d.literal(&text("0"), &column(TypeKind::Bit)), "b'0'");
        // Not a bit string: left for the server to judge, never spliced in bare.
        assert_eq!(d.literal(&text("12"), &column(TypeKind::Bit)), "'12'");
        assert_eq!(
            d.literal(&text("1' OR '1"), &column(TypeKind::Bit)),
            "'1'' OR ''1'"
        );

        // TINYINT(1): numbers as sent, and the words people type.
        assert_eq!(d.literal(&text("1"), &column(TypeKind::Boolean)), "1");
        assert_eq!(d.literal(&text("0"), &column(TypeKind::Boolean)), "0");
        assert_eq!(d.literal(&text("-128"), &column(TypeKind::Boolean)), "-128");
        assert_eq!(d.literal(&text("true"), &column(TypeKind::Boolean)), "TRUE");
        assert_eq!(d.literal(&text("F"), &column(TypeKind::Boolean)), "FALSE");
        assert_eq!(d.literal(&text("yes"), &column(TypeKind::Boolean)), "'yes'");

        // Numbers are bare only in numeric columns, and only when plain.
        assert_eq!(d.literal(&text("42"), &column(TypeKind::Integer)), "42");
        assert_eq!(
            d.literal(&text("1.7976931348623157e308"), &column(TypeKind::Float)),
            "1.7976931348623157e308"
        );
        assert_eq!(
            d.literal(&text("-99999999999999.999999"), &column(TypeKind::Decimal)),
            "-99999999999999.999999"
        );
        assert_eq!(d.literal(&text("42"), &column(TypeKind::Text)), "'42'");
        assert_eq!(
            d.literal(&text("1; DROP TABLE x"), &column(TypeKind::Integer)),
            "'1; DROP TABLE x'"
        );

        // Everything else is a string for the server to convert.
        assert_eq!(
            d.literal(&text("-838:59:59.000000"), &column(TypeKind::Time)),
            "'-838:59:59.000000'"
        );
        assert_eq!(d.literal(&text("a,c"), &column(TypeKind::Enum)), "'a,c'");
        assert_eq!(
            d.literal(&text(r#"{"a": 1}"#), &column(TypeKind::Json)),
            r#"'{"a": 1}'"#
        );
        // A backslash anywhere switches the literal to hexadecimal.
        assert_eq!(
            d.literal(&text(r"a\b"), &column(TypeKind::Text)),
            "_utf8mb4 X'615c62'"
        );
    }

    fn both() -> [MysqlDialect; 2] {
        [mysql(), MysqlDialect::new(Engine::MariaDb)]
    }

    fn t() -> TableRef {
        TableRef::new("shop", "order items")
    }

    fn routine(kind: RoutineKind) -> ObjectRef {
        ObjectRef::Routine {
            schema: "shop".into(),
            name: "order total".into(),
            kind,
            identity_arguments: String::new(),
        }
    }

    fn trigger() -> ObjectRef {
        ObjectRef::Trigger {
            table: t(),
            name: "audit `it`".into(),
        }
    }

    fn index(name: &str) -> ObjectRef {
        ObjectRef::Index {
            table: t(),
            name: name.into(),
        }
    }

    /// The message of the `Unsupported` error `result` holds.
    fn refusal(result: Result<String>) -> String {
        match result {
            Err(Error::Unsupported(message)) => message,
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn rename_statements() {
        for d in both() {
            // Tables and views alike; the new name stays in the database.
            for kind in [
                RelationKind::Table,
                RelationKind::PartitionedTable,
                RelationKind::ForeignTable,
                RelationKind::View,
            ] {
                assert_eq!(
                    d.rename_sql(&ObjectRef::Relation(t(), kind), "odd `name`")
                        .unwrap(),
                    "RENAME TABLE `shop`.`order items` TO `shop`.`odd ``name```",
                    "{kind:?}"
                );
            }
            assert_eq!(
                d.rename_sql(&ObjectRef::Event(TableRef::new("shop", "nightly")), "order")
                    .unwrap(),
                "ALTER EVENT `shop`.`nightly` RENAME TO `shop`.`order`"
            );
            assert_eq!(
                d.rename_sql(&index("by date"), "order").unwrap(),
                "ALTER TABLE `shop`.`order items` RENAME INDEX `by date` TO `order`"
            );
            // A new name is an identifier, never SQL.
            assert_eq!(
                d.rename_sql(
                    &ObjectRef::Relation(t(), RelationKind::Table),
                    "x`; DROP TABLE y"
                )
                .unwrap(),
                "RENAME TABLE `shop`.`order items` TO `shop`.`x``; DROP TABLE y`"
            );
        }
    }

    #[test]
    fn what_cannot_be_renamed_says_why() {
        for d in both() {
            for kind in [
                RoutineKind::Function,
                RoutineKind::Procedure,
                RoutineKind::Aggregate,
                RoutineKind::Window,
            ] {
                assert_eq!(
                    refusal(d.rename_sql(&routine(kind), "x")),
                    "renaming a function or procedure; drop it and create it again under the new name"
                );
            }
            assert_eq!(
                refusal(d.rename_sql(&trigger(), "x")),
                "renaming a trigger; drop it and create it again under the new name"
            );
            for object in [
                ObjectRef::Schema("shop".into()),
                ObjectRef::Database("shop".into()),
            ] {
                assert_eq!(
                    refusal(d.rename_sql(&object, "x")),
                    "renaming a database; create the new one and move the tables into it with RENAME TABLE"
                );
            }
            let view = ObjectRef::Relation(t(), RelationKind::MaterializedView);
            assert_eq!(refusal(d.rename_sql(&view, "x")), "materialized views");
            assert_eq!(refusal(d.drop_sql(&view)), "materialized views");
            let user_type = ObjectRef::Type(t());
            assert_eq!(refusal(d.rename_sql(&user_type, "x")), "user-defined types");
            assert_eq!(refusal(d.drop_sql(&user_type)), "user-defined types");
            assert_eq!(
                refusal(d.drop_sql(&routine(RoutineKind::Window))),
                "user-defined window functions"
            );
            assert_eq!(
                refusal(d.create_schema_sql("x")),
                "schemas inside a database; create a database instead"
            );
        }
        assert_eq!(
            mysql().rename_sql(&trigger(), "x").unwrap_err().to_string(),
            "not supported by this database: renaming a trigger; \
             drop it and create it again under the new name"
        );
    }

    #[test]
    fn source_scripts_replace_what_show_create_printed() {
        use kerno_core::sql::split_statements;
        let (mysql, mariadb) = (mysql(), MysqlDialect::new(Engine::MariaDb));
        let routine = |kind| ObjectRef::Routine {
            schema: "shop".into(),
            name: "f".into(),
            kind,
            identity_arguments: String::new(),
        };
        let view = ObjectRef::Relation(TableRef::new("shop", "v"), RelationKind::View);
        let view_ddl = "CREATE ALGORITHM=UNDEFINED DEFINER=`kerno`@`%` SQL SECURITY DEFINER \
                        VIEW `v` AS select 1 AS `a`;";
        for d in [&mysql, &mariadb] {
            assert_eq!(
                d.replace_source_sql(&view, view_ddl).unwrap(),
                "CREATE OR REPLACE ALGORITHM=UNDEFINED DEFINER=`kerno`@`%` SQL SECURITY DEFINER \
                 VIEW `v` AS select 1 AS `a`;"
            );
        }
        // As the driver prints a routine: between DELIMITER lines.
        let body = "CREATE DEFINER=`kerno`@`%` FUNCTION `f`() RETURNS int\n    DETERMINISTIC\n\
                    BEGIN\n  DECLARE x INT;\n  RETURN 1;\nEND";
        let ddl = format!("DELIMITER //\n{body}//\nDELIMITER ;");
        let statements = |script: &str| -> Vec<String> {
            split_statements(script, LexRules::MYSQL)
                .iter()
                .map(|statement| statement.text(script).to_string())
                .collect()
        };
        // MariaDB replaces it in place.
        let replaced = mariadb
            .replace_source_sql(&routine(RoutineKind::Function), &ddl)
            .unwrap();
        assert_eq!(
            statements(&replaced),
            [body.replacen("CREATE ", "CREATE OR REPLACE ", 1)]
        );
        // MySQL drops it first; the body stays one statement.
        for (kind, keyword) in [
            (RoutineKind::Function, "FUNCTION"),
            (RoutineKind::Procedure, "PROCEDURE"),
        ] {
            let script = mysql.replace_source_sql(&routine(kind), &ddl).unwrap();
            assert_eq!(
                statements(&script),
                [
                    format!("DROP {keyword} IF EXISTS `shop`.`f`"),
                    body.to_string()
                ]
            );
        }
        for object in [
            ObjectRef::Relation(TableRef::new("shop", "t"), RelationKind::Table),
            ObjectRef::Event(TableRef::new("shop", "e")),
            routine(RoutineKind::Aggregate),
        ] {
            assert!(
                matches!(
                    mysql.replace_source_sql(&object, ""),
                    Err(Error::Unsupported(_))
                ),
                "{object:?}"
            );
        }
        // Extensions and standalone enums are PostgreSQL's.
        let t = TableRef::new("shop", "t");
        for result in [
            mariadb.create_extension_sql("x"),
            mariadb.drop_extension_sql("x"),
            mariadb.create_enum_sql(&t, &[]),
            mariadb.add_enum_value_sql(&t, "a", None),
            mariadb.rename_enum_value_sql(&t, "a", "b"),
        ] {
            assert!(matches!(result, Err(Error::Unsupported(_))), "{result:?}");
        }
    }

    #[test]
    fn sequences_are_mariadb_only() {
        let sequence = ObjectRef::Sequence(TableRef::new("shop", "invoice numbers"));
        let mariadb = MysqlDialect::new(Engine::MariaDb);
        assert_eq!(
            mariadb.rename_sql(&sequence, "order").unwrap(),
            "RENAME TABLE `shop`.`invoice numbers` TO `shop`.`order`"
        );
        assert_eq!(
            mariadb.drop_sql(&sequence).unwrap(),
            "DROP SEQUENCE `shop`.`invoice numbers`"
        );
        assert_eq!(refusal(mysql().rename_sql(&sequence, "x")), "sequences");
        assert_eq!(refusal(mysql().drop_sql(&sequence)), "sequences");
    }

    #[test]
    fn drop_statements() {
        for d in both() {
            let drop = |object: ObjectRef| d.drop_sql(&object).unwrap();
            for kind in [
                RelationKind::Table,
                RelationKind::PartitionedTable,
                RelationKind::ForeignTable,
            ] {
                assert_eq!(
                    drop(ObjectRef::Relation(t(), kind)),
                    "DROP TABLE `shop`.`order items`"
                );
            }
            assert_eq!(
                drop(ObjectRef::Relation(t(), RelationKind::View)),
                "DROP VIEW `shop`.`order items`"
            );
            // No argument list: these engines have no overloading.
            assert_eq!(
                drop(routine(RoutineKind::Function)),
                "DROP FUNCTION `shop`.`order total`"
            );
            assert_eq!(
                drop(routine(RoutineKind::Aggregate)),
                "DROP FUNCTION `shop`.`order total`"
            );
            assert_eq!(
                drop(routine(RoutineKind::Procedure)),
                "DROP PROCEDURE `shop`.`order total`"
            );
            // A trigger is named within the database of its table.
            assert_eq!(drop(trigger()), "DROP TRIGGER `shop`.`audit ``it```");
            assert_eq!(
                drop(ObjectRef::Event(TableRef::new("shop", "nightly"))),
                "DROP EVENT `shop`.`nightly`"
            );
            assert_eq!(
                drop(index("by date")),
                "DROP INDEX `by date` ON `shop`.`order items`"
            );
            assert_eq!(
                drop(index("PRIMARY")),
                "DROP INDEX `PRIMARY` ON `shop`.`order items`"
            );
            // A database is the schema of these engines.
            assert_eq!(
                drop(ObjectRef::Schema("my db".into())),
                "DROP DATABASE `my db`"
            );
            assert_eq!(
                drop(ObjectRef::Database("my db".into())),
                "DROP DATABASE `my db`"
            );
        }
    }

    #[test]
    fn signals_name_a_thread_by_its_number_only() {
        for d in both() {
            assert_eq!(d.cancel_session_sql(" 42 ").unwrap(), "KILL QUERY 42");
            assert_eq!(d.terminate_session_sql("7").unwrap(), "KILL CONNECTION 7");
            for id in ["", "-1", "1; DROP DATABASE kerno"] {
                let error = d.terminate_session_sql(id).unwrap_err();
                assert!(matches!(error, Error::Config(_)), "{id}: {error:?}");
                assert!(d.cancel_session_sql(id).is_err(), "{id}");
            }
        }
    }

    #[test]
    fn truncate_and_create_statements() {
        for d in both() {
            assert_eq!(d.truncate_sql(&t()), "TRUNCATE TABLE `shop`.`order items`");
            assert_eq!(
                d.create_database_sql("odd `name`").unwrap(),
                "CREATE DATABASE `odd ``name```"
            );
        }
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn draft_column(name: &str, data_type: &str, nullable: bool) -> ColumnDraft {
        ColumnDraft {
            original: None,
            name: name.into(),
            data_type: data_type.into(),
            nullable,
            default: None,
            comment: None,
            auto_increment: AutoIncrement::Off,
            on_update: None,
            invisible: false,
        }
    }

    fn foreign_key() -> kerno_core::table_draft::ForeignKeyDraft {
        kerno_core::table_draft::ForeignKeyDraft {
            original: None,
            name: "fk".into(),
            columns: strings(&["customer_id"]),
            referenced_table: TableRef::new("shop", "customers"),
            referenced_columns: strings(&["id"]),
            on_update: "NO ACTION".into(),
            on_delete: "CASCADE".into(),
        }
    }

    #[test]
    fn create_table_is_one_statement_with_everything_inline() {
        for d in both() {
            let mut draft = TableDraft::new(t());
            draft.comment = Some("it's ours".into());
            draft.columns.push(ColumnDraft {
                default: Some("'new'".into()),
                comment: Some(r"a\b".into()),
                ..draft_column("status", "varchar(20)", false)
            });
            draft.columns.push(draft_column("customer_id", "int", true));
            draft.indexes = vec![
                IndexDraft {
                    original: None,
                    name: "by status".into(),
                    columns: strings(&["status"]),
                    unique: true,
                },
                IndexDraft {
                    original: None,
                    name: "by customer".into(),
                    columns: strings(&["customer_id", "status"]),
                    unique: false,
                },
            ];
            draft.foreign_keys = vec![foreign_key()];
            assert_eq!(
                d.create_table_sql(&draft).unwrap(),
                strings(&["CREATE TABLE `shop`.`order items` (\n    \
                     `id` bigint NOT NULL AUTO_INCREMENT,\n    \
                     `status` varchar(20) NOT NULL DEFAULT 'new' COMMENT _utf8mb4 X'615c62',\n    \
                     `customer_id` int NULL,\n    \
                     PRIMARY KEY (`id`),\n    \
                     UNIQUE INDEX `by status` (`status`),\n    \
                     INDEX `by customer` (`customer_id`, `status`),\n    \
                     CONSTRAINT `fk` FOREIGN KEY (`customer_id`) REFERENCES `shop`.`customers` (`id`) \
                     ON UPDATE NO ACTION ON DELETE CASCADE\n) COMMENT = 'it''s ours'"])
            );
        }
    }

    #[test]
    fn alter_table_drops_foreign_keys_first_then_does_the_rest_at_once() {
        let from = |name: &str, data_type: &str| ColumnInfo {
            name: name.into(),
            data_type: data_type.into(),
            nullable: true,
            collation: Some("utf8mb4_bin".into()),
            ..column(TypeKind::Text)
        };
        let code = from("code", "varchar(10)");
        let note = from("note", "varchar(10)");
        let changes = [
            TableChange::DropForeignKey("fk".into()),
            TableChange::DropForeignKey("fk2".into()),
            TableChange::DropIndex("old".into()),
            TableChange::DropPrimaryKey("PRIMARY".into()),
            TableChange::DropColumn("gone".into()),
            TableChange::RenameColumn {
                from: "code".into(),
                to: "kode".into(),
            },
            TableChange::RenameColumn {
                from: "a".into(),
                to: "b".into(),
            },
            TableChange::AddColumn(ColumnDraft {
                default: Some("0".into()),
                ..draft_column("n", "int", false)
            }),
            // Renamed and altered: one CHANGE COLUMN, collation kept.
            TableChange::AlterColumn {
                name: "kode".into(),
                from: code.clone(),
                to: ColumnDraft {
                    nullable: false,
                    ..ColumnDraft {
                        name: "kode".into(),
                        ..ColumnDraft::from(&code)
                    }
                },
            },
            // Retyped: the collation of the old type is not carried over.
            TableChange::AlterColumn {
                name: "note".into(),
                from: note.clone(),
                to: ColumnDraft {
                    data_type: "text".into(),
                    comment: Some("n".into()),
                    ..ColumnDraft::from(&note)
                },
            },
            TableChange::AddPrimaryKey(strings(&["id", "n"])),
            TableChange::AddIndex(IndexDraft {
                original: None,
                name: "i".into(),
                columns: strings(&["b"]),
                unique: false,
            }),
            TableChange::AddForeignKey(foreign_key()),
            TableChange::SetTableComment(None),
            TableChange::RenameTable {
                to: "orders".into(),
            },
        ];
        for d in both() {
            assert_eq!(
                d.alter_table_sql(&t(), &changes).unwrap(),
                strings(&[
                    "ALTER TABLE `shop`.`order items`\n    \
                     DROP FOREIGN KEY `fk`,\n    DROP FOREIGN KEY `fk2`",
                    "ALTER TABLE `shop`.`order items`\n    \
                     DROP INDEX `old`,\n    \
                     DROP PRIMARY KEY,\n    \
                     DROP COLUMN `gone`,\n    \
                     CHANGE COLUMN `code` `kode` varchar(10) COLLATE utf8mb4_bin NOT NULL,\n    \
                     RENAME COLUMN `a` TO `b`,\n    \
                     ADD COLUMN `n` int NOT NULL DEFAULT 0,\n    \
                     MODIFY COLUMN `note` text NULL COMMENT 'n',\n    \
                     ADD PRIMARY KEY (`id`, `n`),\n    \
                     ADD INDEX `i` (`b`),\n    \
                     ADD CONSTRAINT `fk` FOREIGN KEY (`customer_id`) REFERENCES `shop`.`customers` (`id`) \
                     ON UPDATE NO ACTION ON DELETE CASCADE,\n    \
                     COMMENT = '',\n    \
                     RENAME TO `shop`.`orders`",
                ])
            );
            // Nothing to do, nothing to run; no foreign key, one statement.
            assert_eq!(d.alter_table_sql(&t(), &[]).unwrap(), Vec::<String>::new());
            assert_eq!(
                d.alter_table_sql(&t(), &[TableChange::SetTableComment(Some("x".into()))])
                    .unwrap(),
                strings(&["ALTER TABLE `shop`.`order items`\n    COMMENT = 'x'"])
            );
        }
    }

    #[test]
    fn on_update_is_written_back_in_the_definition() {
        let mut updated_at = column(TypeKind::DateTime);
        updated_at.name = "updated_at".into();
        updated_at.data_type = "timestamp".into();
        updated_at.nullable = false;
        updated_at.default = Some("CURRENT_TIMESTAMP".into());
        updated_at.on_update = Some("CURRENT_TIMESTAMP".into());
        let comment_only = TableChange::AlterColumn {
            name: "updated_at".into(),
            from: updated_at.clone(),
            to: ColumnDraft {
                comment: Some("touched".into()),
                ..ColumnDraft::from(&updated_at)
            },
        };
        let mut draft = TableDraft::new(t());
        draft.columns.push(ColumnDraft::from(&updated_at));
        for d in both() {
            assert_eq!(
                d.alter_table_sql(&t(), std::slice::from_ref(&comment_only))
                    .unwrap(),
                strings(&["ALTER TABLE `shop`.`order items`\n    \
                     MODIFY COLUMN `updated_at` timestamp NOT NULL DEFAULT CURRENT_TIMESTAMP \
                     ON UPDATE CURRENT_TIMESTAMP COMMENT 'touched'"])
            );
            assert_eq!(
                d.create_table_sql(&draft).unwrap(),
                strings(&["CREATE TABLE `shop`.`order items` (\n    \
                     `id` bigint NOT NULL AUTO_INCREMENT,\n    \
                     `updated_at` timestamp NOT NULL DEFAULT CURRENT_TIMESTAMP \
                     ON UPDATE CURRENT_TIMESTAMP,\n    \
                     PRIMARY KEY (`id`)\n)"])
            );
        }
    }

    #[test]
    fn an_invisible_column_is_written_back_invisible() {
        let mut hidden = column(TypeKind::Text);
        hidden.name = "secret".into();
        hidden.data_type = "varchar(20)".into();
        hidden.invisible = true;
        let change = TableChange::AlterColumn {
            name: "secret".into(),
            from: hidden.clone(),
            to: ColumnDraft {
                comment: Some("hidden".into()),
                ..ColumnDraft::from(&hidden)
            },
        };
        for d in both() {
            assert_eq!(
                d.alter_table_sql(&t(), std::slice::from_ref(&change))
                    .unwrap(),
                strings(&["ALTER TABLE `shop`.`order items`\n    \
                     MODIFY COLUMN `secret` varchar(20) NULL INVISIBLE COMMENT 'hidden'"])
            );
        }
    }

    #[test]
    fn a_generated_column_is_not_rewritten() {
        let mut total = column(TypeKind::Integer);
        total.name = "total".into();
        total.data_type = "int".into();
        total.generation = Some(ColumnGeneration::Stored("a + b".into()));
        let change = TableChange::AlterColumn {
            name: "total".into(),
            from: total.clone(),
            to: ColumnDraft {
                comment: Some("sum".into()),
                ..ColumnDraft::from(&total)
            },
        };
        for d in both() {
            assert_eq!(
                refusal(
                    d.alter_table_sql(&t(), std::slice::from_ref(&change))
                        .map(|statements| statements.join(";"))
                ),
                "changing the generated column `total` from the table form; \
                 write the ALTER TABLE statement instead"
            );
        }
    }

    #[test]
    fn the_row_estimate_is_read_from_the_catalog() {
        for d in both() {
            assert_eq!(
                d.estimated_rows_sql(&TableRef::new("shop", "it's")),
                "SELECT TABLE_ROWS FROM information_schema.TABLES \
                 WHERE TABLE_SCHEMA = 'shop' AND TABLE_NAME = 'it''s'"
            );
        }
    }

    #[test]
    fn insert_defaults_statement() {
        assert_eq!(
            mysql().insert_defaults(&TableRef::new("shop", "t")),
            "INSERT INTO `shop`.`t` () VALUES ()"
        );
    }

    mod maintenance {
        use super::*;
        use kerno_core::maintenance::{CheckOption, RefreshOptions, RepairOptions, VacuumOptions};

        const ODD: &str = "`shop`.`order ``items`";

        fn table() -> ObjectRef {
            ObjectRef::Relation(TableRef::new("shop", "order `items"), RelationKind::Table)
        }

        fn sql(d: &MysqlDialect, tool: Maintenance) -> String {
            let out = d.maintenance_sql(&tool, &table()).unwrap();
            assert_eq!(
                out.returns_rows,
                !matches!(tool, Maintenance::Truncate(_)),
                "{}",
                out.sql
            );
            assert!(out.autocommit, "{}", out.sql);
            out.sql
        }

        #[test]
        fn table_tools_and_their_options() {
            for d in both() {
                assert_eq!(
                    sql(&d, Maintenance::Analyze),
                    format!("ANALYZE TABLE {ODD}")
                );
                assert_eq!(
                    sql(&d, Maintenance::Optimize),
                    format!("OPTIMIZE TABLE {ODD}")
                );
                assert_eq!(
                    sql(&d, Maintenance::Check(None)),
                    format!("CHECK TABLE {ODD}")
                );
                let words = [
                    "FOR UPGRADE",
                    "QUICK",
                    "FAST",
                    "MEDIUM",
                    "EXTENDED",
                    "CHANGED",
                ];
                for (option, words) in CheckOption::ALL.into_iter().zip(words) {
                    assert_eq!(
                        sql(&d, Maintenance::Check(Some(option))),
                        format!("CHECK TABLE {ODD} {words}")
                    );
                }
                assert_eq!(
                    sql(&d, Maintenance::Repair(RepairOptions::default())),
                    format!("REPAIR TABLE {ODD}")
                );
                assert_eq!(
                    sql(
                        &d,
                        Maintenance::Repair(RepairOptions {
                            quick: true,
                            extended: true,
                            use_frm: true,
                        })
                    ),
                    format!("REPAIR TABLE {ODD} QUICK EXTENDED USE_FRM")
                );
                assert_eq!(
                    sql(&d, Maintenance::Truncate(TruncateOptions::default())),
                    format!("TRUNCATE TABLE {ODD}")
                );
                let cascade = Maintenance::Truncate(TruncateOptions {
                    cascade: true,
                    ..TruncateOptions::default()
                });
                assert!(matches!(
                    d.maintenance_sql(&cascade, &table()),
                    Err(Error::Unsupported(_))
                ));
            }
        }

        #[test]
        fn only_tables_have_tools() {
            for d in both() {
                let partitioned =
                    ObjectRef::Relation(TableRef::new("shop", "p"), RelationKind::PartitionedTable);
                for target in [table(), partitioned] {
                    assert_eq!(
                        d.maintenance_tools(&target),
                        [
                            Maintenance::Analyze,
                            Maintenance::Truncate(TruncateOptions::default()),
                            Maintenance::Check(None),
                            Maintenance::Optimize,
                            Maintenance::Repair(RepairOptions::default()),
                        ]
                    );
                }
                for nothing in [
                    ObjectRef::Relation(t(), RelationKind::View),
                    ObjectRef::Database("shop".into()),
                    trigger(),
                    routine(RoutineKind::Procedure),
                ] {
                    assert!(d.maintenance_tools(&nothing).is_empty(), "{nothing:?}");
                }
                // PostgreSQL's tools are refused with `Unsupported`.
                for tool in [
                    Maintenance::Vacuum(VacuumOptions::default()),
                    Maintenance::RefreshMaterializedView(RefreshOptions::default()),
                    Maintenance::EnableTrigger,
                ] {
                    assert!(matches!(
                        d.maintenance_sql(&tool, &table()),
                        Err(Error::Unsupported(_))
                    ));
                }
            }
        }
    }
}
