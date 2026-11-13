//! Text commands of the SQL editor that need to know what is SQL and what
//! is data: changing the case of a selection, and block comments.

use std::ops::Range;

use super::lex::{LexRules, TokenKind, lex};

/// The text of `range` of `text` in upper case (`upper`) or lower case
/// (Eclipse's Format ▸ To Upper / Lower Case, which DBeaver's editor
/// offers). String literals, quoted identifiers and `:name` parameters
/// keep their case: changing it would change the data, the name or the
/// parameter. The whole text is lexed, so a selection that starts inside a
/// string is still known to be in one.
pub fn change_case(text: &str, range: Range<usize>, upper: bool, rules: LexRules) -> String {
    let mut out = String::with_capacity(range.len());
    for token in lex(text, rules) {
        let start = token.range.start.max(range.start);
        let end = token.range.end.min(range.end);
        if start >= end {
            continue;
        }
        let part = &text[start..end];
        let keep = matches!(
            token.kind,
            TokenKind::String | TokenKind::QuotedIdent | TokenKind::NamedParam
        );
        if keep {
            out.push_str(part);
        } else if upper {
            out.push_str(&part.to_uppercase());
        } else {
            out.push_str(&part.to_lowercase());
        }
    }
    out
}

/// Wraps `selected` in `/* */`, or takes the markers off when it is
/// already one block comment (DBeaver's `comment.multi`). The space around
/// it and one space inside each marker are kept as they are.
pub fn toggle_block_comment(selected: &str) -> String {
    let body = selected.trim();
    let lead = &selected[..selected.len() - selected.trim_start().len()];
    let trail = &selected[selected.trim_end().len()..];
    if body.len() >= 4 && body.starts_with("/*") && body.ends_with("*/") {
        let inner = &body[2..body.len() - 2];
        let inner = inner.strip_prefix(' ').unwrap_or(inner);
        let inner = inner.strip_suffix(' ').unwrap_or(inner);
        return format!("{lead}{inner}{trail}");
    }
    format!("{lead}/* {body} */{trail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upper(text: &str) -> String {
        change_case(text, 0..text.len(), true, LexRules::POSTGRES)
    }

    #[test]
    fn case_changes_words_but_not_strings_quoted_names_or_parameters() {
        assert_eq!(
            upper("select \"Mixed\", 'Text', name from t where id = :Id -- note"),
            "SELECT \"Mixed\", 'Text', NAME FROM T WHERE ID = :Id -- NOTE"
        );
        let text = "SELECT 'A' FROM T";
        assert_eq!(
            change_case(text, 0..text.len(), false, LexRules::POSTGRES),
            "select 'A' from t"
        );
        // MySQL quotes names with backticks and strings with double quotes.
        let text = "select `Col`, \"Text\" from t";
        assert_eq!(
            change_case(text, 0..text.len(), true, LexRules::MYSQL),
            "SELECT `Col`, \"Text\" FROM T"
        );
        // Dollar-quoted bodies are strings.
        assert_eq!(upper("do $$ begin end $$"), "DO $$ begin end $$");
    }

    #[test]
    fn only_the_range_comes_back_and_a_string_is_known_from_outside_it() {
        let text = "select 'abc def' from t";
        // From inside the string to the end: the string's part is kept.
        let range = 12..text.len();
        assert_eq!(&text[range.clone()], "def' from t");
        assert_eq!(
            change_case(text, range, true, LexRules::POSTGRES),
            "def' FROM T"
        );
        assert_eq!(change_case(text, 0..3, true, LexRules::POSTGRES), "SEL");
        assert_eq!(change_case(text, 4..4, true, LexRules::POSTGRES), "");
        // Letters beyond ASCII change too.
        assert_eq!(upper("été"), "ÉTÉ");
    }

    #[test]
    fn a_block_comment_wraps_and_unwraps() {
        assert_eq!(toggle_block_comment("select 1"), "/* select 1 */");
        assert_eq!(toggle_block_comment("/* select 1 */"), "select 1");
        assert_eq!(toggle_block_comment("/*select 1*/"), "select 1");
        // The space around the selection stays outside the markers.
        assert_eq!(
            toggle_block_comment("  select 1\n  from t\n"),
            "  /* select 1\n  from t */\n"
        );
        assert_eq!(
            toggle_block_comment("  /* select 1\n  from t */\n"),
            "  select 1\n  from t\n"
        );
        assert_eq!(toggle_block_comment(""), "/*  */");
        assert_eq!(toggle_block_comment("/*  */"), "");
        // `/*/` is not a comment of its own.
        assert_eq!(toggle_block_comment("/*/"), "/* /*/ */");
    }
}
