//! PostgreSQL's SQL dialect.

use kerno_core::Engine;
use kerno_core::dialect::{Dialect, hex};
use kerno_core::sql::LexRules;

/// The PostgreSQL dialect.
#[derive(Debug, Clone, Copy, Default)]
pub struct PgDialect;

impl Dialect for PgDialect {
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
        if text.contains('\\') {
            // An escape string means the same thing whatever
            // standard_conforming_strings is set to.
            format!("E'{}'", text.replace('\\', "\\\\").replace('\'', "''"))
        } else {
            format!("'{}'", text.replace('\'', "''"))
        }
    }

    fn quote_bytes(&self, bytes: &[u8]) -> String {
        // decode() rather than a '\x..' literal, which depends on
        // standard_conforming_strings.
        format!("decode('{}', 'hex')", hex(bytes))
    }

    fn explain(&self, sql: &str, analyze: bool) -> String {
        if analyze {
            format!("EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) {sql}")
        } else {
            format!("EXPLAIN (FORMAT JSON) {sql}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kerno_core::schema::TableRef;

    #[test]
    fn identifiers_are_always_quoted() {
        let d = PgDialect;
        assert_eq!(d.quote_ident("orders"), "\"orders\"");
        assert_eq!(d.quote_ident("select"), "\"select\"");
        assert_eq!(d.quote_ident("Weird \"Name\""), "\"Weird \"\"Name\"\"\"");
        assert_eq!(
            d.table_name(&TableRef::new("shop", "order items")),
            "\"shop\".\"order items\""
        );
    }

    #[test]
    fn strings_escape_quotes_and_backslashes() {
        let d = PgDialect;
        assert_eq!(d.quote_string(""), "''");
        assert_eq!(d.quote_string("it's"), "'it''s'");
        assert_eq!(d.quote_string(r"a\b'c"), r"E'a\\b''c'");
        assert_eq!(d.quote_string("multi\nline"), "'multi\nline'");
    }

    #[test]
    fn bytes_and_paging() {
        let d = PgDialect;
        assert_eq!(
            d.quote_bytes(&[0xde, 0xad, 0x00]),
            "decode('dead00', 'hex')"
        );
        assert_eq!(d.quote_bytes(&[]), "decode('', 'hex')");
        assert_eq!(d.limit_offset(100, 0), "LIMIT 100");
        assert_eq!(d.limit_offset(100, 300), "LIMIT 100 OFFSET 300");
    }

    #[test]
    fn explain_forms() {
        let d = PgDialect;
        assert_eq!(
            d.explain("select 1", false),
            "EXPLAIN (FORMAT JSON) select 1"
        );
        assert_eq!(
            d.explain("select 1", true),
            "EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) select 1"
        );
    }
}
