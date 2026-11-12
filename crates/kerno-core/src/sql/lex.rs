//! A tolerant SQL lexer.
//!
//! It only needs to know where strings, quoted identifiers, comments,
//! parameters and separators are, for whatever the user is typing: it never
//! fails, and an unterminated string or comment simply runs to the end of
//! the text. Every token carries its byte range in the source.

use std::ops::Range;

/// Lexical rules that differ between engines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LexRules {
    /// `$tag$ ... $tag$` strings and `$1` parameters (PostgreSQL).
    pub dollar_quotes: bool,
    /// Backslash escapes inside every quoted string (MySQL). When off, only
    /// `E'...'` strings have them.
    pub backslash_escapes: bool,
    /// `#` starts a line comment (MySQL).
    pub hash_comments: bool,
    /// `/* /* */ */` nests (PostgreSQL).
    pub nested_comments: bool,
    /// `` `name` `` quotes an identifier (MySQL).
    pub backtick_identifiers: bool,
    /// `"text"` is a string rather than an identifier (MySQL default).
    pub double_quoted_strings: bool,
    /// `?` is a positional parameter (MySQL); elsewhere it is an operator.
    pub question_parameters: bool,
    /// The client-side `DELIMITER` command changes the statement separator (MySQL).
    pub delimiter_command: bool,
}

impl LexRules {
    /// PostgreSQL.
    pub const POSTGRES: Self = Self {
        dollar_quotes: true,
        backslash_escapes: false,
        hash_comments: false,
        nested_comments: true,
        backtick_identifiers: false,
        double_quoted_strings: false,
        question_parameters: false,
        delimiter_command: false,
    };

    /// MySQL and MariaDB.
    pub const MYSQL: Self = Self {
        dollar_quotes: false,
        backslash_escapes: true,
        hash_comments: true,
        nested_comments: false,
        backtick_identifiers: true,
        double_quoted_strings: true,
        question_parameters: true,
        delimiter_command: true,
    };
}

/// What a token is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenKind {
    /// Spaces, tabs, newlines.
    Whitespace,
    /// `-- ...` or `# ...` up to the end of the line.
    LineComment,
    /// `/* ... */`.
    BlockComment,
    /// A quoted string, including dollar-quoted ones.
    String,
    /// A quoted identifier.
    QuotedIdent,
    /// A keyword or bare identifier.
    Word,
    /// A numeric literal.
    Number,
    /// `:name`.
    NamedParam,
    /// `$1` or `?`.
    PositionalParam,
    /// `;`.
    Semicolon,
    /// Any other character or operator.
    Punct,
}

impl TokenKind {
    /// Whitespace and comments carry no SQL meaning.
    pub fn is_trivia(self) -> bool {
        matches!(
            self,
            Self::Whitespace | Self::LineComment | Self::BlockComment
        )
    }
}

/// A token and where it is in the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// What the token is.
    pub kind: TokenKind,
    /// Byte range in the source text.
    pub range: Range<usize>,
}

/// Splits `source` into tokens that cover it entirely, in order.
pub fn lex(source: &str, rules: LexRules) -> Vec<Token> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut pos = 0;
    while pos < bytes.len() {
        let start = pos;
        let kind = next_token(source, &mut pos, rules);
        debug_assert!(pos > start);
        tokens.push(Token {
            kind,
            range: start..pos,
        });
    }
    tokens
}

fn is_word_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte >= 0x80
}

fn is_word_part(byte: u8) -> bool {
    is_word_start(byte) || byte.is_ascii_digit() || byte == b'$'
}

/// Advances `pos` past one token and returns its kind.
fn next_token(source: &str, pos: &mut usize, rules: LexRules) -> TokenKind {
    let bytes = source.as_bytes();
    let start = *pos;
    let byte = bytes[start];
    let peek = |offset: usize| bytes.get(start + offset).copied();

    if byte.is_ascii_whitespace() {
        *pos = scan_while(bytes, start, |b| b.is_ascii_whitespace());
        return TokenKind::Whitespace;
    }
    // `--` starts a comment. MySQL requires whitespace after it, and
    // accepting `--x` as a comment there would hide `a--b` arithmetic.
    if byte == b'-' && peek(1) == Some(b'-') {
        let needs_space = rules.hash_comments;
        if !needs_space || peek(2).is_none_or(|b| b.is_ascii_whitespace()) {
            *pos = scan_while(bytes, start, |b| b != b'\n');
            return TokenKind::LineComment;
        }
    }
    if byte == b'#' && rules.hash_comments {
        *pos = scan_while(bytes, start, |b| b != b'\n');
        return TokenKind::LineComment;
    }
    if byte == b'/' && peek(1) == Some(b'*') {
        *pos = scan_block_comment(bytes, start, rules.nested_comments);
        return TokenKind::BlockComment;
    }
    if byte == b'\'' {
        *pos = scan_quoted(bytes, start, b'\'', rules.backslash_escapes);
        return TokenKind::String;
    }
    if byte == b'"' {
        *pos = scan_quoted(
            bytes,
            start,
            b'"',
            rules.backslash_escapes && rules.double_quoted_strings,
        );
        return if rules.double_quoted_strings {
            TokenKind::String
        } else {
            TokenKind::QuotedIdent
        };
    }
    if byte == b'`' && rules.backtick_identifiers {
        *pos = scan_quoted(bytes, start, b'`', false);
        return TokenKind::QuotedIdent;
    }
    if byte == b'$' && rules.dollar_quotes {
        if peek(1).is_some_and(|b| b.is_ascii_digit()) {
            *pos = scan_while(bytes, start + 1, |b| b.is_ascii_digit());
            return TokenKind::PositionalParam;
        }
        if let Some(end) = scan_dollar_quoted(bytes, start) {
            *pos = end;
            return TokenKind::String;
        }
    }
    if byte == b'?' && rules.question_parameters {
        *pos = start + 1;
        return TokenKind::PositionalParam;
    }
    if byte == b':' {
        // `::` is a cast and `:=` an assignment, not parameters.
        if peek(1) == Some(b':') {
            *pos = start + 2;
            return TokenKind::Punct;
        }
        if peek(1).is_some_and(is_word_start) {
            *pos = scan_while(bytes, start + 1, is_word_part);
            return TokenKind::NamedParam;
        }
    }
    if byte == b';' {
        *pos = start + 1;
        return TokenKind::Semicolon;
    }
    if byte.is_ascii_digit() {
        let mut end = scan_while(bytes, start, |b| {
            b.is_ascii_alphanumeric() || b == b'.' || b == b'_'
        });
        // The exponent's sign belongs to a decimal number: `1.5e-3` (but
        // `0x1E-3` is a subtraction).
        if let Some((b'e' | b'E', mantissa)) = bytes[start..end].split_last()
            && mantissa
                .iter()
                .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'_'))
            && matches!(bytes.get(end), Some(b'+' | b'-'))
            && bytes.get(end + 1).is_some_and(u8::is_ascii_digit)
        {
            end = scan_while(bytes, end + 1, |b| b.is_ascii_digit() || b == b'_');
        }
        *pos = end;
        return TokenKind::Number;
    }
    if is_word_start(byte) {
        let end = scan_while(bytes, start, is_word_part);
        // PostgreSQL Unicode escapes: U&'...' strings and U&"..." names.
        if end == start + 1
            && (byte == b'U' || byte == b'u')
            && bytes.get(end) == Some(&b'&')
            && !rules.backslash_escapes
        {
            match bytes.get(end + 1) {
                Some(b'\'') => {
                    *pos = scan_quoted(bytes, end + 1, b'\'', false);
                    return TokenKind::String;
                }
                Some(b'"') if !rules.double_quoted_strings => {
                    *pos = scan_quoted(bytes, end + 1, b'"', false);
                    return TokenKind::QuotedIdent;
                }
                _ => {}
            }
        }
        // PostgreSQL escape strings: E'...' has backslash escapes.
        if end == start + 1
            && (byte == b'E' || byte == b'e')
            && bytes.get(end) == Some(&b'\'')
            && !rules.backslash_escapes
        {
            *pos = scan_quoted(bytes, end, b'\'', true);
            return TokenKind::String;
        }
        *pos = end;
        return TokenKind::Word;
    }
    // Any other character, taken whole so multi-byte UTF-8 is never split.
    let width = source[start..].chars().next().map_or(1, char::len_utf8);
    *pos = start + width;
    TokenKind::Punct
}

fn scan_while(bytes: &[u8], from: usize, keep: impl Fn(u8) -> bool) -> usize {
    let mut pos = from;
    while pos < bytes.len() && keep(bytes[pos]) {
        pos += 1;
    }
    // The first byte always belongs to the token.
    pos.max(from + 1).min(bytes.len())
}

/// Scans a quoted run starting at the opening quote. A doubled quote is an
/// escaped quote; with `backslash`, so is a backslash-escaped one.
fn scan_quoted(bytes: &[u8], start: usize, quote: u8, backslash: bool) -> usize {
    let mut pos = start + 1;
    while pos < bytes.len() {
        let byte = bytes[pos];
        if backslash && byte == b'\\' {
            pos += 2;
        } else if byte == quote {
            if bytes.get(pos + 1) == Some(&quote) {
                pos += 2;
            } else {
                return pos + 1;
            }
        } else {
            pos += 1;
        }
    }
    bytes.len()
}

fn scan_block_comment(bytes: &[u8], start: usize, nested: bool) -> usize {
    let mut depth = 1usize;
    let mut pos = start + 2;
    while pos < bytes.len() {
        if bytes[pos] == b'*' && bytes.get(pos + 1) == Some(&b'/') {
            depth -= 1;
            pos += 2;
            if depth == 0 {
                return pos;
            }
        } else if nested && bytes[pos] == b'/' && bytes.get(pos + 1) == Some(&b'*') {
            depth += 1;
            pos += 2;
        } else {
            pos += 1;
        }
    }
    bytes.len()
}

/// Scans `$tag$ ... $tag$`. Returns `None` when `start` is not the opening
/// of a dollar-quoted string.
fn scan_dollar_quoted(bytes: &[u8], start: usize) -> Option<usize> {
    let mut tag_end = start + 1;
    while tag_end < bytes.len() && is_word_part(bytes[tag_end]) && bytes[tag_end] != b'$' {
        tag_end += 1;
    }
    if bytes.get(tag_end) != Some(&b'$') {
        return None;
    }
    // A tag cannot start with a digit (`$1$` is not a quote opener).
    if bytes.get(start + 1).is_some_and(u8::is_ascii_digit) {
        return None;
    }
    let delimiter = &bytes[start..=tag_end];
    let body = tag_end + 1;
    let close = bytes[body..]
        .windows(delimiter.len())
        .position(|window| window == delimiter);
    Some(match close {
        Some(offset) => body + offset + delimiter.len(),
        None => bytes.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use TokenKind::*;

    fn kinds(source: &str, rules: LexRules) -> Vec<(TokenKind, &str)> {
        lex(source, rules)
            .into_iter()
            .filter(|token| token.kind != Whitespace)
            .map(|token| (token.kind, &source[token.range]))
            .collect()
    }

    #[test]
    fn tokens_cover_the_source_exactly() {
        let sources = [
            "select 'a''b', \"Id\" from t; -- done\n/* x */ $$ body; $$ é ☃",
            "'unterminated",
            "/* unterminated",
            "$tag$ unterminated",
            "",
            ";;;",
            "\u{1F600}",
        ];
        for rules in [LexRules::POSTGRES, LexRules::MYSQL] {
            for source in sources {
                let tokens = lex(source, rules);
                let mut pos = 0;
                for token in &tokens {
                    assert_eq!(token.range.start, pos, "{source:?}");
                    assert!(source.is_char_boundary(token.range.end), "{source:?}");
                    pos = token.range.end;
                }
                assert_eq!(pos, source.len(), "{source:?}");
            }
        }
    }

    #[test]
    fn strings_keep_semicolons_and_quotes_inside() {
        assert_eq!(
            kinds("select 'a;''b' ; x", LexRules::POSTGRES),
            vec![
                (Word, "select"),
                (String, "'a;''b'"),
                (Semicolon, ";"),
                (Word, "x")
            ]
        );
    }

    #[test]
    fn backslash_escapes_depend_on_the_engine() {
        // PostgreSQL: a backslash is an ordinary character, the string ends
        // at the next quote.
        assert_eq!(
            kinds(r"'a\' ;", LexRules::POSTGRES),
            vec![(String, r"'a\'"), (Semicolon, ";")]
        );
        // ... except in E'' strings.
        assert_eq!(
            kinds(r"E'a\';' ;", LexRules::POSTGRES),
            vec![(String, r"E'a\';'"), (Semicolon, ";")]
        );
        // MySQL: the backslash escapes the quote.
        assert_eq!(
            kinds(r"'a\';' ;", LexRules::MYSQL),
            vec![(String, r"'a\';'"), (Semicolon, ";")]
        );
    }

    #[test]
    fn dollar_quoted_strings_hide_their_body() {
        assert_eq!(
            kinds(
                "do $body$ begin; 'x'; end $body$; select $$a$$",
                LexRules::POSTGRES
            ),
            vec![
                (Word, "do"),
                (String, "$body$ begin; 'x'; end $body$"),
                (Semicolon, ";"),
                (Word, "select"),
                (String, "$$a$$"),
            ]
        );
        // MySQL has no dollar quoting: `$$` is punctuation (and a common
        // custom delimiter).
        assert_eq!(
            kinds("a $$ b", LexRules::MYSQL),
            vec![(Word, "a"), (Punct, "$"), (Punct, "$"), (Word, "b")]
        );
    }

    #[test]
    fn comments_by_engine() {
        assert_eq!(
            kinds("a -- c;\nb /* x /* y */ ; */ c", LexRules::POSTGRES),
            vec![
                (Word, "a"),
                (LineComment, "-- c;"),
                (Word, "b"),
                (BlockComment, "/* x /* y */ ; */"),
                (Word, "c"),
            ]
        );
        assert_eq!(
            kinds("a # c;\nb /* x /* y */ c", LexRules::MYSQL),
            vec![
                (Word, "a"),
                (LineComment, "# c;"),
                (Word, "b"),
                (BlockComment, "/* x /* y */"),
                (Word, "c"),
            ]
        );
        // MySQL: `--` without a following space is arithmetic, not a comment.
        assert_eq!(
            kinds("5--3", LexRules::MYSQL),
            vec![(Number, "5"), (Punct, "-"), (Punct, "-"), (Number, "3")]
        );
        assert_eq!(
            kinds("5--3", LexRules::POSTGRES),
            vec![(Number, "5"), (LineComment, "--3")]
        );
    }

    #[test]
    fn parameters_are_told_apart_from_casts_and_operators() {
        assert_eq!(
            kinds(
                "a = :name and b = $2 and c::int = '1' and d := 3",
                LexRules::POSTGRES
            ),
            vec![
                (Word, "a"),
                (Punct, "="),
                (NamedParam, ":name"),
                (Word, "and"),
                (Word, "b"),
                (Punct, "="),
                (PositionalParam, "$2"),
                (Word, "and"),
                (Word, "c"),
                (Punct, "::"),
                (Word, "int"),
                (Punct, "="),
                (String, "'1'"),
                (Word, "and"),
                (Word, "d"),
                (Punct, ":"),
                (Punct, "="),
                (Number, "3"),
            ]
        );
        // `?` is a parameter on MySQL and a JSON operator on PostgreSQL.
        assert_eq!(kinds("?", LexRules::MYSQL), vec![(PositionalParam, "?")]);
        assert_eq!(kinds("?", LexRules::POSTGRES), vec![(Punct, "?")]);
        // Parameters inside strings and comments are not parameters.
        assert_eq!(
            kinds("':a' -- :b", LexRules::POSTGRES),
            vec![(String, "':a'"), (LineComment, "-- :b")]
        );
    }

    #[test]
    fn exponents_and_unicode_escapes_are_one_token() {
        assert_eq!(
            kinds("1.5e-3 2E+10 0x1E-3 1e", LexRules::POSTGRES),
            vec![
                (Number, "1.5e-3"),
                (Number, "2E+10"),
                (Number, "0x1E"),
                (Punct, "-"),
                (Number, "3"),
                (Number, "1e"),
            ]
        );
        assert_eq!(
            kinds(
                "U&'d\\0061t''a' u&\"n\\0061me\" u & 'x'",
                LexRules::POSTGRES
            ),
            vec![
                (String, "U&'d\\0061t''a'"),
                (QuotedIdent, "u&\"n\\0061me\""),
                (Word, "u"),
                (Punct, "&"),
                (String, "'x'"),
            ]
        );
        // On MySQL `&` is bitwise and.
        assert_eq!(
            kinds("U&'x'", LexRules::MYSQL),
            vec![(Word, "U"), (Punct, "&"), (String, "'x'")]
        );
    }

    #[test]
    fn identifier_quoting_by_engine() {
        assert_eq!(
            kinds("\"a;b\" `c`", LexRules::POSTGRES),
            vec![
                (QuotedIdent, "\"a;b\""),
                (Punct, "`"),
                (Word, "c"),
                (Punct, "`")
            ]
        );
        assert_eq!(
            kinds("\"a;b\" `c;d`", LexRules::MYSQL),
            vec![(String, "\"a;b\""), (QuotedIdent, "`c;d`")]
        );
    }
}
