//! Splitting a script into statements, and finding the statement under the
//! cursor. Built on the tolerant lexer, so it works on text that is still
//! being typed.

use std::ops::Range;

use super::lex::{LexRules, Token, TokenKind, lex};

/// One statement of a script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatementSpan {
    /// Byte range of the statement text, without surrounding whitespace and
    /// without its terminating delimiter. This is what is sent to the server.
    pub range: Range<usize>,
    /// Byte offset just past the statement's delimiter and the rest of that
    /// line's whitespace: the cursor still "belongs" to the statement up to here.
    pub reach: usize,
}

impl StatementSpan {
    /// The statement text.
    pub fn text<'a>(&self, source: &'a str) -> &'a str {
        &source[self.range.clone()]
    }
}

/// Splits `source` into statements.
///
/// Statements end at `;` outside strings, quoted identifiers and comments.
/// With [`LexRules::delimiter_command`], `DELIMITER xx` lines change the
/// separator the way the `mysql` client does, and are not statements
/// themselves. Like that client, a custom delimiter is matched as plain
/// text anywhere outside strings, quoted identifiers and comments, even
/// glued to a word (`END$$`). Empty statements and comment-only stretches
/// are dropped.
///
/// While the separator is `;`, a routine body written without `DELIMITER`
/// stays in its statement, as the server reads it (C9): a PostgreSQL
/// `BEGIN ATOMIC … END` in `CREATE FUNCTION` or `CREATE PROCEDURE`, and a
/// MySQL or MariaDB `BEGIN … END` in `CREATE PROCEDURE`, `FUNCTION`,
/// `TRIGGER` or `EVENT` (or MariaDB's `BEGIN NOT ATOMIC … END`). See
/// [`Blocks`] for when a block opens and closes. A body never closed runs to
/// the end of the text.
///
/// One pass over the tokens: linear in the length of the script (20,000
/// statements split in about 15 ms in a debug build, against 16 s for the
/// earlier rescan of every token at each delimiter; a body of 20,000 lines
/// never closed, in about 45 ms).
pub fn split_statements(source: &str, rules: LexRules) -> Vec<StatementSpan> {
    let tokens = lex(source, rules);
    let bytes = source.as_bytes();
    let mut statements = Vec::new();
    let mut delimiter = String::from(";");
    // Where the statement being collected starts.
    let mut segment_start = 0;
    // Everything before this byte has been dealt with.
    let mut cursor = 0;
    // Whether the statement being collected has anything but whitespace
    // and comments so far.
    let mut has_content = false;
    let mut blocks = Blocks::default();

    for (index, token) in tokens.iter().enumerate() {
        if token.range.end <= cursor {
            continue;
        }
        let runs = runs_as_sql(source, token, rules);
        if token.kind.is_trivia() && !runs {
            continue;
        }
        let opaque = runs || matches!(token.kind, TokenKind::String | TokenKind::QuotedIdent);
        if rules.delimiter_command
            && !has_content
            && cursor <= token.range.start
            && let Some((new_delimiter, line_end)) = delimiter_command(source, token)
        {
            delimiter = new_delimiter;
            segment_start = line_end;
            cursor = line_end;
            blocks = Blocks::default();
            continue;
        }
        // Under a custom delimiter, the delimiter alone decides.
        if delimiter == ";" {
            blocks.see(source, &tokens, index, rules);
        }
        if opaque {
            has_content = true;
            continue;
        }
        let mut position = token.range.start.max(cursor);
        while position < token.range.end {
            let end = position + delimiter.len();
            let found = bytes[position..].starts_with(delimiter.as_bytes())
                // A delimiter may run over several tokens (`//`), but never
                // into a string, a quoted name or a comment. Tokens are in
                // order and cover the text, so only this one and those after
                // it, up to `end`, can overlap.
                && tokens[index..]
                    .iter()
                    .take_while(|other| other.range.start < end)
                    .all(|other| {
                        !matches!(
                            other.kind,
                            TokenKind::String
                                | TokenKind::QuotedIdent
                                | TokenKind::LineComment
                                | TokenKind::BlockComment
                        )
                    });
            if found && blocks.depth == 0 {
                push_statement(source, segment_start..position, end, rules, &mut statements);
                segment_start = end;
                cursor = end;
                has_content = false;
                blocks = Blocks::default();
                position = end;
            } else {
                has_content = true;
                position += 1;
            }
        }
    }
    push_statement(
        source,
        segment_start..source.len(),
        source.len(),
        rules,
        &mut statements,
    );
    statements
}

/// Where the statement being collected stands toward a routine body
/// written without `DELIMITER` (C9). While a block is open (`depth` > 0),
/// `;` does not end the statement.
///
/// A block opens only in a routine's statement (see [`routine_header`]):
/// on PostgreSQL at `BEGIN ATOMIC`, once; on MySQL and MariaDB at a `BEGIN`
/// outside parentheses that is not a name (see [`Blocks::begins`]), nested
/// ones included, and at a statement's leading `BEGIN NOT ATOMIC`. A bare
/// `BEGIN` anywhere else is transaction control and ends at its `;`.
///
/// `END` closes the innermost block when it follows a `;` or the `BEGIN`
/// (`ATOMIC`) of an empty block, and is not `END IF`, `END LOOP`, `END
/// WHILE`, `END REPEAT`, `END CASE` or MariaDB's `END FOR`: every statement
/// of a body ends with `;` in both grammars, while the `END` of a `CASE …
/// END` expression follows an expression, and `end` as a name follows
/// something else too. So `CASE`, `IF`, `LOOP`, `WHILE` and `REPEAT` need no
/// tracking (`IF(…)` and `REPEAT(…)` are also functions), and a label after
/// `END` changes nothing. Once the body closed, it never opens again: the
/// statement ends at the next `;`.
#[derive(Default)]
struct Blocks {
    state: Body,
    /// Open blocks.
    depth: u32,
    /// Open parentheses since the last `;`.
    parens: u32,
    /// The last meaningful token.
    previous: Option<usize>,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Body {
    /// Nothing meaningful seen yet.
    #[default]
    Start,
    /// Not a routine: nothing to track.
    Plain,
    /// A routine definition whose body has not opened.
    Header,
    /// Inside the body.
    Open,
    /// The body closed.
    Closed,
}

/// Words after which `begin` is a name, not a block (MySQL: `BEGIN` and
/// `END` are not reserved there).
const NAME_AFTER: &[&str] = &[
    "SELECT", "RETURN", "DECLARE", "AS", "ON", "FROM", "WHERE", "AND", "OR", "NOT", "BY", "SET",
    "INTO", "WHEN", "IN", "IS", "LIKE", "DISTINCT", "JOIN",
];

/// `END` followed by one of these closes its own construct, not a block.
const END_OF_OTHER: &[&str] = &["IF", "LOOP", "WHILE", "REPEAT", "CASE", "FOR"];

impl Blocks {
    /// Takes the meaningful token at `index` into account.
    fn see(&mut self, source: &str, tokens: &[Token], index: usize, rules: LexRules) {
        let token = &tokens[index];
        let mysql = rules.hash_comments;
        match self.state {
            Body::Start => {
                self.state =
                    if is_word(source, token, "CREATE") && routine_header(source, tokens, index) {
                        Body::Header
                    } else if mysql
                        && is_word(source, token, "BEGIN")
                        && is_next_word(source, tokens, index, "NOT")
                        && next_meaningful(tokens, index)
                            .is_some_and(|not| is_next_word(source, tokens, not, "ATOMIC"))
                    {
                        self.depth = 1;
                        Body::Open
                    } else {
                        Body::Plain
                    };
            }
            Body::Header | Body::Open => self.in_routine(source, tokens, index, mysql),
            Body::Plain | Body::Closed => {}
        }
        self.previous = Some(index);
    }

    fn in_routine(&mut self, source: &str, tokens: &[Token], index: usize, mysql: bool) {
        let token = &tokens[index];
        match (token.kind, &source[token.range.clone()]) {
            (TokenKind::Punct, "(") => self.parens += 1,
            (TokenKind::Punct, ")") => self.parens = self.parens.saturating_sub(1),
            // Parentheses never span statements of a body.
            (TokenKind::Semicolon, _) => self.parens = 0,
            _ => {}
        }
        if self.parens > 0 {
            return;
        }
        let opens = if mysql {
            self.begins(source, tokens, index)
        } else {
            self.state == Body::Header
                && is_word(source, token, "BEGIN")
                && is_next_word(source, tokens, index, "ATOMIC")
        };
        if opens {
            self.depth += 1;
            self.state = Body::Open;
        } else if self.state == Body::Open
            && is_word(source, token, "END")
            && self.ends(source, tokens, index)
        {
            self.depth -= 1;
            if self.depth == 0 {
                self.state = Body::Closed;
            }
        }
    }

    /// Whether the token at `index` opens a MySQL block: `BEGIN`, unless
    /// what surrounds it makes it a name (`SELECT begin`, `NEW.begin`,
    /// `begin, …`, `ON begin`), or `lbl:BEGIN` (one token to the lexer).
    fn begins(&self, source: &str, tokens: &[Token], index: usize) -> bool {
        let token = &tokens[index];
        let text = &source[token.range.clone()];
        if token.kind == TokenKind::NamedParam {
            return text.eq_ignore_ascii_case(":begin");
        }
        if !is_word(source, token, "BEGIN") {
            return false;
        }
        let name_before = self.previous.is_some_and(|previous| {
            let previous = &tokens[previous];
            let text = &source[previous.range.clone()];
            match previous.kind {
                TokenKind::Punct => !matches!(text, ")" | ":"),
                TokenKind::Word => NAME_AFTER
                    .iter()
                    .any(|word| text.eq_ignore_ascii_case(word)),
                _ => false,
            }
        });
        let name_after = next_meaningful(tokens, index).is_some_and(|next| {
            matches!(tokens[next].kind, TokenKind::Punct | TokenKind::Semicolon)
        });
        !name_before && !name_after
    }

    /// Whether the `END` at `index` closes a block.
    fn ends(&self, source: &str, tokens: &[Token], index: usize) -> bool {
        let after_statement = self.previous.is_some_and(|previous| {
            let previous = &tokens[previous];
            previous.kind == TokenKind::Semicolon
                || ["BEGIN", "ATOMIC"]
                    .iter()
                    .any(|word| is_word(source, previous, word))
                || previous.kind == TokenKind::NamedParam
                    && source[previous.range.clone()].eq_ignore_ascii_case(":begin")
        });
        after_statement
            && !next_meaningful(tokens, index).is_some_and(|next| {
                END_OF_OTHER
                    .iter()
                    .any(|word| is_word(source, &tokens[next], word))
            })
    }
}

/// Whether the `CREATE` at `index` defines a routine: `CREATE [OR REPLACE]
/// [DEFINER = user] [AGGREGATE] PROCEDURE | FUNCTION | TRIGGER | EVENT`.
/// The user is `CURRENT_USER[()]`, `CURRENT_ROLE` or a name with an optional
/// `@host`. Only a few tokens are read.
fn routine_header(source: &str, tokens: &[Token], index: usize) -> bool {
    let mut words = tokens[index + 1..]
        .iter()
        .filter(|token| !token.kind.is_trivia());
    let is =
        |token: Option<&Token>, word: &str| token.is_some_and(|token| is_word(source, token, word));
    let punct = |token: Option<&Token>, text: &str| {
        token.is_some_and(|token| {
            token.kind == TokenKind::Punct && &source[token.range.clone()] == text
        })
    };
    let mut next = words.next();
    if is(next, "OR") {
        if !is(words.next(), "REPLACE") {
            return false;
        }
        next = words.next();
    }
    if is(next, "DEFINER") {
        if !punct(words.next(), "=") {
            return false;
        }
        words.next();
        next = words.next();
        if punct(next, "(") {
            if !punct(words.next(), ")") {
                return false;
            }
            next = words.next();
        } else if punct(next, "@") {
            words.next();
            next = words.next();
        }
    }
    if is(next, "AGGREGATE") {
        next = words.next();
    }
    ["PROCEDURE", "FUNCTION", "TRIGGER", "EVENT"]
        .iter()
        .any(|word| is(next, word))
}

fn is_word(source: &str, token: &Token, word: &str) -> bool {
    token.kind == TokenKind::Word && source[token.range.clone()].eq_ignore_ascii_case(word)
}

/// The index of the first meaningful token after `index`.
fn next_meaningful(tokens: &[Token], index: usize) -> Option<usize> {
    tokens[index + 1..]
        .iter()
        .position(|token| !token.kind.is_trivia())
        .map(|offset| index + 1 + offset)
}

fn is_next_word(source: &str, tokens: &[Token], index: usize, word: &str) -> bool {
    next_meaningful(tokens, index).is_some_and(|next| is_word(source, &tokens[next], word))
}

/// Recognises `DELIMITER xx` at `token`. Returns the new delimiter and the
/// byte offset of the end of the line.
fn delimiter_command(source: &str, token: &Token) -> Option<(String, usize)> {
    if token.kind != TokenKind::Word
        || !source[token.range.clone()].eq_ignore_ascii_case("delimiter")
    {
        return None;
    }
    let line_end = source[token.range.end..]
        .find('\n')
        .map_or(source.len(), |offset| token.range.end + offset);
    let argument = source[token.range.end..line_end].trim();
    // The mysql client takes the first whitespace-separated word.
    let delimiter = argument.split_whitespace().next()?;
    Some((delimiter.to_string(), line_end))
}

/// Whether `token` is a comment MySQL and MariaDB run as SQL: `/*! … */`,
/// `/*!NNNNN … */` or MariaDB's `/*M! … */`. It is statement text, never
/// trimmed (C21: cutting a trailing `/*! WHERE id = 1 */` would send a
/// `DELETE` without its condition), and a statement when it stands alone
/// (a dump's `/*!40101 SET … */;`). Elsewhere it is a comment.
fn runs_as_sql(source: &str, token: &Token, rules: LexRules) -> bool {
    let text = &source[token.range.clone()];
    rules.hash_comments
        && token.kind == TokenKind::BlockComment
        && (text.starts_with("/*!") || text.starts_with("/*M!"))
}

/// Records the statement in `segment`, if it holds one.
fn push_statement(
    source: &str,
    segment: Range<usize>,
    delimiter_end: usize,
    rules: LexRules,
    statements: &mut Vec<StatementSpan>,
) {
    if segment.start >= segment.end {
        return;
    }
    // A segment can begin or end inside what was one token of the whole
    // text (a delimiter glued to a word), so it is lexed on its own.
    let text = &source[segment.clone()];
    let tokens = lex(text, rules);
    let mut meaningful = tokens
        .iter()
        .filter(|token| !token.kind.is_trivia() || runs_as_sql(text, token, rules));
    let Some(first) = meaningful.next() else {
        return;
    };
    let last = meaningful.next_back().unwrap_or(first);
    // Leading and trailing comments stay out; they are not part of what
    // runs. Comments inside the statement (optimizer hints) are kept, and
    // MySQL's `/*! */` wherever it stands (see `runs_as_sql`).
    let start = segment.start + first.range.start;
    let end = segment.start + last.range.end;
    // The cursor belongs to the statement up to the end of the delimiter's line.
    let rest = &source[delimiter_end..];
    let trailing = rest
        .find(|c: char| !matches!(c, ' ' | '\t' | '\r'))
        .unwrap_or(rest.len());
    statements.push(StatementSpan {
        range: start..end,
        reach: delimiter_end + trailing,
    });
}

/// The statement the cursor is in or just after, for "run statement at
/// cursor". Between two statements, the cursor picks the one that follows
/// unless it is still on the previous statement's last line.
pub fn statement_at(statements: &[StatementSpan], cursor: usize) -> Option<&StatementSpan> {
    statements
        .iter()
        .find(|statement| cursor <= statement.reach)
        .or(statements.last())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(source: &str, rules: LexRules) -> Vec<&str> {
        split_statements(source, rules)
            .iter()
            .map(|statement| statement.text(source))
            .collect()
    }

    fn pg(source: &str) -> Vec<&str> {
        texts(source, LexRules::POSTGRES)
    }

    fn my(source: &str) -> Vec<&str> {
        texts(source, LexRules::MYSQL)
    }

    #[test]
    fn splits_on_semicolons_and_drops_empty_statements() {
        assert_eq!(
            texts("select 1;\n\n select 2 ;;  ; select 3", LexRules::POSTGRES),
            vec!["select 1", "select 2", "select 3"]
        );
        assert!(texts("", LexRules::POSTGRES).is_empty());
        assert!(texts(" ;; \n -- only a comment\n /* x */ ", LexRules::POSTGRES).is_empty());
    }

    #[test]
    fn semicolons_inside_strings_comments_and_bodies_do_not_split() {
        let source = "insert into t values ('a;b'); -- c;d\n\
                      create function f() returns int as $$ begin; return 1; end $$ language plpgsql;\n\
                      select \"x;y\" /* ; */ from t";
        assert_eq!(
            texts(source, LexRules::POSTGRES),
            vec![
                "insert into t values ('a;b')",
                "create function f() returns int as $$ begin; return 1; end $$ language plpgsql",
                "select \"x;y\" /* ; */ from t",
            ]
        );
    }

    #[test]
    fn leading_comments_are_left_out_but_inner_ones_are_kept() {
        let source = "-- header\n/* note */ select /*+ hint */ 1 -- trailing\n; select 2";
        assert_eq!(
            texts(source, LexRules::POSTGRES),
            vec!["select /*+ hint */ 1", "select 2"]
        );
    }

    /// C21: MySQL and MariaDB run what is inside `/*! … */` and `/*M! … */`,
    /// so such a comment is part of the statement wherever it stands, and a
    /// statement of its own when it stands alone (a dump's first lines).
    #[test]
    fn mysql_executable_comments_are_statement_text() {
        let source = "/* note */ DELETE FROM t /*! WHERE id = 1 */ -- why\n;\n\
                      /*!40101 SET NAMES utf8 */;\n\
                      /*M!100100 SET x = 1 */ ; /* plain */ ;\n\
                      /*!50000 DELETE FROM t */ /*!90000 WHERE id = 1 */";
        assert_eq!(
            texts(source, LexRules::MYSQL),
            vec![
                "DELETE FROM t /*! WHERE id = 1 */",
                "/*!40101 SET NAMES utf8 */",
                "/*M!100100 SET x = 1 */",
                "/*!50000 DELETE FROM t */ /*!90000 WHERE id = 1 */",
            ]
        );
        // A `;` inside one does not split, as in any comment.
        assert_eq!(
            texts("/*!50003 SET a = 1; */ select 2", LexRules::MYSQL),
            vec!["/*!50003 SET a = 1; */ select 2"]
        );
        // On PostgreSQL they are comments like any other.
        assert_eq!(
            texts(
                "/*!40101 SET NAMES utf8 */; select 1 /*! x */",
                LexRules::POSTGRES
            ),
            vec!["select 1"]
        );
    }

    #[test]
    fn unterminated_input_is_one_statement() {
        assert_eq!(
            texts("select 'abc; select 2", LexRules::POSTGRES),
            vec!["select 'abc; select 2"]
        );
    }

    #[test]
    fn mysql_delimiter_command() {
        let source = "select 1;\n\
                      DELIMITER //\n\
                      create procedure p()\nbegin\n  select 1;\n  select 2;\nend//\n\
                      create trigger t before insert on x for each row set new.a = 1;//\n\
                      delimiter ;\n\
                      select 3;";
        assert_eq!(
            texts(source, LexRules::MYSQL),
            vec![
                "select 1",
                "create procedure p()\nbegin\n  select 1;\n  select 2;\nend",
                "create trigger t before insert on x for each row set new.a = 1;",
                "select 3",
            ]
        );
    }

    #[test]
    fn delimiter_is_only_a_command_at_statement_start_and_only_on_mysql() {
        // A column named delimiter.
        assert_eq!(
            texts("select delimiter from t; select 2", LexRules::MYSQL),
            vec!["select delimiter from t", "select 2"]
        );
        // PostgreSQL has no DELIMITER command.
        assert_eq!(
            texts("delimiter //\nselect 1;", LexRules::POSTGRES),
            vec!["delimiter //\nselect 1"]
        );
        // `$$` as a custom delimiter on MySQL.
        assert_eq!(
            texts(
                "delimiter $$\nselect 1; select 2$$\nselect 3$$",
                LexRules::MYSQL
            ),
            vec!["select 1; select 2", "select 3"]
        );
    }

    #[test]
    fn a_custom_delimiter_glued_to_a_word_still_ends_the_statement() {
        // `$` may be part of an identifier, so `END$$` lexes as one word;
        // the mysql client nevertheless ends the statement there.
        let source =
            "DELIMITER $$\ncreate procedure p()\nbegin\n  select 1;\nEND$$\nDELIMITER ;\nselect 2;";
        assert_eq!(
            texts(source, LexRules::MYSQL),
            vec!["create procedure p()\nbegin\n  select 1;\nEND", "select 2"]
        );
        // Same with a delimiter made of letters, and one glued on both sides.
        assert_eq!(
            texts(
                "delimiter go\nselect 1 go select 2go\nselect 3 go",
                LexRules::MYSQL
            ),
            vec!["select 1", "select 2", "select 3"]
        );
        // The delimiter inside a string, a quoted name or a comment is text
        // (and a trailing comment is not part of the statement).
        assert_eq!(
            texts(
                "delimiter $$\nselect '$$', `a$$b` /* $$ */ $$ select 2 $$",
                LexRules::MYSQL
            ),
            vec!["select '$$', `a$$b`", "select 2"]
        );
    }

    /// C9: a PostgreSQL 14+ `BEGIN ATOMIC … END` body is part of its
    /// `CREATE FUNCTION` or `CREATE PROCEDURE`; a bare `BEGIN` is a
    /// transaction and still ends at its `;`.
    #[test]
    fn begin_atomic_bodies_stay_one_statement() {
        assert_eq!(
            pg(
                "CREATE FUNCTION f() RETURNS int LANGUAGE sql BEGIN ATOMIC SELECT 1; SELECT 2; END; SELECT 3;"
            ),
            vec![
                "CREATE FUNCTION f() RETURNS int LANGUAGE sql BEGIN ATOMIC SELECT 1; SELECT 2; END",
                "SELECT 3",
            ]
        );
        assert_eq!(
            pg(
                "create or replace procedure p(a int)\nlanguage sql\nbegin atomic\n  insert into t values (a);\n  \
                select case when a > 0 then 1 else 0 end;\n  select 1 as end, \"end\";\nend;\nselect 3"
            ),
            vec![
                "create or replace procedure p(a int)\nlanguage sql\nbegin atomic\n  insert into t values (a);\n  \
                 select case when a > 0 then 1 else 0 end;\n  select 1 as end, \"end\";\nend",
                "select 3",
            ]
        );
        // An empty body, and a single-statement body without a block.
        assert_eq!(
            pg("CREATE FUNCTION f() RETURNS int LANGUAGE sql BEGIN ATOMIC END; SELECT 2"),
            vec![
                "CREATE FUNCTION f() RETURNS int LANGUAGE sql BEGIN ATOMIC END",
                "SELECT 2"
            ]
        );
        assert_eq!(
            pg("CREATE FUNCTION f() RETURNS int LANGUAGE sql RETURN 1; SELECT 2"),
            vec![
                "CREATE FUNCTION f() RETURNS int LANGUAGE sql RETURN 1",
                "SELECT 2"
            ]
        );
        // Transaction control keeps splitting, and so do names.
        assert_eq!(
            pg(
                "BEGIN; SELECT 1; END; BEGIN TRANSACTION; COMMIT; BEGIN ISOLATION LEVEL SERIALIZABLE; \
                BEGIN WORK; SELECT 1 AS begin, \"end\" FROM t; BEGIN ATOMIC; SELECT 2"
            ),
            vec![
                "BEGIN",
                "SELECT 1",
                "END",
                "BEGIN TRANSACTION",
                "COMMIT",
                "BEGIN ISOLATION LEVEL SERIALIZABLE",
                "BEGIN WORK",
                "SELECT 1 AS begin, \"end\" FROM t",
                "BEGIN ATOMIC",
                "SELECT 2",
            ]
        );
        // Words in strings, comments, quoted names and dollar bodies do not count.
        assert_eq!(
            pg(
                "CREATE FUNCTION f() RETURNS text LANGUAGE sql BEGIN ATOMIC SELECT 'end;' -- end;\n; \
                SELECT $$ end; $$ /* ; end */; END; SELECT 2"
            ),
            vec![
                "CREATE FUNCTION f() RETURNS text LANGUAGE sql BEGIN ATOMIC SELECT 'end;' -- end;\n; \
                 SELECT $$ end; $$ /* ; end */; END",
                "SELECT 2"
            ]
        );
        // `BEGIN ATOMIC` belongs to a function or a procedure only.
        assert_eq!(
            pg("CREATE TABLE t (a int); BEGIN ATOMIC; SELECT 1"),
            vec!["CREATE TABLE t (a int)", "BEGIN ATOMIC", "SELECT 1"]
        );
        // Text after the final END belongs to the statement up to the next `;`
        // (the server refuses it).
        assert_eq!(
            pg(
                "CREATE FUNCTION f() RETURNS int LANGUAGE sql BEGIN ATOMIC SELECT 1; END x; SELECT 2"
            ),
            vec![
                "CREATE FUNCTION f() RETURNS int LANGUAGE sql BEGIN ATOMIC SELECT 1; END x",
                "SELECT 2"
            ]
        );
    }

    /// C9: a MySQL or MariaDB routine, trigger or event whose body is a
    /// `BEGIN … END` compound statement is one statement without `DELIMITER`,
    /// as the server reads it.
    #[test]
    fn mysql_compound_bodies_stay_one_statement() {
        assert_eq!(
            my("CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END; SELECT 3;"),
            vec![
                "CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END",
                "SELECT 3"
            ]
        );
        // Every kind, the DEFINER forms, OR REPLACE, AGGREGATE, IF NOT EXISTS.
        for create in [
            "CREATE FUNCTION f() RETURNS int DETERMINISTIC",
            "CREATE OR REPLACE PROCEDURE p(IN begin INT, OUT end INT)",
            "CREATE DEFINER=`kerno`@`%` PROCEDURE `p`()",
            "CREATE DEFINER = 'kerno'@'localhost' FUNCTION f() RETURNS varchar(10) CHARSET utf8mb4 \
             COMMENT 'x' READS SQL DATA",
            "CREATE DEFINER=kerno@localhost PROCEDURE p()",
            "CREATE DEFINER = CURRENT_USER() PROCEDURE p()",
            "CREATE DEFINER = CURRENT_USER PROCEDURE IF NOT EXISTS p()",
            "CREATE AGGREGATE FUNCTION a(x INT) RETURNS INT",
            "CREATE TRIGGER t BEFORE INSERT ON x FOR EACH ROW",
            "CREATE TRIGGER t BEFORE INSERT ON x FOR EACH ROW FOLLOWS `u`",
            "CREATE EVENT e ON SCHEDULE EVERY 1 DAY DO",
            "CREATE PROCEDURE p() lbl:",
        ] {
            let body = "BEGIN DECLARE x INT; SET NEW.begin = 1; SET NEW.end = 2; END";
            let source = format!("{create} {body}; SELECT 3");
            assert_eq!(
                my(&source),
                vec![format!("{create} {body}"), "SELECT 3".to_string()],
                "{create}"
            );
        }
        // What closes its own construct, not the block; nested blocks, labels,
        // handlers, `CASE … END` expressions.
        let body = "CREATE PROCEDURE p()\nmain: BEGIN\n\
                    DECLARE CONTINUE HANDLER FOR NOT FOUND BEGIN SET @done = 1; END;\n\
                    DECLARE EXIT HANDLER FOR SQLSTATE '23000' BEGIN ROLLBACK; END;\n\
                    IF 1 THEN BEGIN SELECT 1; END; ELSEIF 2 THEN SELECT 2; ELSE SELECT 3; END IF;\n\
                    l: LOOP LEAVE l; END LOOP l;\n\
                    WHILE 0 DO SELECT 4; END WHILE;\n\
                    REPEAT SELECT 5; UNTIL 1 END REPEAT;\n\
                    CASE @x WHEN 1 THEN SELECT 6; ELSE BEGIN END; END CASE;\n\
                    SELECT CASE WHEN 1 THEN 'a' ELSE 'b' END, IF(1, 2, 3), REPEAT('x', 2);\n\
                    SELECT start, end, begin FROM t;\n\
                    inner_block: BEGIN SELECT 7; END inner_block;\n\
                    END main";
        assert_eq!(my(&format!("{body};\nSELECT 8;")), vec![body, "SELECT 8"]);
        // MariaDB's anonymous compound statement.
        assert_eq!(
            my("BEGIN NOT ATOMIC SELECT 1; SELECT 2; END; SELECT 3"),
            vec!["BEGIN NOT ATOMIC SELECT 1; SELECT 2; END", "SELECT 3"]
        );
        // A single-statement body ends at its `;`, `begin` and `end` as names
        // too, and transaction control keeps splitting.
        assert_eq!(
            my("CREATE PROCEDURE p() SELECT begin, end FROM t; SELECT 2; \
                CREATE FUNCTION f() RETURNS int DETERMINISTIC RETURN 1; \
                BEGIN; BEGIN WORK; SELECT begin FROM t; COMMIT; \
                CREATE TABLE event (begin int, end int); CREATE TABLE function (a int); BEGIN; SELECT 4"),
            vec![
                "CREATE PROCEDURE p() SELECT begin, end FROM t",
                "SELECT 2",
                "CREATE FUNCTION f() RETURNS int DETERMINISTIC RETURN 1",
                "BEGIN",
                "BEGIN WORK",
                "SELECT begin FROM t",
                "COMMIT",
                "CREATE TABLE event (begin int, end int)",
                "CREATE TABLE function (a int)",
                "BEGIN",
                "SELECT 4",
            ]
        );
        // Words in strings, comments and quoted names do not count.
        assert_eq!(
            my(
                "CREATE PROCEDURE p() BEGIN SELECT 'end;', \"end;\", `end`; # end;\n \
                SELECT 1 /* ; end */; END; SELECT 2"
            ),
            vec![
                "CREATE PROCEDURE p() BEGIN SELECT 'end;', \"end;\", `end`; # end;\n \
                 SELECT 1 /* ; end */; END",
                "SELECT 2"
            ]
        );
        // A trigger on a table named `begin` has a single-statement body.
        assert_eq!(
            my("CREATE TRIGGER t BEFORE INSERT ON begin FOR EACH ROW SET NEW.a = 1; SELECT 2"),
            vec![
                "CREATE TRIGGER t BEFORE INSERT ON begin FOR EACH ROW SET NEW.a = 1",
                "SELECT 2"
            ]
        );
    }

    /// C9: with `DELIMITER` set, the delimiter decides, blocks or not; a
    /// dump's executable comments are opaque.
    #[test]
    fn a_delimiter_still_decides_and_dumps_are_unchanged() {
        assert_eq!(
            my("DELIMITER //\nCREATE PROCEDURE p() BEGIN SELECT 1; END//\nDELIMITER ;\nSELECT 2;"),
            vec!["CREATE PROCEDURE p() BEGIN SELECT 1; END", "SELECT 2"]
        );
        // The delimiter inside a block still ends the statement.
        assert_eq!(
            my("DELIMITER //\nCREATE PROCEDURE p() BEGIN SELECT 1// SELECT 2; END//"),
            vec!["CREATE PROCEDURE p() BEGIN SELECT 1", "SELECT 2; END"]
        );
        // As mysqldump writes a routine.
        let dump = "DELIMITER ;;\n\
                    /*!50003 CREATE*/ /*!50020 DEFINER=`root`@`%`*/ /*!50003 PROCEDURE `p`()\n\
                    BEGIN\n  SELECT 1;\n  SELECT 2;\nEND */;;\n\
                    DELIMITER ;\n\
                    /*!50003 SET sql_mode = @saved_sql_mode */ ;";
        assert_eq!(
            my(dump),
            vec![
                "/*!50003 CREATE*/ /*!50020 DEFINER=`root`@`%`*/ /*!50003 PROCEDURE `p`()\n\
                 BEGIN\n  SELECT 1;\n  SELECT 2;\nEND */",
                "/*!50003 SET sql_mode = @saved_sql_mode */",
            ]
        );
        // The same comments without DELIMITER: their `;` never counted.
        assert_eq!(
            my("/*!50003 CREATE*/ /*!50003 PROCEDURE p() BEGIN SELECT 1; END */; SELECT 2"),
            vec![
                "/*!50003 CREATE*/ /*!50003 PROCEDURE p() BEGIN SELECT 1; END */",
                "SELECT 2"
            ]
        );
    }

    /// C9: a body still being typed holds the rest of the text: nothing
    /// after it runs on its own, and the server refuses the whole.
    #[test]
    fn an_unfinished_body_holds_the_rest_of_the_text() {
        assert_eq!(
            texts(
                "SELECT 0; CREATE PROCEDURE p() BEGIN SELECT 1; DELETE FROM t; SELECT 3;",
                LexRules::MYSQL
            ),
            vec![
                "SELECT 0",
                "CREATE PROCEDURE p() BEGIN SELECT 1; DELETE FROM t; SELECT 3;"
            ]
        );
        assert_eq!(
            texts(
                "CREATE FUNCTION f() RETURNS int LANGUAGE sql BEGIN ATOMIC SELECT 1; SELECT 2",
                LexRules::POSTGRES
            ),
            vec!["CREATE FUNCTION f() RETURNS int LANGUAGE sql BEGIN ATOMIC SELECT 1; SELECT 2"]
        );
        // A runaway block stays linear.
        let source = format!(
            "CREATE PROCEDURE p() BEGIN\n{}",
            "select case when a then 1 end;\n".repeat(20_000)
        );
        let started = std::time::Instant::now();
        let statements = split_statements(&source, LexRules::MYSQL);
        let elapsed = started.elapsed();
        assert_eq!(statements.len(), 1);
        assert!(
            elapsed < std::time::Duration::from_millis(200),
            "a 20,000-line body took {elapsed:?}"
        );
    }

    /// C9: "run statement at cursor" inside a body runs the whole `CREATE`.
    #[test]
    fn statement_at_cursor_inside_a_body() {
        for (source, rules) in [
            (
                "SELECT 0;\nCREATE FUNCTION f() RETURNS int LANGUAGE sql\nBEGIN ATOMIC\n  SELECT 1;\n  SELECT 2;\nEND;\nSELECT 3;",
                LexRules::POSTGRES,
            ),
            (
                "SELECT 0;\nCREATE PROCEDURE p()\nBEGIN\n  SELECT 1;\n  SELECT 2;\nEND;\nSELECT 3;",
                LexRules::MYSQL,
            ),
        ] {
            let statements = split_statements(source, rules);
            let at = |cursor: usize| statement_at(&statements, cursor).map(|s| s.text(source));
            let create =
                &source[source.find("CREATE").unwrap()..source.find(";\nSELECT 3").unwrap()];
            assert_eq!(at(source.find("SELECT 2").unwrap() + 3), Some(create));
            assert_eq!(at(source.find("SELECT 1").unwrap()), Some(create));
            assert_eq!(at(source.find("END").unwrap() + 1), Some(create));
            assert_eq!(at(source.len()), Some("SELECT 3"));
            assert_eq!(at(2), Some("SELECT 0"));
        }
    }

    #[test]
    fn a_long_script_splits_in_linear_time() {
        let source = "select 1;\n".repeat(20_000);
        let started = std::time::Instant::now();
        let statements = split_statements(&source, LexRules::MYSQL);
        let elapsed = started.elapsed();
        assert_eq!(statements.len(), 20_000);
        assert!(
            elapsed < std::time::Duration::from_millis(200),
            "20,000 statements took {elapsed:?}"
        );
    }

    #[test]
    fn ranges_index_the_original_text() {
        let source = "  select 1 ;\n\tselect é ; ";
        let statements = split_statements(source, LexRules::POSTGRES);
        assert_eq!(&source[statements[0].range.clone()], "select 1");
        assert_eq!(&source[statements[1].range.clone()], "select é");
    }

    #[test]
    fn statement_at_cursor() {
        let source = "select 1;  \nselect 2;\n\n\nselect 3";
        let statements = split_statements(source, LexRules::POSTGRES);
        let at = |cursor: usize| statement_at(&statements, cursor).map(|s| s.text(source));

        assert_eq!(at(0), Some("select 1"));
        assert_eq!(at(4), Some("select 1"));
        // Just after the semicolon, and in the trailing spaces of that line.
        assert_eq!(at(9), Some("select 1"));
        assert_eq!(at(11), Some("select 1"));
        // Start of the next line belongs to the next statement.
        assert_eq!(at(12), Some("select 2"));
        // On a blank line between statements: the one that follows.
        assert_eq!(at(source.find("\n\n\n").unwrap() + 2), Some("select 3"));
        // Past the end: the last one.
        assert_eq!(at(source.len()), Some("select 3"));
        assert_eq!(statement_at(&[], 0), None);
    }
}
