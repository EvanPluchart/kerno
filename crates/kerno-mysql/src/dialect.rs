//! The SQL dialect of MySQL and MariaDB.

use std::sync::atomic::{AtomicBool, Ordering};

use kerno_core::dialect::{Dialect, default_literal, hex};
use kerno_core::schema::{ColumnInfo, TableRef};
use kerno_core::sql::params::is_number;
use kerno_core::sql::{CellValue, LexRules};
use kerno_core::{Engine, TypeKind};

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
}

/// Whether `text` is the digits of a bit string, as BIT cells are presented.
fn is_bit_string(text: &str) -> bool {
    text.bytes().all(|byte| matches!(byte, b'0' | b'1'))
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

    /// Neither engine has `DEFAULT VALUES`; both accept empty lists.
    fn insert_defaults(&self, table: &TableRef) -> String {
        format!("INSERT INTO {} () VALUES ()", self.table_name(table))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn insert_defaults_statement() {
        assert_eq!(
            mysql().insert_defaults(&TableRef::new("shop", "t")),
            "INSERT INTO `shop`.`t` () VALUES ()"
        );
    }
}
