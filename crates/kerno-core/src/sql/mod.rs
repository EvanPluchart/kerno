//! SQL text helpers that need no server: lexing, statement splitting,
//! parameters, the statements behind pending edits, and statement
//! templates for a table.

pub mod classify;
pub mod edits;
pub mod generate;
pub mod lex;
pub mod params;
pub mod split;

pub use classify::{StatementClass, StatementKind, TxCommand, classify};
pub use edits::{ApplyFailure, CellValue, RowChange, apply_edits, edit_statements};
pub use generate::{delete_template, insert_template, select_template, update_template};
pub use lex::{LexRules, Token, TokenKind, lex};
pub use params::{Param, ParamKey, ParamValue, bind_params, find_params};
pub use split::{StatementSpan, split_statements, statement_at};
