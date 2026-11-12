//! SQL text helpers that need no server: lexing, statement splitting.

pub mod lex;
pub mod split;

pub use lex::{LexRules, Token, TokenKind, lex};
pub use split::{StatementSpan, split_statements, statement_at};
