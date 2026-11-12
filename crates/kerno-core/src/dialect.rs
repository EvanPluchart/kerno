//! What differs between engines in the SQL Kerno writes.

use crate::config::Engine;
use crate::result::TypeKind;
use crate::schema::{ColumnInfo, TableRef};
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

    /// A literal holding `value`, to store in `column` or compare with it.
    ///
    /// Text is written as a string literal and left to the server to
    /// convert to the column's type, except that plain numbers for numeric
    /// columns and the usual spellings of booleans are written bare.
    /// Dialects override this for types whose literal needs another form.
    fn literal(&self, value: &CellValue, column: &ColumnInfo) -> String {
        match value {
            CellValue::Null => "NULL".into(),
            CellValue::Default => "DEFAULT".into(),
            CellValue::Bytes(bytes) => self.quote_bytes(bytes),
            CellValue::Text(text) => match column.kind {
                kind if kind.is_numeric() && is_number(text) => text.trim().to_string(),
                TypeKind::Boolean if ["t", "true"].iter().any(|v| text.eq_ignore_ascii_case(v)) => {
                    "TRUE".into()
                }
                TypeKind::Boolean
                    if ["f", "false"].iter().any(|v| text.eq_ignore_ascii_case(v)) =>
                {
                    "FALSE".into()
                }
                _ => self.quote_string(text),
            },
        }
    }

    /// An `INSERT` that gives every column its default.
    fn insert_defaults(&self, table: &TableRef) -> String {
        format!("INSERT INTO {} DEFAULT VALUES", self.table_name(table))
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

    #[test]
    fn hex_encodes_every_byte() {
        assert_eq!(hex(&[]), "");
        assert_eq!(hex(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
    }
}
