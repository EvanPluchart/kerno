//! "Format SQL" for the editor.
//!
//! [`format_sql`] works on tokens, not on a parse tree: it lays out whatever
//! the editor holds, including statements the server would reject, and it
//! never touches a comment, a string, a quoted name or a dollar-quoted body.
//! It only changes whitespace and the letter case of keywords. Each
//! statement is checked after the fact: if its tokens are not the ones that
//! came in, it is returned as it was written.

use std::borrow::Cow;

use super::complete::{runs_on, upper};
use super::lex::{LexRules, Token, TokenKind, lex};

const INDENT: &str = "    ";

/// Keywords written in upper case, sorted. The flag is set for words MySQL
/// does not reserve: there they can name a table without quotes, and table
/// names are case-sensitive on most servers, so they are left alone right
/// after `FROM`, `JOIN`, `INTO`, `UPDATE` and `TABLE`.
///
/// Type and function names are not here on purpose, and neither is any word
/// that commonly names a table or a column (`user`, `status`, `date` ...).
const KEYWORDS: &[(&str, bool)] = &[
    ("ADD", false),
    ("ALL", false),
    ("ALTER", false),
    ("ANALYZE", false),
    ("AND", false),
    ("ANY", true),
    ("AS", false),
    ("ASC", false),
    ("BEGIN", true),
    ("BETWEEN", false),
    ("BY", false),
    ("CASCADE", false),
    ("CASE", false),
    ("CHECK", false),
    ("COLUMN", false),
    ("COMMIT", true),
    ("CONFLICT", true),
    ("CONSTRAINT", false),
    ("CREATE", false),
    ("CROSS", false),
    ("DEFAULT", false),
    ("DELETE", false),
    ("DESC", false),
    ("DESCRIBE", false),
    ("DISTINCT", false),
    ("DO", true),
    ("DROP", false),
    ("DUPLICATE", true),
    ("ELSE", false),
    ("END", true),
    ("ESCAPE", true),
    ("EXCEPT", false),
    ("EXISTS", false),
    ("EXPLAIN", false),
    ("FALSE", false),
    ("FOR", false),
    ("FOREIGN", false),
    ("FROM", false),
    ("FULL", false),
    ("GRANT", false),
    ("GROUP", false),
    ("HAVING", false),
    ("IF", false),
    ("ILIKE", true),
    ("IN", false),
    ("INNER", false),
    ("INSERT", false),
    ("INTERSECT", false),
    ("INTO", false),
    ("IS", false),
    ("JOIN", false),
    ("LATERAL", false),
    ("LEFT", false),
    ("LIKE", false),
    ("LIMIT", false),
    ("MATERIALIZED", true),
    ("NATURAL", false),
    ("NOT", false),
    ("NOTHING", true),
    ("NULL", false),
    ("NULLS", true),
    ("OFFSET", true),
    ("ON", false),
    ("ONLY", true),
    ("OR", false),
    ("ORDER", false),
    ("OUTER", false),
    ("OVER", false),
    ("PARTITION", false),
    ("PRIMARY", false),
    ("RECURSIVE", false),
    ("REFERENCES", false),
    ("REPLACE", false),
    ("RETURNING", true),
    ("RIGHT", false),
    ("ROLLBACK", true),
    ("SELECT", false),
    ("SET", false),
    ("SOME", true),
    ("TABLE", false),
    ("TEMPORARY", true),
    ("THEN", false),
    ("TO", false),
    ("TRUE", false),
    ("TRUNCATE", true),
    ("UNION", false),
    ("UNIQUE", false),
    ("UPDATE", false),
    ("USING", false),
    ("VALUES", false),
    ("WHEN", false),
    ("WHERE", false),
    ("WINDOW", false),
    ("WITH", false),
];

/// Words that are keywords only right after certain keywords, as `(words,
/// keywords before)`: they are common column names (`index`, `schema`,
/// `key`, `first`).
const CONTEXTUAL: &[(&[&str], &[&str])] = &[
    // The kind of object a statement creates, alters or drops.
    (
        &[
            "DATABASE",
            "FUNCTION",
            "INDEX",
            "PROCEDURE",
            "SCHEMA",
            "TRIGGER",
            "VIEW",
        ],
        &[
            "CREATE",
            "DROP",
            "ALTER",
            "REPLACE",
            "UNIQUE",
            "MATERIALIZED",
            "TEMPORARY",
            "RECURSIVE",
        ],
    ),
    (&["KEY"], &["PRIMARY", "FOREIGN", "UNIQUE", "DUPLICATE"]),
    (&["FIRST", "LAST"], &["NULLS"]),
];

/// Formats the SQL text of an editor: keywords in upper case, one clause
/// per line, lists and conditions indented, one blank line between
/// statements, a newline at the end.
///
/// Only whitespace and the letter case of keywords change. Comments,
/// strings, quoted identifiers and dollar-quoted bodies come back byte for
/// byte, MySQL `DELIMITER` sections are not touched at all, and formatting
/// twice gives the same text as formatting once.
pub fn format_sql(sql: &str, rules: LexRules) -> String {
    format_counting(sql, rules).0
}

/// [`format_sql`], and how many statements were kept as written because
/// laying them out would have changed their tokens.
fn format_counting(sql: &str, rules: LexRules) -> (String, usize) {
    let tokens = lex(sql, rules);
    let mut out = String::with_capacity(sql.len() + sql.len() / 8);
    let mut kept = 0;
    // What separates one piece (a statement, a comment standing alone, a
    // DELIMITER section) from the next.
    let mut after_comment = false;
    let mut blank_line = false;
    let mut index = 0;
    while index < tokens.len() {
        let token = &tokens[index];
        if token.kind == TokenKind::Whitespace {
            blank_line |= sql[token.range.clone()].matches('\n').count() > 1;
            index += 1;
            continue;
        }
        let start = index;
        let is_comment = token.kind.is_trivia();
        let piece = if is_comment {
            index += 1;
            Cow::Borrowed(&sql[token.range.clone()])
        } else if rules.delimiter_command
            && let Some(region_end) = delimiter_region(sql, token)
        {
            let mut end = token.range.end;
            while index < tokens.len() && tokens[index].range.start < region_end {
                if tokens[index].kind != TokenKind::Whitespace {
                    end = tokens[index].range.end;
                }
                index += 1;
            }
            Cow::Borrowed(&sql[token.range.start..end])
        } else {
            index = statement_end(sql, &tokens, index);
            let statement = &tokens[start..index];
            let formatted = Statement::new(sql, statement, rules).layout();
            if same_tokens(sql, statement, &formatted, rules) {
                Cow::Owned(formatted)
            } else {
                kept += 1;
                let end = statement
                    .last()
                    .map_or(token.range.end, |last| last.range.end);
                Cow::Borrowed(&sql[token.range.start..end])
            }
        };
        if !out.is_empty() {
            // A comment stays right above what it was written above.
            out.push_str(if after_comment && !blank_line {
                "\n"
            } else {
                "\n\n"
            });
        }
        out.push_str(&piece);
        after_comment = is_comment;
        blank_line = false;
    }
    // A string, quoted name or comment left open runs to the end of the
    // text: a newline after it would become part of it.
    let open_ended = tokens.last().is_some_and(|last| {
        matches!(
            last.kind,
            TokenKind::String | TokenKind::QuotedIdent | TokenKind::BlockComment
        ) && runs_on(&sql[last.range.clone()], rules)
    });
    if !out.is_empty() && !open_ended {
        out.push('\n');
    }
    (out, kept)
}

/// When `token` is the MySQL client's `DELIMITER` command, the end of the
/// text it governs: its own line for `DELIMITER ;`, else everything up to
/// and including the line that switches back to `;`. What is written under
/// another delimiter is a routine body: it is left as it is.
fn delimiter_region(sql: &str, token: &Token) -> Option<usize> {
    if token.kind != TokenKind::Word || !sql[token.range.clone()].eq_ignore_ascii_case("delimiter")
    {
        return None;
    }
    let line_end = |from: usize| sql[from..].find('\n').map_or(sql.len(), |at| from + at);
    let mut end = line_end(token.range.end);
    let mut delimiter = sql[token.range.end..end].split_whitespace().next()?;
    while delimiter != ";" && end < sql.len() {
        let start = end + 1;
        end = line_end(start);
        let line = sql[start..end].trim_start();
        if let Some(command) = line.get(..9)
            && command.eq_ignore_ascii_case("delimiter")
            && line[9..].starts_with(char::is_whitespace)
            && let Some(next) = line[9..].split_whitespace().next()
        {
            delimiter = next;
        }
    }
    Some(end)
}

/// Index just past the statement that starts at `start`: its `;` and the
/// comments written after it on the same line.
fn statement_end(sql: &str, tokens: &[Token], start: usize) -> usize {
    let mut end = start;
    while end < tokens.len() && tokens[end].kind != TokenKind::Semicolon {
        end += 1;
    }
    if end == tokens.len() {
        while tokens[end - 1].kind == TokenKind::Whitespace {
            end -= 1;
        }
        return end;
    }
    end += 1;
    loop {
        let mut next = end;
        if tokens.get(next).is_some_and(|token| {
            token.kind == TokenKind::Whitespace && !sql[token.range.clone()].contains('\n')
        }) {
            next += 1;
        }
        match tokens.get(next) {
            Some(token) if token.kind.is_trivia() && token.kind != TokenKind::Whitespace => {
                end = next + 1;
            }
            _ => return end,
        }
    }
}

/// Whether `formatted` holds the tokens of the statement it was made from:
/// the same kinds and text, keywords in upper case aside.
fn same_tokens(sql: &str, statement: &[Token], formatted: &str, rules: LexRules) -> bool {
    let after = lex(formatted, rules);
    let mut after = after
        .iter()
        .filter(|token| token.kind != TokenKind::Whitespace);
    for token in statement {
        if token.kind == TokenKind::Whitespace {
            continue;
        }
        let Some(other) = after.next() else {
            return false;
        };
        let (was, is) = (&sql[token.range.clone()], &formatted[other.range.clone()]);
        let upper_cased = token.kind == TokenKind::Word
            && is.eq_ignore_ascii_case(was)
            && !is.bytes().any(|byte| byte.is_ascii_lowercase());
        if other.kind != token.kind || !(was == is || upper_cased) {
            return false;
        }
    }
    after.next().is_none()
}

/// A token of a statement, comments included.
struct Tok<'a> {
    kind: TokenKind,
    text: &'a str,
    /// Nothing, not even a space, separated it from the token before.
    glued: bool,
    /// A line break came before it.
    newline: bool,
    /// Its line was indented.
    indented: bool,
    /// The keyword it is, in upper case.
    keyword: Option<&'static str>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Open,
    Close,
    Comma,
    Semicolon,
    Dot,
    Cast,
    Operator,
    Operand,
}

impl Tok<'_> {
    fn class(&self) -> Class {
        match self.kind {
            TokenKind::Semicolon => Class::Semicolon,
            TokenKind::Punct => match self.text {
                "(" | "[" => Class::Open,
                ")" | "]" => Class::Close,
                "," => Class::Comma,
                "." => Class::Dot,
                "::" => Class::Cast,
                _ => Class::Operator,
            },
            _ => Class::Operand,
        }
    }

    /// Whether an operator written after it has it as left operand.
    fn ends_operand(&self) -> bool {
        match self.class() {
            Class::Close => true,
            Class::Operand => {
                matches!(self.keyword, None | Some("NULL" | "TRUE" | "FALSE" | "END"))
            }
            _ => false,
        }
    }
}

/// How a run of operator characters is spaced.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Run {
    /// `a = b`: a space on both sides.
    Binary,
    /// `-1`, `@var`, `*`: glued to what follows if it was written so.
    Prefix,
    /// `U&'...'`, `1e-3`, `a[1:2]`: spaced as it was written.
    Written,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Clause {
    None,
    Select,
    Where,
    Having,
    Join,
    Set,
    Values,
    With,
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FrameKind {
    /// The statement itself.
    Top,
    /// A subquery or a CTE body: laid out like a statement, one level in.
    Query,
    /// The column list of CREATE TABLE: one definition per line.
    List,
    /// Any other parentheses or brackets: kept on one line.
    Inline,
}

/// The layout state of the statement or of one pair of parentheses.
struct Frame<'a> {
    kind: FrameKind,
    /// The opening character.
    open: &'a str,
    /// Indentation of its clause keywords.
    level: usize,
    /// Indentation of its closing parenthesis.
    close_level: usize,
    clause: Clause,
    /// Where the items of the current clause go when each has its own line.
    list_level: Option<usize>,
    /// A BETWEEN waits for its AND.
    between: bool,
    /// How many CASE were open when it started.
    case_base: usize,
}

#[derive(Clone, Copy)]
enum Sep {
    None,
    Space,
    Line(usize),
}

#[derive(Default)]
struct Out {
    text: String,
    /// Indentation of the current line.
    level: usize,
    /// Nothing is written on the current line yet.
    fresh: bool,
}

impl Out {
    fn line(&mut self, level: usize) {
        if self.text.is_empty() {
            return;
        }
        self.text.push('\n');
        for _ in 0..level {
            self.text.push_str(INDENT);
        }
        self.level = level;
        self.fresh = true;
    }

    fn space(&mut self) {
        if !self.fresh && !self.text.is_empty() {
            self.text.push(' ');
        }
    }

    fn put(&mut self, text: &str) {
        self.text.push_str(text);
        self.fresh = false;
    }
}

/// One statement: its tokens, and where the significant ones are.
struct Statement<'a> {
    toks: Vec<Tok<'a>>,
    /// Indices in `toks` of the tokens that are not comments.
    sig: Vec<usize>,
}

impl<'a> Statement<'a> {
    fn new(sql: &'a str, tokens: &[Token], rules: LexRules) -> Self {
        let mut toks = Vec::with_capacity(tokens.len());
        let mut sig = Vec::with_capacity(tokens.len());
        let mut gap = None;
        for token in tokens {
            let text = &sql[token.range.clone()];
            if token.kind == TokenKind::Whitespace {
                gap = Some(text);
                continue;
            }
            if !token.kind.is_trivia() {
                sig.push(toks.len());
            }
            toks.push(Tok {
                kind: token.kind,
                text,
                glued: gap.is_none(),
                newline: gap.is_some_and(|gap| gap.contains('\n')),
                indented: gap.is_some_and(|gap| !gap.ends_with('\n')),
                keyword: None,
            });
            gap = None;
        }
        let mut statement = Self { toks, sig };
        for k in 0..statement.sig.len() {
            let index = statement.sig[k];
            statement.toks[index].keyword = statement.keyword(k, rules.backtick_identifiers);
        }
        statement
    }

    /// The keyword of the `k`th significant token, or an empty string.
    fn kw(&self, k: usize) -> &'static str {
        self.sig
            .get(k)
            .and_then(|&index| self.toks[index].keyword)
            .unwrap_or("")
    }

    /// The text of the `k`th significant token, or an empty string.
    fn text(&self, k: usize) -> &'a str {
        self.sig.get(k).map_or("", |&index| self.toks[index].text)
    }

    /// Whether the `k`th significant token is a keyword here, and which.
    /// Keywords of the tokens before it are known already.
    fn keyword(&self, k: usize, mysql: bool) -> Option<&'static str> {
        let index = self.sig[k];
        let tok = &self.toks[index];
        if tok.kind != TokenKind::Word {
            return None;
        }
        let prev = self.kw(k.wrapping_sub(1));
        let next = self.text(k + 1);
        // Part of a qualified name.
        if self.text(k.wrapping_sub(1)) == "." || next == "." {
            return None;
        }
        let mut buffer = [0; 16];
        let word = upper(tok.text, &mut buffer);
        for (words, before) in CONTEXTUAL {
            if let Some(found) = words.iter().find(|found| **found == word) {
                return before.contains(&prev).then_some(*found);
            }
        }
        let found = KEYWORDS.binary_search_by(|(keyword, _)| (*keyword).cmp(word));
        let (word, unreserved) = KEYWORDS[found.ok()?];
        let called =
            next == "(" && self.sig.get(k + 1) == Some(&(index + 1)) && self.toks[index + 1].glued;
        let fits = match word {
            // Also functions.
            "LEFT" | "RIGHT" | "IF" | "REPLACE" | "INSERT" | "TRUNCATE" => !called,
            "FULL" => next.eq_ignore_ascii_case("join") || next.eq_ignore_ascii_case("outer"),
            _ => true,
        };
        // `AS name`: MySQL returns the name as it is written.
        let alias = prev == "AS"
            && !matches!(
                word,
                "SELECT"
                    | "WITH"
                    | "NOT"
                    | "MATERIALIZED"
                    | "VALUES"
                    | "INSERT"
                    | "UPDATE"
                    | "DELETE"
                    | "TABLE"
            );
        let table_name =
            unreserved && mysql && matches!(prev, "FROM" | "JOIN" | "INTO" | "UPDATE" | "TABLE");
        (fits && !alias && !table_name).then_some(word)
    }

    /// The clause the `k`th significant token starts, if it starts one:
    /// such a keyword goes on a new line.
    fn clause_start(&self, k: usize) -> Option<Clause> {
        let prev = self.kw(k.wrapping_sub(1));
        let prev_text = self.text(k.wrapping_sub(1));
        let next = self.kw(k + 1);
        Some(match self.kw(k) {
            "SELECT" => Clause::Select,
            "FROM"
                if prev != "DELETE"
                    && !(prev == "DISTINCT"
                        && matches!(self.kw(k.wrapping_sub(2)), "IS" | "NOT")) =>
            {
                Clause::Other
            }
            "WHERE" => Clause::Where,
            "HAVING" => Clause::Having,
            "LIMIT" | "OFFSET" | "RETURNING" | "WINDOW" | "UNION" | "INTERSECT" | "EXCEPT" => {
                Clause::Other
            }
            "SET" => Clause::Set,
            // Not `DEFAULT VALUES`, nor the MySQL function `VALUES(column)`.
            "VALUES"
                if k == 0
                    || prev != "DEFAULT"
                        && !matches!(
                            self.toks[self.sig[k - 1]].class(),
                            Class::Operator | Class::Comma | Class::Open
                        ) =>
            {
                Clause::Values
            }
            "GROUP" | "ORDER" if next == "BY" => Clause::Other,
            "JOIN"
                if !matches!(
                    prev,
                    "LEFT" | "RIGHT" | "FULL" | "INNER" | "CROSS" | "NATURAL" | "OUTER"
                ) =>
            {
                Clause::Join
            }
            "LEFT" | "RIGHT" | "FULL" | "INNER" | "CROSS"
                if matches!(next, "JOIN" | "OUTER") && prev != "NATURAL" =>
            {
                Clause::Join
            }
            "NATURAL" if matches!(next, "JOIN" | "LEFT" | "RIGHT" | "FULL" | "INNER") => {
                Clause::Join
            }
            "ON" if matches!(next, "CONFLICT" | "DUPLICATE") => Clause::Other,
            // After the CTEs of a WITH.
            "INSERT" | "UPDATE" | "DELETE" if prev_text == ")" => Clause::Other,
            "WITH"
                if k == 0 || prev_text == "(" || matches!(prev, "AS" | "EXPLAIN" | "ANALYZE") =>
            {
                Clause::With
            }
            _ => return None,
        })
    }

    /// Whether the list that starts at the `first`th significant token has
    /// more than one item.
    fn has_comma(&self, first: usize) -> bool {
        let mut depth = 0usize;
        for k in first..self.sig.len() {
            match self.toks[self.sig[k]].class() {
                Class::Open => depth += 1,
                Class::Close if depth == 0 => return false,
                Class::Close => depth -= 1,
                Class::Comma if depth == 0 => return true,
                Class::Semicolon => return false,
                _ if depth == 0 && self.clause_start(k).is_some() => return false,
                _ => {}
            }
        }
        false
    }

    /// The index after the parenthesis that closes the one opened by the
    /// `open`th significant token; `open` itself when it opens none.
    fn after_group(&self, open: usize) -> usize {
        let mut depth = 0usize;
        for k in open..self.sig.len() {
            match self.toks[self.sig[k]].class() {
                Class::Open => depth += 1,
                Class::Close if depth <= 1 => return k + usize::from(depth == 1),
                Class::Close => depth -= 1,
                _ if depth == 0 => return open,
                _ => {}
            }
        }
        self.sig.len()
    }

    /// How many WHEN the CASE at the `k`th significant token has; 0 when it
    /// has no END.
    fn case_whens(&self, k: usize) -> usize {
        let mut nested = 0usize;
        let mut whens = 0;
        for next in k + 1..self.sig.len() {
            match self.kw(next) {
                "CASE" => nested += 1,
                "END" if nested == 0 => return whens,
                "END" => nested -= 1,
                "WHEN" if nested == 0 => whens += 1,
                _ => {}
            }
        }
        0
    }

    /// Whether the operator character at `index` is glued to another one
    /// after it.
    fn run_goes_on(&self, index: usize) -> bool {
        self.toks
            .get(index + 1)
            .is_some_and(|next| next.glued && next.class() == Class::Operator)
    }

    /// Whether the `-` or `+` at `index` ends a run of operator characters
    /// it is not part of: `=-1` is `= -1`. PostgreSQL reads a trailing sign
    /// as part of the operator only when the run has one of `~!@#%^&|?` and
    /// the backtick (`@-@`).
    fn trailing_sign(&self, index: usize) -> bool {
        // Only signs up to the end of the run: `=-+1` is `= - +1`.
        let mut at = index;
        loop {
            if !matches!(self.toks[at].text, "-" | "+") {
                return false;
            }
            if !self.run_goes_on(at) {
                break;
            }
            at += 1;
        }
        let mut at = index;
        while at > 0 && self.toks[at].glued && self.toks[at - 1].class() == Class::Operator {
            at -= 1;
            if self.toks[at]
                .text
                .contains(['~', '!', '@', '#', '%', '^', '&', '|', '`', '?'])
            {
                return false;
            }
        }
        true
    }

    /// Whether a space goes between the `k`th significant token and the one
    /// before, were they on the same line. Whitespace is only ever removed
    /// where it cannot join two tokens into another one: after an opening
    /// parenthesis, before a closing one, a comma or a semicolon, around
    /// `::`. Everywhere else two tokens that were apart stay apart.
    fn space(&self, k: usize, run: &mut Run) -> bool {
        let (before, index) = (self.sig[k - 1], self.sig[k]);
        let (a, b) = (&self.toks[before], &self.toks[index]);
        let tight = b.glued && before + 1 == index;
        let (class_a, class_b) = (a.class(), b.class());
        if class_b == Class::Operator {
            if class_a == Class::Operator && tight {
                if !self.trailing_sign(index) {
                    return false;
                }
                *run = Run::Prefix;
                return true;
            }
            // What a space would break: `U&'...'` literals, the exponent
            // of `1e-3` (three tokens for the lexer) and array slices.
            let written = match b.text {
                "&" => {
                    tight
                        && a.text.eq_ignore_ascii_case("u")
                        && self.toks.get(index + 1).is_some_and(|next| {
                            next.glued
                                && matches!(next.kind, TokenKind::String | TokenKind::QuotedIdent)
                        })
                }
                "-" | "+" => tight && a.kind == TokenKind::Number && a.text.ends_with(['e', 'E']),
                ":" => !self.run_goes_on(index),
                _ => false,
            };
            *run = if written {
                Run::Written
            } else if a.ends_operand() {
                Run::Binary
            } else {
                Run::Prefix
            };
        }
        match (class_a, class_b) {
            // `: ::` must not become `:::`.
            (Class::Operator, Class::Cast) | (Class::Cast, Class::Operator) => !tight,
            (_, Class::Close | Class::Comma | Class::Semicolon)
            | (Class::Open | Class::Cast, _)
            | (_, Class::Cast) => false,
            (Class::Dot, _) | (_, Class::Dot) => !tight,
            (_, Class::Operator) => *run != Run::Written || !tight,
            (Class::Operator, _) => *run == Run::Binary || !tight,
            (Class::Comma, _) => true,
            // `f(x)` and `f (x)` are not the same call in MySQL, and
            // `a[1]` is not `a [1]`: as written. A keyword is set apart.
            (_, Class::Open) => {
                let as_written =
                    b.text == "[" || (a.kind == TokenKind::Word && a.keyword.is_none());
                !(as_written && tight)
            }
            // `N'...'`, `X'...'`, `_utf8'...'`.
            _ => !(tight && a.kind == TokenKind::Word && b.kind == TokenKind::String),
        }
    }

    fn layout(&self) -> String {
        let mut out = Out::default();
        let mut frames = vec![Frame {
            kind: FrameKind::Top,
            open: "",
            level: 0,
            close_level: 0,
            clause: Clause::None,
            list_level: None,
            between: false,
            case_base: 0,
        }];
        // Open CASE expressions: the indentation of their line, and whether
        // each WHEN gets its own line.
        let mut cases: Vec<(usize, bool)> = Vec::new();
        // Whether the statement is laid out clause by clause. Other
        // statements (DDL, routines, client commands) keep their line breaks.
        let mut clauses = matches!(
            self.kw(0),
            "SELECT" | "WITH" | "INSERT" | "UPDATE" | "DELETE" | "VALUES" | "EXPLAIN" | "REPLACE"
        );
        let create_table = self.kw(0) == "CREATE" && (1..4).any(|k| self.kw(k) == "TABLE");
        let mut columns_done = false;
        // A token that must start a line, and at which indentation.
        let mut pending: Option<(usize, usize)> = None;
        let mut run = Run::Prefix;
        let mut comments_from = 0;

        for k in 0..self.sig.len() {
            let index = self.sig[k];
            let tok = &self.toks[index];
            let class = tok.class();
            let keyword = tok.keyword.unwrap_or("");
            let mut sep = if k > 0 && self.space(k, &mut run) {
                Sep::Space
            } else {
                Sep::None
            };
            if let Some((at, level)) = pending
                && at == k
            {
                sep = Sep::Line(level);
                pending = None;
            }

            if class == Class::Close
                && frames.len() > 1
                && frames
                    .last()
                    .is_some_and(|frame| matches!((frame.open, tok.text), ("(", ")") | ("[", "]")))
                && let Some(frame) = frames.pop()
            {
                cases.truncate(frame.case_base);
                if frame.kind != FrameKind::Inline {
                    sep = Sep::Line(frame.close_level);
                }
            }
            let top = frames.len() - 1;
            // A query inside another statement: CREATE VIEW ... AS SELECT.
            if top == 0
                && !clauses
                && matches!(keyword, "SELECT" | "WITH")
                && matches!(self.kw(k.wrapping_sub(1)), "AS" | "FOR")
            {
                clauses = true;
            }
            let structural = match frames[top].kind {
                FrameKind::Top => clauses,
                FrameKind::Query => true,
                FrameKind::List | FrameKind::Inline => false,
            };
            let frame = &mut frames[top];
            if keyword == "AND" && frame.between {
                frame.between = false;
            } else if structural {
                if let Some(clause) = self.clause_start(k) {
                    sep = Sep::Line(frame.level);
                    frame.clause = clause;
                    frame.between = false;
                    frame.list_level = None;
                    if clause == Clause::With {
                        frame.list_level = Some(frame.level);
                    } else if matches!(clause, Clause::Select | Clause::Set | Clause::Values) {
                        let mut first = k + 1;
                        while clause == Clause::Select
                            && matches!(self.kw(first), "DISTINCT" | "ALL")
                        {
                            first += 1;
                        }
                        // `SELECT DISTINCT ON (...)`.
                        if clause == Clause::Select && self.kw(first) == "ON" {
                            first = self.after_group(first + 1);
                        }
                        if self.has_comma(first) {
                            frame.list_level = Some(frame.level + 1);
                            pending = Some((first, frame.level + 1));
                        }
                    }
                } else if matches!(keyword, "AND" | "OR")
                    && matches!(frame.clause, Clause::Where | Clause::Having | Clause::Join)
                    && cases.len() == frame.case_base
                {
                    sep = Sep::Line(frame.level + 1);
                } else if class == Class::Comma
                    && let Some(level) = frame.list_level
                {
                    pending = Some((k + 1, level));
                }
            }
            if class == Class::Comma && frame.kind == FrameKind::List {
                pending = Some((k + 1, frame.level));
            }
            let mut opens_case = None;
            match keyword {
                "BETWEEN" => frame.between = true,
                "CASE" => opens_case = Some(structural && self.case_whens(k) > 1),
                "WHEN" | "ELSE" | "END" if cases.len() > frame.case_base => {
                    if let Some(&(level, multiline)) = cases.last() {
                        if keyword == "END" {
                            cases.pop();
                        }
                        if multiline {
                            sep = Sep::Line(level + usize::from(keyword != "END"));
                        }
                    }
                }
                _ => {}
            }
            if top == 0
                && !clauses
                && k > 0
                && tok.newline
                && !matches!(class, Class::Close | Class::Comma | Class::Semicolon)
            {
                sep = Sep::Line(usize::from(tok.indented));
            }

            // Comments written before the token stay on their own line or
            // at the end of the line, as they were.
            let level = frame.level;
            let comments = &self.toks[comments_from..index];
            let after_open = k > 0 && self.toks[self.sig[k - 1]].class() == Class::Open;
            for comment in comments {
                if comment.newline {
                    out.line(match sep {
                        Sep::Line(level) => level,
                        _ => level + 1,
                    });
                } else if !after_open {
                    out.space();
                }
                out.put(comment.text);
            }
            if let Some(last) = comments.last()
                && !matches!(sep, Sep::Line(_))
            {
                sep = if last.kind == TokenKind::LineComment || tok.newline {
                    Sep::Line(level + 1)
                } else if matches!(class, Class::Close | Class::Comma | Class::Semicolon) {
                    Sep::None
                } else {
                    Sep::Space
                };
            }

            match sep {
                Sep::Line(level) => out.line(level),
                Sep::Space => out.space(),
                Sep::None => {}
            }
            out.put(tok.keyword.unwrap_or(tok.text));
            comments_from = index + 1;

            if let Some(multiline) = opens_case {
                cases.push((out.level, multiline));
            }
            if class == Class::Open {
                let kind = if tok.text == "(" && matches!(self.kw(k + 1), "SELECT" | "WITH") {
                    FrameKind::Query
                } else if tok.text == "(" && create_table && top == 0 && !columns_done {
                    columns_done = true;
                    FrameKind::List
                } else {
                    FrameKind::Inline
                };
                let level = if kind == FrameKind::Inline {
                    level
                } else {
                    out.level + 1
                };
                if kind != FrameKind::Inline {
                    pending = Some((k + 1, level));
                }
                frames.push(Frame {
                    kind,
                    open: tok.text,
                    level,
                    close_level: out.level,
                    clause: Clause::None,
                    list_level: None,
                    between: false,
                    case_base: cases.len(),
                });
            }
        }
        for comment in &self.toks[comments_from..] {
            if comment.newline {
                out.line(0);
            } else {
                out.space();
            }
            out.put(comment.text);
        }
        out.text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use LexRules as R;

    /// Statements and the exact text they must come back as (without the
    /// final newline).
    const CASES: &[(LexRules, &str, &str)] = &[
        (
            R::POSTGRES,
            "select id, name from users where active = true and age > 18 order by name limit 10;",
            "SELECT\n    id,\n    name\nFROM users\nWHERE active = TRUE\n    AND age > 18\nORDER BY name\nLIMIT 10;",
        ),
        (
            R::POSTGRES,
            "select o.id, c.full_name from shop.orders o left join shop.customers c on c.id = o.customer_id and c.is_active inner join shop.order_items i on i.order_id = o.id where o.total >= 100",
            "SELECT\n    o.id,\n    c.full_name\nFROM shop.orders o\nLEFT JOIN shop.customers c ON c.id = o.customer_id\n    AND c.is_active\nINNER JOIN shop.order_items i ON i.order_id = o.id\nWHERE o.total >= 100",
        ),
        (
            R::POSTGRES,
            "select * from (select id, total from shop.orders where status in ('paid','shipped')) as t where t.total > 10",
            "SELECT *\nFROM (\n    SELECT\n        id,\n        total\n    FROM shop.orders\n    WHERE status IN ('paid', 'shipped')\n) AS t\nWHERE t.total > 10",
        ),
        (
            R::POSTGRES,
            "with recent as (select * from shop.orders where placed_at > now() - interval '7 days'), big as (select * from recent where total > 100) select count(*) from big;",
            "WITH recent AS (\n    SELECT *\n    FROM shop.orders\n    WHERE placed_at > now() - interval '7 days'\n),\nbig AS (\n    SELECT *\n    FROM recent\n    WHERE total > 100\n)\nSELECT count(*)\nFROM big;",
        ),
        (
            R::POSTGRES,
            "insert into shop.categories (id, name) values (1, 'a'), (2, 'b') on conflict (id) do update set name = excluded.name returning id;",
            "INSERT INTO shop.categories (id, name)\nVALUES\n    (1, 'a'),\n    (2, 'b')\nON CONFLICT (id) DO UPDATE\nSET name = excluded.name\nRETURNING id;",
        ),
        (
            R::POSTGRES,
            "update shop.orders set status = 'paid', total = total * 1.2 where id = $1 returning *",
            "UPDATE shop.orders\nSET\n    status = 'paid',\n    total = total * 1.2\nWHERE id = $1\nRETURNING *",
        ),
        (
            R::POSTGRES,
            "delete from shop.reviews where rating < 2 and created_at < now() - interval '1 year';",
            "DELETE FROM shop.reviews\nWHERE rating < 2\n    AND created_at < now() - interval '1 year';",
        ),
        (
            R::POSTGRES,
            "select id, case when total > 100 then 'big' when total > 10 then 'medium' else 'small' end as size, case when is_active then 1 else 0 end from shop.orders",
            "SELECT\n    id,\n    CASE\n        WHEN total > 100 THEN 'big'\n        WHEN total > 10 THEN 'medium'\n        ELSE 'small'\n    END AS size,\n    CASE WHEN is_active THEN 1 ELSE 0 END\nFROM shop.orders",
        ),
        (
            R::POSTGRES,
            "select id, row_number() over (partition by customer_id order by placed_at desc) as rn, sum(total) filter (where status = 'paid') over w from shop.orders window w as (order by id)",
            "SELECT\n    id,\n    row_number() OVER (PARTITION BY customer_id ORDER BY placed_at DESC) AS rn,\n    sum(total) filter (WHERE status = 'paid') OVER w\nFROM shop.orders\nWINDOW w AS (ORDER BY id)",
        ),
        (
            R::POSTGRES,
            "select tags[1], preferences->>'theme', price::numeric(10,2), '{}'::jsonb @> attributes from shop.products where tags && array['a','b'] and attributes #>> '{a,b}' is not null",
            "SELECT\n    tags[1],\n    preferences ->> 'theme',\n    price::numeric(10, 2),\n    '{}'::jsonb @> attributes\nFROM shop.products\nWHERE tags && array['a', 'b']\n    AND attributes #>> '{a,b}' IS NOT NULL",
        ),
        (
            R::POSTGRES,
            "create table if not exists shop.notes (id bigint primary key, body text not null default '', created_at timestamptz not null default now(), constraint notes_body_check check (length(body) < 1000));",
            "CREATE TABLE IF NOT EXISTS shop.notes (\n    id bigint PRIMARY KEY,\n    body text NOT NULL DEFAULT '',\n    created_at timestamptz NOT NULL DEFAULT now(),\n    CONSTRAINT notes_body_check CHECK (length(body) < 1000)\n);",
        ),
        // A statement with no layout of its own keeps its line breaks, and
        // the dollar-quoted body is not touched.
        (
            R::POSTGRES,
            "create or replace function shop.touch() returns trigger\nlanguage plpgsql as $$\nbegin\n  new.updated_at := now();\n  return new;\nend;\n$$;",
            "CREATE OR REPLACE FUNCTION shop.touch() returns trigger\nlanguage plpgsql AS $$\nbegin\n  new.updated_at := now();\n  return new;\nend;\n$$;",
        ),
        // Comments stay where they were: at the end of a line or on their own.
        (
            R::POSTGRES,
            "-- header comment\n\nselect a, -- first\n  b /* inline */, c\n-- before from\nfrom t -- trailing\nwhere x = 1; -- done",
            "-- header comment\n\nSELECT\n    a, -- first\n    b /* inline */,\n    c\n-- before from\nFROM t -- trailing\nWHERE x = 1; -- done",
        ),
        (
            R::POSTGRES,
            "select 1;select 2;;\n\n\nselect 3",
            "SELECT 1;\n\nSELECT 2;\n\n;\n\nSELECT 3",
        ),
        (
            R::MYSQL,
            "select `id`,`name` from `users` where id=? limit ?,?",
            "SELECT\n    `id`,\n    `name`\nFROM `users`\nWHERE id = ?\nLIMIT ?, ?",
        ),
        // A DELIMITER section is a script for the mysql client: left as it is.
        (
            R::MYSQL,
            "delimiter $$\ncreate procedure p()\nbegin\n  select 1;\nend$$\ndelimiter ;\nselect   2;\n",
            "delimiter $$\ncreate procedure p()\nbegin\n  select 1;\nend$$\ndelimiter ;\n\nSELECT 2;",
        ),
        (
            R::POSTGRES,
            "select a from t union all select b from u order by 1",
            "SELECT a\nFROM t\nUNION ALL\nSELECT b\nFROM u\nORDER BY 1",
        ),
        (
            R::POSTGRES,
            "select * from shop.customers c where exists (select 1 from shop.orders o where o.customer_id = c.id and o.total > 100) and c.is_active",
            "SELECT *\nFROM shop.customers c\nWHERE EXISTS (\n    SELECT 1\n    FROM shop.orders o\n    WHERE o.customer_id = c.id\n        AND o.total > 100\n)\n    AND c.is_active",
        ),
        (
            R::POSTGRES,
            "select * from t where a between 1 and 10 and b = 2 or c is null",
            "SELECT *\nFROM t\nWHERE a BETWEEN 1 AND 10\n    AND b = 2\n    OR c IS NULL",
        ),
        (
            R::POSTGRES,
            "select -1, count(*), t.*, a*-b, 1 - -2 from t",
            "SELECT\n    -1,\n    count(*),\n    t.*,\n    a * -b,\n    1 - -2\nFROM t",
        ),
        (
            R::POSTGRES,
            "select 'select from where' as s, \"From\" from \"Select\" where note = 'it''s -- not a comment'",
            "SELECT\n    'select from where' AS s,\n    \"From\"\nFROM \"Select\"\nWHERE note = 'it''s -- not a comment'",
        ),
        (
            R::POSTGRES,
            "select * from t where a = :name and b = $1 and c in (:x, :y)",
            "SELECT *\nFROM t\nWHERE a = :name\n    AND b = $1\n    AND c IN (:x, :y)",
        ),
        (
            R::POSTGRES,
            "alter table shop.orders add column note text, drop column currency;",
            "ALTER TABLE shop.orders ADD COLUMN note text, DROP COLUMN currency;",
        ),
        (
            R::POSTGRES,
            "select status, count(*) as n from shop.orders group by status having count(*) > 5 and sum(total) > 100 order by n desc",
            "SELECT\n    status,\n    count(*) AS n\nFROM shop.orders\nGROUP BY status\nHAVING count(*) > 5\n    AND sum(total) > 100\nORDER BY n DESC",
        ),
        (
            R::MYSQL,
            "insert into t (a,b) values (1,2) on duplicate key update b = values(b), a=a+1",
            "INSERT INTO t (a, b)\nVALUES (1, 2)\nON DUPLICATE KEY UPDATE b = VALUES (b), a = a + 1",
        ),
        (
            R::POSTGRES,
            "explain analyze select * from t",
            "EXPLAIN ANALYZE\nSELECT *\nFROM t",
        ),
        (
            R::POSTGRES,
            "create view shop.v as select id, total from shop.orders where total > 0;",
            "CREATE VIEW shop.v AS\nSELECT\n    id,\n    total\nFROM shop.orders\nWHERE total > 0;",
        ),
        (
            R::POSTGRES,
            "insert into shop.audit_log (actor, action) select 'me', action from shop.audit_log",
            "INSERT INTO shop.audit_log (actor, action)\nSELECT\n    'me',\n    action\nFROM shop.audit_log",
        ),
        (
            R::POSTGRES,
            "select id, (select count(*) from shop.order_items i where i.order_id = o.id) as items from shop.orders o",
            "SELECT\n    id,\n    (\n        SELECT count(*)\n        FROM shop.order_items i\n        WHERE i.order_id = o.id\n    ) AS items\nFROM shop.orders o",
        ),
        (
            R::POSTGRES,
            "grant select, insert on shop.orders to kerno_app;",
            "GRANT SELECT, INSERT ON shop.orders TO kerno_app;",
        ),
        // MySQL table names are case-sensitive: one that MySQL lets go
        // unquoted keeps its case.
        (
            R::MYSQL,
            "select * from view v join end e on e.id = v.id",
            "SELECT *\nFROM view v\nJOIN end e ON e.id = v.id",
        ),
        // A qualified name is never a keyword.
        (
            R::POSTGRES,
            "select t.order, t.\"group\" from shop.select t",
            "SELECT\n    t.order,\n    t.\"group\"\nFROM shop.select t",
        ),
        // Function names are left alone, even those that are keywords elsewhere.
        (
            R::MYSQL,
            "select left(name, 2), if(a, 1, 2), replace(x, 'a', 'b') from t left join u on u.id = t.id",
            "SELECT\n    left(name, 2),\n    if(a, 1, 2),\n    replace(x, 'a', 'b')\nFROM t\nLEFT JOIN u ON u.id = t.id",
        ),
        (
            R::POSTGRES,
            "-- just a note\n/* and another */",
            "-- just a note\n/* and another */",
        ),
        (
            R::POSTGRES,
            "select E'a\\'b', n'x' , x'ff'",
            "SELECT\n    E'a\\'b',\n    n'x',\n    x'ff'",
        ),
        (
            R::POSTGRES,
            "select a from t where x=-1 and y>=-2 and z = 3*-4",
            "SELECT a\nFROM t\nWHERE x = -1\n    AND y >= -2\n    AND z = 3 * -4",
        ),
        // What a space would break is left as written: exponents, array
        // slices; nested arrays get their spaces.
        (
            R::POSTGRES,
            "select 1.5e-3, arr[1:2], array[[1,2],[3,4]] from t order by a nulls last",
            "SELECT\n    1.5e-3,\n    arr[1:2],\n    array[[1, 2], [3, 4]]\nFROM t\nORDER BY a NULLS LAST",
        ),
        (
            R::POSTGRES,
            "insert into shop.orders default values returning id;",
            "INSERT INTO shop.orders DEFAULT VALUES\nRETURNING id;",
        ),
        (
            R::POSTGRES,
            "select distinct on (customer_id) customer_id, placed_at from shop.orders",
            "SELECT DISTINCT ON (customer_id)\n    customer_id,\n    placed_at\nFROM shop.orders",
        ),
    ];

    /// More text the properties must hold for, under both sets of rules.
    const CORPUS: &[&str] = &[
        "",
        "   \n\t ",
        "select 1",
        "SELECT\n\t*\r\nFROM\tt\r\n-- crlf comment\r\nWHERE x = 1;\r\n",
        "select /*+ hint */ a /* one */ , /* two */ b from /* three */ t /* four */ where /* five */ a = 1 /* six */ ; /* seven */",
        "-- leading\nselect a -- trailing a\n, b -- trailing b\nfrom t -- trailing t\n-- own line\nwhere a = 1 -- end\n;",
        "select (a + b) * (c - d) / e % f from t where not (a and b) or (c and (d or e))",
        "select a from t where x in (select y from u where z in (select w from v where q = 1 and r = 2) and p = 3) and s = 4",
        "with recursive tree (id, parent_id, depth) as (select id, parent_id, 0 from shop.categories where parent_id is null union all select c.id, c.parent_id, t.depth + 1 from shop.categories c join tree t on t.id = c.parent_id) select * from tree order by depth, id",
        "select coalesce(sum(i.quantity * i.unit_price), 0)::numeric(12, 2) as computed_total from shop.order_items i group by i.order_id",
        "select array[1,2,3], array[[1,2],[3,4]], arr[1:2], (arr)[1], '{1,2}'::int[] from t",
        "select data->'a'->>'b', data #> '{a,b}', data @> '{\"a\":1}', data ? 'k', data ?| array['a'] from docs",
        "create or replace function shop.f(a int, b text default 'x') returns int language plpgsql as $fn$\nbegin\n  if a > 0 then return a; end if;\n  return 0; -- done\nend;\n$fn$;",
        "do $$ begin raise notice 'hi; there'; end $$;",
        "create index concurrently if not exists orders_status_idx on shop.orders using btree (status, placed_at desc) where status <> 'cancelled';",
        "create unique index products_sku_key on shop.products (lower(sku));",
        "alter table shop.orders\n    add constraint orders_customer_fk foreign key (customer_id) references shop.customers (id) on delete cascade,\n    alter column total set default 0;",
        "drop table if exists shop.tmp cascade; truncate table shop.audit_log; vacuum analyze shop.orders;",
        "create trigger orders_touch before update on shop.orders for each row execute function shop.set_updated_at();",
        "grant select, insert, update on all tables in schema shop to kerno_app;",
        "comment on column shop.orders.total is 'Total; with a semicolon';",
        "copy shop.orders (id, total) from stdin with (format csv, header true);",
        "select * from t where a = 'it''s' and b = E'tab\\tquote\\'' and c = $q$dollar ' quote$q$ and d = U&'\\0041'",
        "select `a`.`b`, `weird``name` from `db`.`t` where `x` = \"str\" and y = 'it\\'s' # mysql comment\nlimit 5",
        "select * from t where a <=> b and c != d and e <> f and g := 1 and !h and i div 2 = 1 and j regexp '^a'",
        "set @total := (select sum(total) from orders); select @total, @@session.sql_mode;",
        "delimiter //\ncreate trigger t before insert on orders for each row\nbegin\n  set new.created_at = now();\nend//\ndelimiter ;\n\nselect 1;\ndelimiter $$\nselect 2$$\ndelimiter ;",
        "DELIMITER ;\nselect 1;",
        "delimiter $$\nselect 1$$ -- never switched back   ",
        "insert into t values (1, 'a'), (2, 'b'), (3, 'c');",
        "insert into shop.orders default values returning id;",
        "replace into t (a) values (1);",
        "select distinct on (customer_id) customer_id, placed_at from shop.orders order by customer_id, placed_at desc nulls last limit 10 offset 20;",
        "select a from t for update skip locked;",
        "select a is distinct from b, c is not distinct from d from t",
        "select extract(year from placed_at), substring(name from 1 for 3), trim(both ' ' from name), cast(total as text), position('a' in name) from t",
        "select count(distinct customer_id), string_agg(name, ', ' order by name), percentile_cont(0.5) within group (order by total) from t",
        "select * from t1 natural join t2 cross join t3 full outer join t4 on t4.id = t1.id left outer join lateral (select * from t5 where t5.id = t1.id limit 1) l on true",
        "select * from a, b, c where a.id = b.id and b.id = c.id",
        "values (1, 'a'), (2, 'b')",
        "table shop.orders",
        "begin; update t set a = 1 where b = 2; commit;",
        "select 'unterminated string from where",
        "select \"unterminated ident from where",
        "select 1 /* unterminated comment from where",
        "select $tag$ unterminated dollar from where",
        ")))) from ,, where (( select and or ;;; ]]] [[[",
        "select case when a then case when b then 1 else 2 end when c then 3 end, case x when 1 then 'a' end from t",
        "select a, case when b then 1 end where",
        "select * from t where a between 1 and 2 and b between (select 1) and (select 2) and c",
        "select 1 where a=-1 and b=+2 and c<-3 and d>=-4 and e*-1=f and g--h\n=1",
        "select 1::int::text, '1'::int+2, a::numeric(10,2)*b, (a)::text||'x' from t",
        "select * from t where (a, b) in ((1, 2), (3, 4)) and row(a, b) = row(1, 2)",
        "select sélection, \"ünï\", 'çà' from tablé where ☃ = 1",
        "select 1e10, 1.5e-3, .5, 0x1F, 1_000, 1. from t",
        "select a.b.c.d, a . b, a. b, a .b from t",
        "\\d shop.orders",
        "select [a], {b}, a[1][2], f(x)[1], (f(x)).y from t",
        "create table if not exists t (\n  id int primary key, -- the id\n  /* name */ name varchar(10) not null default 'x',\n  price numeric(10,2) check (price > 0),\n  constraint fk foreign key (id) references u (id)\n) engine=innodb default charset=utf8mb4;",
        "create table t2 as select * from t where 1 = 0;",
        "create temporary table tmp (a int, b int);",
        "create materialized view shop.mv as select 1 as a with no data;",
        "select * from t where a like '%x%' escape '\\' and b ilike 'y' and c similar to 'z' and d not in (1,2) and e is not null",
        "select now() at time zone 'utc', interval '1 day' * 2, date '2024-01-01' + 1, timestamp with time zone '2024-01-01'",
        "update t set (a, b) = (select 1, 2), c = default where d in (select e from f) returning *",
        "delete from t using u where t.id = u.id and u.x > 1",
        "with a as (select 1), b as materialized (select 2), c (x, y) as not materialized (select 3, 4) insert into t select * from a, b, c",
        "select\n\n\n  a\n\n,b\n\n\nfrom\n\nt\n\n\n\n;\n\n\n\nselect 2\n\n",
        "/* header */\n\n-- second\nselect 1; -- same line\n-- next\n\n/* block */ select 2 /* tail */\n-- end\n",
        "select 1 -- no newline at end",
        ";",
        ";;;",
        "-- only",
        "select * from t where x = ? and y in (?, ?) limit ? offset ?",
        "call shop.cancel_stale_orders(interval '30 days'); show tables; use shop; describe orders; explain select 1;",
        "select if(a > 1, 'x', 'y'), ifnull(b, 0), values(c) from t on duplicate key update",
        "lock tables t write; unlock tables;",
        "select a from t where b = 1 -- why\n  and c = 2 /* because */ or d = 3\n  -- alone\n  and e = 4",
        "select (\n  -- inside\n  a\n), ( /* x */ b /* y */ ), (c -- z\n) from t",
        "select a /* multi\n line */ from t /* another\none */\nwhere b",
        "create function f() returns int as 'select 1' language sql;\ncreate procedure p() begin select 1; select 2; end",
        "select * from t where a = any (select b from u) and c > all (array[1, 2]) and d = some (select 1)",
        "select key, index, schema, view, value from settings where key = 'a'",
        "insert into t (a) values (1) on conflict do nothing",
        "select * from t as of system time '-1s' where x",
        "select a AS \"A\", b As B, c aS c from T",
        "SELECT A, B FROM T WHERE X AND Y",
    ];

    /// Every text of both lists.
    fn corpus() -> impl Iterator<Item = &'static str> {
        CASES
            .iter()
            .map(|(_, input, _)| *input)
            .chain(CORPUS.iter().copied())
    }

    fn significant(sql: &str, rules: LexRules) -> Vec<(TokenKind, &str)> {
        lex(sql, rules)
            .into_iter()
            .filter(|token| token.kind != TokenKind::Whitespace)
            .map(|token| (token.kind, &sql[token.range]))
            .collect()
    }

    fn is_keyword(word: &str) -> bool {
        KEYWORDS.iter().any(|(keyword, _)| *keyword == word)
            || CONTEXTUAL.iter().any(|(words, _)| words.contains(&word))
    }

    #[test]
    fn the_keyword_table_is_sorted() {
        assert!(KEYWORDS.windows(2).all(|pair| pair[0].0 < pair[1].0));
    }

    #[test]
    fn representative_statements_come_back_exactly_so() {
        for (rules, input, expected) in CASES {
            let formatted = format_sql(input, *rules);
            assert_eq!(
                formatted,
                format!("{expected}\n"),
                "\n--- input ---\n{input}\n--- got ---\n{formatted}"
            );
        }
    }

    #[test]
    fn empty_text_stays_empty() {
        for rules in [R::POSTGRES, R::MYSQL] {
            assert_eq!(format_sql("", rules), "");
            assert_eq!(format_sql("  \n\t\n", rules), "");
        }
    }

    #[test]
    fn the_corpus_is_large_enough() {
        assert!(corpus().count() >= 60 + CASES.len());
        assert!(CASES.len() >= 25);
    }

    /// The tokens that come out are the tokens that went in: same kinds,
    /// same text, except for keywords written in upper case. That covers
    /// comments, strings, quoted identifiers and dollar-quoted bodies byte
    /// for byte.
    #[test]
    fn formatting_preserves_every_token() {
        for rules in [R::POSTGRES, R::MYSQL] {
            for input in corpus() {
                let formatted = format_sql(input, rules);
                let before = significant(input, rules);
                let after = significant(&formatted, rules);
                assert_eq!(
                    before.len(),
                    after.len(),
                    "\n--- input ---\n{input}\n--- got ---\n{formatted}"
                );
                for (was, is) in before.iter().zip(&after) {
                    assert_eq!(
                        was.0, is.0,
                        "\n--- input ---\n{input}\n--- got ---\n{formatted}"
                    );
                    if was.1 != is.1 {
                        assert!(
                            was.0 == TokenKind::Word
                                && is.1 == was.1.to_ascii_uppercase()
                                && is_keyword(is.1),
                            "{:?} became {:?}\n--- input ---\n{input}\n--- got ---\n{formatted}",
                            was.1,
                            is.1
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn formatting_is_idempotent() {
        for rules in [R::POSTGRES, R::MYSQL] {
            for input in corpus() {
                let once = format_sql(input, rules);
                let twice = format_sql(&once, rules);
                assert_eq!(
                    twice, once,
                    "\n--- input ---\n{input}\n--- once ---\n{once}\n--- twice ---\n{twice}"
                );
            }
        }
    }

    /// The safety net (a statement kept as written) must not be what makes
    /// the properties hold.
    #[test]
    fn no_statement_of_the_corpus_needs_the_safety_net() {
        for rules in [R::POSTGRES, R::MYSQL] {
            for input in corpus() {
                let (formatted, kept) = format_counting(input, rules);
                assert_eq!(
                    kept, 0,
                    "\n--- input ---\n{input}\n--- got ---\n{formatted}"
                );
            }
        }
    }

    /// Text nobody would write: random sequences of SQL pieces. The
    /// generator is a fixed linear congruential one, so a failure repeats.
    #[test]
    fn random_token_soup_keeps_the_properties() {
        const PIECES: &[&str] = &[
            "select",
            "from",
            "where",
            "and",
            "or",
            "join",
            "left",
            "on",
            "group",
            "by",
            "order",
            "case",
            "when",
            "then",
            "else",
            "end",
            "with",
            "as",
            "insert",
            "into",
            "values",
            "update",
            "set",
            "delete",
            "union",
            "all",
            "create",
            "table",
            "between",
            "in",
            "is",
            "not",
            "null",
            "distinct",
            "limit",
            "returning",
            "default",
            "key",
            "delimiter",
            "begin",
            "a",
            "b",
            "t",
            "shop",
            "x1",
            "U",
            "E",
            "n",
            "1",
            "2.5",
            "1e",
            "0x1F",
            "'s'",
            "'it''s'",
            "\"q\"",
            "`bt`",
            "$1",
            "$$d;$$",
            ":p",
            "?",
            "(",
            ")",
            "(",
            ")",
            "[",
            "]",
            ",",
            ",",
            ".",
            ";",
            "::",
            ":",
            "=",
            "<",
            ">",
            "-",
            "+",
            "*",
            "/",
            "|",
            "&",
            "@",
            "#",
            "!",
            "~",
            "%",
            "^",
            "{",
            "}",
            "$",
            "\\",
            "-- c\n",
            "# h\n",
            "/* b */",
            "/* m\n l */",
            " ",
            " ",
            " ",
            " ",
            "\n",
            "\n",
            "\n\n",
            "\t",
            "",
        ];
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = |bound: usize| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as usize % bound
        };
        let mut kept_total = 0;
        for _ in 0..3000 {
            let mut input = String::new();
            for _ in 0..next(40) + 1 {
                input.push_str(PIECES[next(PIECES.len())]);
                // Most pieces are set apart; some are glued to the next.
                if next(3) > 0 {
                    input.push(' ');
                }
            }
            for rules in [R::POSTGRES, R::MYSQL] {
                let (once, kept) = format_counting(&input, rules);
                kept_total += kept;
                let before = significant(&input, rules);
                let after = significant(&once, rules);
                let same = before.len() == after.len()
                    && before.iter().zip(&after).all(|(was, is)| {
                        was.0 == is.0
                            && (was.1 == is.1
                                || (was.0 == TokenKind::Word
                                    && is.1 == was.1.to_ascii_uppercase()
                                    && is_keyword(is.1)))
                    });
                assert!(same, "\n--- input ---\n{input:?}\n--- got ---\n{once:?}");
                let twice = format_sql(&once, rules);
                assert_eq!(
                    twice, once,
                    "\n--- input ---\n{input:?}\n--- once ---\n{once}\n--- twice ---\n{twice}"
                );
            }
        }
        // The safety net (a statement kept as written) was never needed.
        assert_eq!(kept_total, 0);
    }

    #[test]
    fn output_is_tidy() {
        for rules in [R::POSTGRES, R::MYSQL] {
            for input in corpus() {
                let formatted = format_sql(input, rules);
                if formatted.is_empty() {
                    assert!(significant(input, rules).is_empty(), "{input}");
                    continue;
                }
                // Nothing can be written after a string or a comment left open.
                let open_ended = matches!(
                    significant(input, rules).last(),
                    Some((
                        TokenKind::String | TokenKind::QuotedIdent | TokenKind::BlockComment,
                        _
                    ))
                ) && !formatted.ends_with('\n');
                assert!(formatted.ends_with('\n') || open_ended, "{input}");
                assert!(!formatted.ends_with("\n\n"), "{input}");
                assert!(!formatted.starts_with(char::is_whitespace), "{input}");
            }
        }
    }

    #[test]
    fn statements_are_separated_by_one_blank_line() {
        assert_eq!(
            format_sql("select 1;\nselect 2;\n\n\n\nselect 3;", R::POSTGRES),
            "SELECT 1;\n\nSELECT 2;\n\nSELECT 3;\n"
        );
        // A comment stays glued to the statement it was written above.
        assert_eq!(
            format_sql(
                "select 1;\n-- about two\nselect 2;\n-- loose\n\nselect 3;",
                R::POSTGRES
            ),
            "SELECT 1;\n\n-- about two\nSELECT 2;\n\n-- loose\n\nSELECT 3;\n"
        );
    }
}
