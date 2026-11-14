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

pub use classify::{
    NOT_A_SELECT, StatementClass, StatementKind, TxCommand, classify, single_table_select,
};
pub use complete::{
    Catalog, CatalogColumn, CatalogForeignKey, CatalogRelation, CatalogSchema, Completion,
    CompletionItem, CompletionKind, complete,
};
pub use edits::{
    ApplyFailure, CellValue, RowChange, apply_edits, apply_edits_in_transaction, edit_statements,
};
pub use format::{
    Comma, FormatOptions, Indent, KeywordCase, format_cleanly, format_sql, format_with,
};
pub use generate::{
    RowSql, call_routine, delete_template, insert_template, rows_sql, select_template,
    update_template,
};
pub use lex::{LexRules, Token, TokenKind, lex};
pub use params::{Param, ParamKey, ParamValue, bind_params, find_params};
pub use split::{StatementSpan, split_statements, statement_at};
pub use text::{change_case, matching_bracket, occurrences, toggle_block_comment};
