//! SQL text helpers that need no server: lexing, statement splitting,
//! parameters, and the statements behind pending edits.

pub mod edits;
pub mod lex;
pub mod params;
pub mod split;

pub use edits::{ApplyFailure, CellValue, RowChange, apply_edits, edit_statements};
pub use lex::{LexRules, Token, TokenKind, lex};
pub use params::{Param, ParamKey, ParamValue, bind_params, find_params};
pub use split::{StatementSpan, split_statements, statement_at};
