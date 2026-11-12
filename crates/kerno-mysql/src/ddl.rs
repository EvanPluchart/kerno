//! `CREATE` statements, as the server prints them with `SHOW CREATE ...`.
//!
//! The text is the server's, unchanged: names are unqualified where the
//! server leaves them so (a table's own name, the tables a view or trigger
//! reads), which means a script is meant to run with the object's database
//! as the current one, as with `mysqldump`.
//!
//! Routines, triggers and events have bodies with semicolons in them. Their
//! statements are wrapped in `DELIMITER` lines, the convention of the
//! `mysql` client that Kerno's statement splitter follows too
//! (`kerno_core::sql::split_statements`), so a script splits into the
//! statements the server must receive one by one.
//!
//! Only an index has no `SHOW CREATE`; its statement is built from the
//! catalog (see `introspect`).

use kerno_core::schema::{ObjectRef, RelationKind, RoutineKind, TableRef};
use kerno_core::{Engine, Error, Result, Session};

use crate::introspect::{self, not_found};
use crate::session::MysqlSession;

pub(crate) async fn ddl(session: &MysqlSession, object: &ObjectRef) -> Result<String> {
    let dialect = session.dialect();
    match object {
        ObjectRef::Relation(table, RelationKind::View | RelationKind::MaterializedView) => {
            let name = dialect.table_name(table);
            show_create(session, "VIEW", &name, 1).await.map(terminated)
        }
        ObjectRef::Relation(table, _) => self::table(session, table).await,
        ObjectRef::Routine {
            schema, name, kind, ..
        } => {
            let keyword = match kind {
                RoutineKind::Procedure => "PROCEDURE",
                // MariaDB's stored aggregate functions are functions too.
                RoutineKind::Function | RoutineKind::Aggregate => "FUNCTION",
                RoutineKind::Window => {
                    return Err(Error::Unsupported("user-defined window functions".into()));
                }
            };
            let name = dialect.table_name(&TableRef::new(schema.clone(), name.clone()));
            show_create(session, keyword, &name, 2)
                .await
                .map(|body| delimited(&body))
        }
        ObjectRef::Trigger { table, name } => trigger(session, &table.schema, name).await,
        ObjectRef::Event(event) => {
            let name = dialect.table_name(event);
            show_create(session, "EVENT", &name, 3)
                .await
                .map(|body| delimited(&body))
        }
        ObjectRef::Sequence(sequence) => {
            if session.engine() != Engine::MariaDb {
                return Err(Error::Unsupported("sequences".into()));
            }
            let name = dialect.table_name(sequence);
            show_create(session, "SEQUENCE", &name, 1)
                .await
                .map(terminated)
        }
        ObjectRef::Index { table, name } => introspect::table_structure(session, table)
            .await?
            .indexes
            .into_iter()
            .find(|index| &index.name == name)
            .and_then(|index| index.definition)
            .ok_or_else(|| {
                not_found(&format!(
                    "index {} on {}",
                    dialect.quote_ident(name),
                    dialect.table_name(table)
                ))
            }),
        // A database is the schema of these engines.
        ObjectRef::Schema(name) | ObjectRef::Database(name) => {
            show_create(session, "DATABASE", &dialect.quote_ident(name), 1)
                .await
                .map(terminated)
        }
        ObjectRef::Type(_) => Err(Error::Unsupported("user-defined types".into())),
    }
}

/// Runs `SHOW CREATE <keyword> <name>` and returns the statement, which is
/// in a column whose position depends on the kind of object.
async fn show_create(
    session: &MysqlSession,
    keyword: &str,
    name: &str,
    column: usize,
) -> Result<String> {
    let rows = session
        .query(&format!("SHOW CREATE {keyword} {name}"))
        .await?;
    let row = rows
        .first()
        .ok_or_else(|| not_found(&format!("{} {name}", keyword.to_lowercase())))?;
    // The server answers NULL instead of an error when the account may
    // list the object but not read its definition.
    row.get(column).cloned().flatten().ok_or_else(|| {
        Error::Db(Box::new(kerno_core::DbError::message(format!(
            "the server did not show the definition of {} {name}: this account lacks the \
             privilege to read it",
            keyword.to_lowercase()
        ))))
    })
}

/// A table's script: the table with its indexes, constraints and comments,
/// then its triggers.
async fn table(session: &MysqlSession, table: &TableRef) -> Result<String> {
    let name = session.dialect().table_name(table);
    let mut statements = vec![terminated(show_create(session, "TABLE", &name, 1).await?)];
    for trigger in introspect::table_structure(session, table).await?.triggers {
        statements.push(self::trigger(session, &table.schema, &trigger.name).await?);
    }
    Ok(statements.join("\n\n"))
}

async fn trigger(session: &MysqlSession, database: &str, name: &str) -> Result<String> {
    let name = session.dialect().table_name(&TableRef::new(database, name));
    show_create(session, "TRIGGER", &name, 2)
        .await
        .map(|body| delimited(&body))
}

/// `sql` with exactly one trailing semicolon.
fn terminated(sql: String) -> String {
    format!("{};", sql.trim_end().trim_end_matches(';'))
}

/// A statement whose body contains semicolons, between `DELIMITER` lines.
fn delimited(sql: &str) -> String {
    let sql = sql.trim_end().trim_end_matches(';');
    // A delimiter the statement does not contain, made of punctuation
    // only: the splitter looks for it where a token starts.
    let mut delimiter = String::from("//");
    if sql.contains("//") {
        delimiter = ";;".into();
        while sql.contains(&delimiter) {
            delimiter.push(';');
        }
    }
    format!("DELIMITER {delimiter}\n{sql}{delimiter}\nDELIMITER ;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use kerno_core::sql::{LexRules, split_statements};

    #[test]
    fn statements_end_with_one_semicolon() {
        assert_eq!(terminated("select 1".into()), "select 1;");
        assert_eq!(terminated("select 1;\n".into()), "select 1;");
        assert_eq!(terminated("select 1;;".into()), "select 1;");
    }

    #[test]
    fn bodies_with_semicolons_split_as_one_statement() {
        let body = "CREATE TRIGGER t BEFORE INSERT ON x FOR EACH ROW\nBEGIN\n    SET NEW.a = 1;\n    SET NEW.b = 2;\nEND";
        let script = format!("CREATE TABLE x (a int);\n\n{}", delimited(body));
        assert_eq!(
            delimited(body),
            format!("DELIMITER //\n{body}//\nDELIMITER ;")
        );
        let statements: Vec<&str> = split_statements(&script, LexRules::MYSQL)
            .iter()
            .map(|statement| statement.text(&script))
            .collect();
        assert_eq!(statements, ["CREATE TABLE x (a int)", body]);
    }

    #[test]
    fn the_delimiter_avoids_what_the_body_contains() {
        for (body, delimiter) in [
            ("CREATE PROCEDURE p() BEGIN SELECT 'http://x'; END", ";;"),
            ("CREATE PROCEDURE p() BEGIN SELECT 1 // 2;; END", ";;;"),
        ] {
            let script = delimited(body);
            assert!(
                script.starts_with(&format!("DELIMITER {delimiter}\n")),
                "{script}"
            );
            let statements: Vec<&str> = split_statements(&script, LexRules::MYSQL)
                .iter()
                .map(|statement| statement.text(&script))
                .collect();
            assert_eq!(statements, [body]);
        }
    }
}
