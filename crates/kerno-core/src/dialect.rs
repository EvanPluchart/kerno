//! What differs between engines in the SQL Kerno writes.

use crate::config::Engine;
use crate::schema::TableRef;
use crate::sql::LexRules;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_encodes_every_byte() {
        assert_eq!(hex(&[]), "");
        assert_eq!(hex(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
    }
}
