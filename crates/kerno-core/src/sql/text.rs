//! Text commands of the SQL editor that need to know what is SQL and what
//! is data: changing the case of a selection, block comments, and the
//! bracket that matches the one at the cursor.

use std::ops::Range;

use super::lex::{LexRules, Token, TokenKind, lex};

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

/// The bracket just before `offset` (else the one just after it) and the
/// bracket that matches it, for the editor to mark. Only brackets of SQL
/// count: those in strings, quoted names and comments are text. Depth
/// counts the same kind of bracket only, and a match never crosses a `;`.
/// A dollar-quoted body is code: inside one, its own brackets are matched.
/// `None` when no bracket touches `offset` or it has no match.
pub fn matching_bracket(
    sql: &str,
    offset: usize,
    rules: LexRules,
) -> Option<(Range<usize>, Range<usize>)> {
    let tokens = lex(sql, rules);
    // The body of a routine is code in a dollar-quoted string: inside one,
    // its brackets are matched as those of a script of their own.
    if let Some(body) = tokens.iter().find(|token| {
        token.kind == TokenKind::String
            && token.range.start < offset
            && offset < token.range.end
            && sql[token.range.clone()].starts_with('$')
    }) {
        let quoted = &sql[body.range.clone()];
        let tag = quoted[1..].find('$')? + 2;
        let inner = body.range.start + tag..body.range.end.checked_sub(tag)?;
        if !quoted.ends_with(&quoted[..tag]) || !(inner.start..=inner.end).contains(&offset) {
            return None;
        }
        let shift = |range: Range<usize>| range.start + inner.start..range.end + inner.start;
        return matching_bracket(&sql[inner.clone()], offset - inner.start, rules)
            .map(|(at, other)| (shift(at), shift(other)));
    }
    // A bracket is a one-byte punctuation token: `(` and `)` give
    // `Some(('(', ')'))` and `Some((')', '('))`.
    let bracket = |token: &Token| {
        if token.kind != TokenKind::Punct || token.range.len() != 1 {
            return None;
        }
        match sql.as_bytes()[token.range.start] {
            b'(' => Some((b'(', b')')),
            b'[' => Some((b'[', b']')),
            b'{' => Some((b'{', b'}')),
            b')' => Some((b')', b'(')),
            b']' => Some((b']', b'[')),
            b'}' => Some((b'}', b'{')),
            _ => None,
        }
    };
    let at = tokens
        .iter()
        .position(|token| token.range.end == offset && bracket(token).is_some())
        .or_else(|| {
            tokens
                .iter()
                .position(|token| token.range.start == offset && bracket(token).is_some())
        })?;
    let (same, other) = bracket(&tokens[at])?;
    let forward = matches!(same, b'(' | b'[' | b'{');
    let rest: Box<dyn Iterator<Item = &Token>> = if forward {
        Box::new(tokens[at + 1..].iter())
    } else {
        Box::new(tokens[..at].iter().rev())
    };
    let mut depth = 0usize;
    for token in rest {
        if token.kind == TokenKind::Semicolon {
            return None;
        }
        match bracket(token) {
            Some((byte, _)) if byte == same => depth += 1,
            Some((byte, _)) if byte == other => {
                if depth == 0 {
                    return Some((tokens[at].range.clone(), token.range.clone()));
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    None
}

/// The most occurrences [`occurrences`] returns.
pub const MAX_OCCURRENCES: usize = 1_000;

/// Every occurrence in `text` of what `selection` designates, in order and
/// the designating one included, at most [`MAX_OCCURRENCES`]:
///
/// - an empty selection (a cursor) designates the word it touches (a cursor
///   just after a word counts), matched as a whole word;
/// - a selection on one line designates its text, matched anywhere.
///
/// Matching ignores ASCII case, as SQL names and keywords do (`Orders` and
/// `orders` are one table). Strings and comments are searched too, as
/// editors do. Nothing is designated by a cursor outside a word, a
/// selection over several lines, or one of blanks only.
pub fn occurrences(text: &str, selection: Range<usize>) -> Vec<Range<usize>> {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let (needle, whole_word) = if selection.is_empty() {
        let at = selection.start;
        if !text.is_char_boundary(at) {
            return Vec::new();
        }
        let start = text[..at]
            .char_indices()
            .rev()
            .take_while(|&(_, c)| is_word(c))
            .last()
            .map_or(at, |(index, _)| index);
        let end = at
            + text[at..]
                .char_indices()
                .find(|&(_, c)| !is_word(c))
                .map_or(text.len() - at, |(index, _)| index);
        (&text[start..end], true)
    } else {
        match text.get(selection) {
            Some(selected) if !selected.contains('\n') && !selected.trim().is_empty() => {
                (selected, false)
            }
            _ => return Vec::new(),
        }
    };
    if needle.is_empty() {
        return Vec::new();
    }
    let (hay, pin) = (text.as_bytes(), needle.as_bytes());
    let mut found = Vec::new();
    let mut at = 0;
    // ponytail: a plain scan, O(text × word); a 1 MB script with a long
    // selection stays well under a frame.
    while at + pin.len() <= hay.len() && found.len() < MAX_OCCURRENCES {
        // A match starts on a character: `pin` begins with an ASCII byte or
        // a lead byte, which no continuation byte equals.
        if hay[at..at + pin.len()].eq_ignore_ascii_case(pin) {
            let end = at + pin.len();
            let apart = !whole_word
                || (!text[..at].chars().next_back().is_some_and(is_word)
                    && !text[end..].chars().next().is_some_and(is_word));
            if apart {
                found.push(at..end);
                at = end;
                continue;
            }
        }
        at += 1;
    }
    found
}

/// `sql` with every literal replaced by `?`: quoted strings (dollar-quoted
/// ones too) and numbers go, comments go (they are free text), names and
/// keywords stay. For what is shown or sent where a value must not be: a
/// table tab's filter holds values of its rows ("Filter by this value", a
/// foreign key followed).
pub fn without_literals(sql: &str, rules: LexRules) -> String {
    let mut out = String::with_capacity(sql.len());
    for token in lex(sql, rules) {
        match token.kind {
            TokenKind::String | TokenKind::Number => out.push('?'),
            TokenKind::LineComment | TokenKind::BlockComment => out.push(' '),
            _ => out.push_str(&sql[token.range]),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_go_and_names_stay() {
        let pg = |sql| without_literals(sql, LexRules::POSTGRES);
        assert_eq!(
            pg("SELECT * FROM shop.customers WHERE email = 'ada@example.com' AND id > 42"),
            "SELECT * FROM shop.customers WHERE email = ? AND id > ?"
        );
        // A quote in a string, a dollar-quoted one, a number with an
        // exponent, a comment that quotes a value: nothing of a value stays.
        assert_eq!(
            pg(
                "WHERE n = 'O''Brien' OR t = $x$se'cret$x$ OR v < 1.5e3 -- was 'old'\nORDER BY \"Full Name\""
            ),
            "WHERE n = ? OR t = ? OR v < ?  \nORDER BY \"Full Name\""
        );
        assert_eq!(pg("LIMIT 20 OFFSET 40"), "LIMIT ? OFFSET ?");
        // MySQL: a double-quoted string is a value, a backticked name is a name.
        assert_eq!(
            without_literals("WHERE `name` = \"ada\" AND x = 'a\\'b'", LexRules::MYSQL),
            "WHERE `name` = ? AND x = ?"
        );
    }

    /// The occurrences, with the cursor or selection marked by `|` (one:
    /// a cursor; two: a selection), as the text each covers at its offset.
    fn marks(marked: &str) -> Vec<String> {
        let first = marked.find('|').expect("a cursor");
        let text = marked.replace('|', "");
        let second = marked[first + 1..].find('|').map(|at| at + first);
        let selection = first..second.unwrap_or(first);
        occurrences(&text, selection)
            .into_iter()
            .map(|range| format!("{}@{}", &text[range.clone()], range.start))
            .collect()
    }

    #[test]
    fn the_word_at_the_cursor_is_found_as_a_whole_word_in_any_case() {
        assert_eq!(
            marks("select id, o.|id from orders o where ID > 1 and idx = 2"),
            ["id@7", "id@13", "ID@36"]
        );
        // A cursor just after the word, or just before it, is in it.
        assert_eq!(marks("a.b| + b"), ["b@2", "b@6"]);
        assert_eq!(marks("|ab + ab_c + ab"), ["ab@0", "ab@12"]);
        // Inside strings and comments too.
        assert_eq!(
            marks("total| -- the total\n'total'"),
            ["total@0", "total@13", "total@20"]
        );
        // Outside a word, nothing.
        assert_eq!(marks("a + | b"), Vec::<String>::new());
        assert_eq!(marks("|"), Vec::<String>::new());
    }

    #[test]
    fn a_selection_on_one_line_is_found_anywhere() {
        assert_eq!(marks("|id|x id idx ID"), ["id@0", "id@4", "id@7", "ID@11"]);
        assert_eq!(marks("a|.b| a.b"), [".b@1", ".b@5"]);
        // Several lines, or only blanks: nothing.
        assert_eq!(marks("|a\nb| a\nb"), Vec::<String>::new());
        assert_eq!(marks("a| |b a b"), Vec::<String>::new());
    }

    #[test]
    fn letters_beyond_ascii_are_matched_as_they_are() {
        assert_eq!(marks("|été| ÉTÉ été"), ["été@0", "été@12"]);
        assert_eq!(marks("prix_|été été_x été"), ["prix_été@0"]);
        // An offset inside a character designates nothing.
        assert_eq!(occurrences("é", 1..1), Vec::<Range<usize>>::new());
        assert_eq!(occurrences("ab", 9..9), Vec::<Range<usize>>::new());
    }

    #[test]
    fn matches_do_not_overlap_and_stop_at_the_cap() {
        assert_eq!(marks("|aa|aaa"), ["aa@0", "aa@2"]);
        let text = "x ".repeat(MAX_OCCURRENCES + 10);
        assert_eq!(occurrences(&text, 0..0).len(), MAX_OCCURRENCES);
    }

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

    /// The marked pair as the text it covers, with the cursor at the `|`.
    fn pair(marked: &str, rules: LexRules) -> Option<(String, String)> {
        let offset = marked.find('|').expect("a cursor");
        let text = marked.replacen('|', "", 1);
        matching_bracket(&text, offset, rules).map(|(at, other)| {
            (
                format!("{}@{}", &text[at.clone()], at.start),
                format!("{}@{}", &text[other.clone()], other.start),
            )
        })
    }

    fn pg(marked: &str) -> Option<(String, String)> {
        pair(marked, LexRules::POSTGRES)
    }

    fn found(at: &str, other: &str) -> Option<(String, String)> {
        Some((at.to_string(), other.to_string()))
    }

    #[test]
    fn the_bracket_beside_the_cursor_and_its_match_are_found_either_way() {
        // Just after the bracket, and just before it.
        assert_eq!(pg("count(|*)"), found("(@5", ")@7"));
        assert_eq!(pg("count|(*)"), found("(@5", ")@7"));
        assert_eq!(pg("count(*)|"), found(")@7", "(@5"));
        assert_eq!(pg("count(*|)"), found(")@7", "(@5"));
        // Between two brackets, the one before the cursor wins.
        assert_eq!(pg("f(a)|(b)"), found(")@3", "(@1"));
        // Square brackets and braces too.
        assert_eq!(pg("a[|1]"), found("[@1", "]@3"));
        assert_eq!(pg("|{x}"), found("{@0", "}@2"));
        // Elsewhere, nothing.
        assert_eq!(pg("count(*) |from t"), None);
        assert_eq!(pg("|"), None);
    }

    #[test]
    fn nested_brackets_match_at_their_own_depth() {
        assert_eq!(pg("|(a + (b * c))"), found("(@0", ")@12"));
        assert_eq!(pg("(a + |(b * c))"), found("(@5", ")@11"));
        assert_eq!(pg("(a + (b * c)|)"), found(")@11", "(@5"));
        // Another kind of bracket does not count.
        assert_eq!(pg("|(a[1], b[2])"), found("(@0", ")@11"));
    }

    #[test]
    fn brackets_in_a_dollar_quoted_body_match_inside_it() {
        let sql = "create function f() returns int as $body$ begin return (1 + (2)); end $body$";
        let open = sql.find("(1").unwrap();
        let close = sql.rfind("))").unwrap() + 1;
        assert_eq!(
            matching_bracket(sql, open + 1, LexRules::POSTGRES),
            Some((open..open + 1, close..close + 1))
        );
        // Not across the end of the body, and an unfinished body has none.
        assert_eq!(
            matching_bracket("select $$ ( $$ )", 11, LexRules::POSTGRES),
            None
        );
        assert_eq!(
            matching_bracket("select $$ ()", 11, LexRules::POSTGRES),
            None
        );
    }

    #[test]
    fn an_unbalanced_bracket_has_no_match() {
        assert_eq!(pg("|(a + (b)"), None);
        assert_eq!(pg("a)|"), None);
        // A match never crosses the end of a statement.
        assert_eq!(pg("select (1;|)"), None);
        assert_eq!(pg("select |(1; select 2)"), None);
    }

    #[test]
    fn brackets_in_strings_names_and_comments_do_not_count() {
        assert_eq!(pg("|(')' || x)"), found("(@0", ")@9"));
        assert_eq!(pg("|(\")\" + x)"), found("(@0", ")@8"));
        assert_eq!(pg("|(x -- )\n)"), found("(@0", ")@8"));
        assert_eq!(pg("|(x /* ) */)"), found("(@0", ")@10"));
        assert_eq!(pg("|($$)$$)"), found("(@0", ")@6"));
        // A bracket inside a string is not marked itself.
        assert_eq!(pg("'(|'"), None);
        assert_eq!(pg("-- (|)"), None);
        // MySQL: backticks name, double quotes are strings, `#` comments.
        let mysql = |marked| pair(marked, LexRules::MYSQL);
        assert_eq!(mysql("|(`)` + \")\")"), found("(@0", ")@10"));
        assert_eq!(mysql("|(x # )\n)"), found("(@0", ")@7"));
    }

    #[test]
    fn multi_byte_text_around_the_brackets_is_safe() {
        assert_eq!(pg("'été', (|'ü')"), found("(@9", ")@14"));
        assert_eq!(pg("(é)|"), found(")@3", "(@0"));
        // An offset inside a character is not a bracket's edge.
        assert_eq!(pg("é|"), None);
        assert_eq!(matching_bracket("é", 1, LexRules::POSTGRES), None);
        assert_eq!(matching_bracket("()", 9, LexRules::POSTGRES), None);
    }
}
