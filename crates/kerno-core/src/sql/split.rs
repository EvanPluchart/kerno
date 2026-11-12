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
/// themselves. Empty statements and comment-only stretches are dropped.
pub fn split_statements(source: &str, rules: LexRules) -> Vec<StatementSpan> {
    let tokens = lex(source, rules);
    let mut statements = Vec::new();
    let mut delimiter = String::from(";");
    // First token of the statement being collected.
    let mut first = 0;
    let mut index = 0;

    while index < tokens.len() {
        let token = &tokens[index];

        if rules.delimiter_command
            && is_statement_start(&tokens[first..index])
            && let Some((new_delimiter, line_end)) = delimiter_command(source, token)
        {
            push_statement(
                source,
                &tokens[first..index],
                token.range.start,
                &mut statements,
            );
            delimiter = new_delimiter;
            // Skip the rest of the DELIMITER line.
            while index < tokens.len() && tokens[index].range.start < line_end {
                index += 1;
            }
            first = index;
            continue;
        }

        let at_delimiter = match token.kind {
            TokenKind::Semicolon | TokenKind::Punct => {
                source[token.range.start..].starts_with(delimiter.as_str())
            }
            _ => false,
        };
        if at_delimiter {
            let delimiter_end = token.range.start + delimiter.len();
            push_statement(
                source,
                &tokens[first..index],
                delimiter_end,
                &mut statements,
            );
            while index < tokens.len() && tokens[index].range.start < delimiter_end {
                index += 1;
            }
            first = index;
            continue;
        }
        index += 1;
    }
    push_statement(source, &tokens[first..], source.len(), &mut statements);
    statements
}

/// Whether nothing but whitespace and comments was collected so far.
fn is_statement_start(tokens: &[Token]) -> bool {
    tokens.iter().all(|token| token.kind.is_trivia())
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

fn push_statement(
    source: &str,
    tokens: &[Token],
    delimiter_end: usize,
    statements: &mut Vec<StatementSpan>,
) {
    let mut meaningful = tokens.iter().filter(|token| !token.kind.is_trivia());
    let Some(first) = meaningful.next() else {
        return;
    };
    let last = meaningful.next_back().unwrap_or(first);
    // Leading comments stay out; they are not part of what runs. Comments
    // inside the statement (optimizer hints, MySQL `/*! */`) are kept.
    let start = first.range.start;
    let end = last.range.end;
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
