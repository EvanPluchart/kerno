//! SQL text helpers that need no server: lexing, statement splitting,
//! parameters, the statements behind pending edits, and statement
//! templates for a table.

pub mod classify;
pub mod complete;
pub mod edits;
pub mod format;
pub mod generate;
pub mod lex;
pub mod params;
pub mod split;
pub mod text;

pub use classify::{StatementClass, StatementKind, TxCommand, classify};
pub use complete::{
    Catalog, CatalogColumn, CatalogForeignKey, CatalogRelation, CatalogSchema, Completion,
    CompletionItem, CompletionKind, complete,
};
pub use edits::{ApplyFailure, CellValue, RowChange, apply_edits, edit_statements};
pub use format::format_sql;
pub use generate::{
    RowSql, call_routine, delete_template, insert_template, rows_sql, select_template,
    update_template,
};
pub use lex::{LexRules, Token, TokenKind, lex};
pub use params::{Param, ParamKey, ParamValue, bind_params, find_params};
pub use split::{StatementSpan, split_statements, statement_at};
pub use text::{change_case, toggle_block_comment};
