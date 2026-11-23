//! The assistant asked in the editor (K52, D-121): what a request made at the cursor sends, and
//! the proposal it brings back as a line diff against the SQL it would replace. No GPUI: the
//! editor's field and its diff (`crates/kerno/src/query/ai_inline.rs`) use these. The body is
//! made by [`super::build_chat`], as every other request's; sample rows never go with these.

use std::ops::Range;

use super::{Segment, segments};

/// What is asked at the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inline {
    /// The user's sentence, about the SQL to change (the selection or the statement at the
    /// cursor), or about nothing when the cursor's line is empty: the model writes from nothing.
    Ask {
        sentence: String,
        sql: Option<String>,
    },
    /// A statement the server refused, with what the error view shows of the refusal (message
    /// and code, detail, hint) and the character the server pointed at.
    Fix {
        sql: String,
        error: String,
        position: Option<u32>,
    },
}

impl Inline {
    /// The user message: the instruction, then the subject.
    pub fn message(&self, engine: &str) -> String {
        let engine = if engine.is_empty() { "SQL" } else { engine };
        match self {
            Self::Ask {
                sentence,
                sql: Some(sql),
            } => format!(
                "Change this {engine} SQL as the request says, using only the tables and columns \
                 described. Give the whole new SQL in one ```sql block (it replaces the SQL below \
                 as a whole), then at most one sentence about the change.\n\nRequest: {}\n\nThe \
                 SQL to change:\n```sql\n{}\n```",
                sentence.trim(),
                sql.trim()
            ),
            Self::Ask {
                sentence,
                sql: None,
            } => format!(
                "Write {engine} SQL for this request, using only the tables and columns \
                 described. Give it in one ```sql block, then at most one sentence about \
                 it.\n\nRequest: {}",
                sentence.trim()
            ),
            Self::Fix {
                sql,
                error,
                position,
            } => {
                let at = position
                    .map(|at| format!("\nThe server points at character {at} of the statement."))
                    .unwrap_or_default();
                format!(
                    "This {engine} statement failed with the error below. Correct it, using only \
                     the tables and columns described. Give the whole corrected statement in one \
                     ```sql block (it replaces the statement as a whole), then at most one \
                     sentence about the fix.\n\nStatement:\n```sql\n{}\n```\n\nError:\n{}{at}",
                    sql.trim(),
                    error.trim()
                )
            }
        }
    }

    /// The text whose table names choose the tables described.
    pub fn text(&self) -> String {
        match self {
            Self::Ask { sentence, sql } => format!("{}\n{sentence}", sql.as_deref().unwrap_or("")),
            Self::Fix { sql, error, .. } => format!("{sql}\n{error}"),
        }
    }
}

/// The message of a refinement: the user's further sentence about the last proposal, which the
/// earlier turns carry.
pub fn refine(sentence: &str) -> String {
    format!(
        "Change your last SQL as this says: {}\nGive the whole SQL again in one ```sql block.",
        sentence.trim()
    )
}

/// The SQL an answer proposes: its first closed ```sql block, or closed block with no language.
/// `None` for an answer without one: its text is shown, nothing is offered to accept.
pub fn proposed_sql(answer: &str) -> Option<String> {
    segments(answer)
        .iter()
        .find_map(Segment::sql)
        .map(|sql| sql.trim().to_string())
        .filter(|sql| !sql.is_empty())
}

/// What the editor's `range` of `text` becomes on Accept, with `sql` the answer's: the buffer's
/// line ending kept; over a statement whose `;` stays after the range, the answer's own final
/// `;` left out (else it would be doubled); at a caret, the SQL on its own line
/// ([`super::placed`]) and ended by `;` when text follows it (else it would join the next
/// statement).
pub fn replacement(text: &str, range: Range<usize>, sql: &str) -> String {
    let crlf = if range.is_empty() {
        text.contains("\r\n")
    } else {
        text[range.clone()].contains("\r\n")
    };
    let mut sql = sql.replace("\r\n", "\n").trim().to_string();
    let after = &text[range.end..];
    if range.is_empty() {
        if !sql.ends_with(';') && !after.trim().is_empty() {
            sql.push(';');
        }
        sql = super::placed(text, range, &sql).0;
    } else if after.trim_start_matches([' ', '\t']).starts_with(';') {
        sql = sql.trim_end_matches(';').trim_end().to_string();
    }
    if crlf {
        sql = sql.replace('\n', "\r\n");
    }
    sql
}

/// Whether `range` of `text` still holds `original`: the place a request was made on was not
/// edited or moved while it waited (an answer to another text is dropped).
pub fn still_there(text: &str, range: &Range<usize>, original: &str) -> bool {
    text.get(range.clone()) == Some(original)
}

/// How a line of a proposal stands against the SQL it replaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Kept,
    Removed,
    Added,
}

impl Change {
    /// The sign in the diff's margin: the change is never said by colour alone.
    pub fn sign(self) -> &'static str {
        match self {
            Self::Kept => " ",
            Self::Removed => "−",
            Self::Added => "+",
        }
    }
}

/// One line of a diff, without its line ending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub change: Change,
    pub text: String,
}

/// Lines compared at most (old × new) by the longest common subsequence; beyond, every old
/// line is removed and every new one added.
const MAX_CELLS: usize = 4_000_000;

/// The lines of `new` against those of `old`, in order: kept, removed (before the lines added
/// in their place), added. Lines are compared without their ending (`\r\n` and `\n` alike);
/// tabs and spaces count as written.
pub fn diff(old: &str, new: &str) -> Vec<Line> {
    let old: Vec<&str> = old.lines().collect();
    let new: Vec<&str> = new.lines().collect();
    let line = |change, text: &str| Line {
        change,
        text: text.to_string(),
    };
    // ponytail: a full O(n·m) table, fine for statements; Myers' diff if scripts of
    // thousands of lines are ever asked about.
    if old.len() * new.len() > MAX_CELLS {
        return (old.iter().map(|text| line(Change::Removed, text)))
            .chain(new.iter().map(|text| line(Change::Added, text)))
            .collect();
    }
    // `common[i][j]`: the longest common subsequence of old[i..] and new[j..].
    let width = new.len() + 1;
    let mut common = vec![0u32; (old.len() + 1) * width];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            common[i * width + j] = if old[i] == new[j] {
                common[(i + 1) * width + j + 1] + 1
            } else {
                common[(i + 1) * width + j].max(common[i * width + j + 1])
            };
        }
    }
    let (mut i, mut j, mut lines) = (0, 0, Vec::with_capacity(old.len() + new.len()));
    while i < old.len() || j < new.len() {
        if i < old.len() && j < new.len() && old[i] == new[j] {
            lines.push(line(Change::Kept, old[i]));
            (i, j) = (i + 1, j + 1);
        } else if j == new.len()
            || (i < old.len() && common[(i + 1) * width + j] >= common[i * width + j + 1])
        {
            lines.push(line(Change::Removed, old[i]));
            i += 1;
        } else {
            lines.push(line(Change::Added, new[j]));
            j += 1;
        }
    }
    lines
}

/// The bytes of `old` (without line endings) of each line `lines` removes, for the editor to
/// tint them in place.
pub fn removed_ranges(old: &str, lines: &[Line]) -> Vec<Range<usize>> {
    let mut starts = old.split_inclusive('\n').scan(0, |at, line| {
        let start = *at;
        *at += line.len();
        Some(start..start + line.trim_end_matches(['\n', '\r']).len())
    });
    (lines.iter())
        .filter(|line| line.change != Change::Added)
        .filter_map(|line| Some((line.change, starts.next()?)))
        .filter(|(change, _)| *change == Change::Removed)
        .map(|(_, range)| range)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::{Context, Table, Turn, body, build_chat};

    fn context() -> Context {
        Context {
            engine: "PostgreSQL".into(),
            schemas: vec!["shop".into()],
            tables: vec![Table {
                schema: "shop".into(),
                name: "orders".into(),
                view: false,
                columns: vec![
                    ("id".into(), "bigint".into()),
                    ("total".into(), "numeric".into()),
                ],
                foreign_keys: Vec::new(),
                detail: None,
            }],
            ..Context::default()
        }
    }

    fn signs(lines: &[Line]) -> Vec<String> {
        (lines.iter())
            .map(|line| format!("{}{}", line.change.sign(), line.text))
            .collect()
    }

    /// K52: an ask sends the schema context, the SQL to change and the sentence, once each;
    /// no sample rows; on an empty line no SQL at all.
    #[test]
    fn an_ask_sends_the_sql_to_change_and_the_sentence() {
        let ask = Inline::Ask {
            sentence: " only the orders of this year ".into(),
            sql: Some("SELECT * FROM shop.orders".into()),
        };
        let payload = build_chat(&context(), &[], &ask.message("PostgreSQL"), "m");
        let messages = payload["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        let user = messages[1]["content"].as_str().unwrap();
        assert!(
            user.contains("Request: only the orders of this year\n"),
            "{user}"
        );
        assert!(
            user.ends_with("The SQL to change:\n```sql\nSELECT * FROM shop.orders\n```"),
            "{user}"
        );
        let bytes = String::from_utf8(body(&payload)).unwrap();
        assert_eq!(bytes.matches("shop.orders (table)").count(), 1);
        assert!(!bytes.contains("Sample rows"), "{bytes}");
        assert!(ask.text().contains("shop.orders"));
        // From nothing: the sentence alone.
        let blank = Inline::Ask {
            sentence: "list the customers without an order".into(),
            sql: None,
        }
        .message("PostgreSQL");
        assert!(blank.starts_with("Write PostgreSQL SQL"), "{blank}");
        assert!(!blank.contains("The SQL to change"), "{blank}");
        assert!(blank.ends_with("Request: list the customers without an order"));
    }

    /// K52: a fix sends the statement and the error as the error view shows it, with the
    /// place the server pointed at.
    #[test]
    fn a_fix_sends_the_statement_the_error_and_its_place() {
        let fix = Inline::Fix {
            sql: "SELECT totl FROM shop.orders".into(),
            error: "column \"totl\" does not exist [42703]\nHint: Perhaps you meant \"total\"."
                .into(),
            position: Some(8),
        };
        let message = fix.message("PostgreSQL");
        assert!(message.contains("```sql\nSELECT totl FROM shop.orders\n```"));
        assert!(message.contains("Error:\ncolumn \"totl\" does not exist [42703]\nHint:"));
        assert!(
            message.ends_with("points at character 8 of the statement."),
            "{message}"
        );
        let without = Inline::Fix {
            sql: "SELECT totl FROM shop.orders".into(),
            error: "column \"totl\" does not exist".into(),
            position: None,
        };
        assert!(!without.message("").contains("points at"));
        assert!(fix.text().contains("shop.orders"));
    }

    /// K52: a refinement goes with the turns so far, the schema once.
    #[test]
    fn a_refinement_goes_with_the_turns_so_far() {
        let first = Inline::Ask {
            sentence: "count the orders".into(),
            sql: None,
        }
        .message("PostgreSQL");
        let earlier = [
            Turn::user(first.clone()),
            Turn::assistant("```sql\nSELECT count(*) FROM shop.orders;\n```"),
        ];
        let payload = build_chat(&context(), &earlier, &refine("no, by month"), "m");
        let messages = payload["messages"].as_array().unwrap();
        let roles: Vec<&str> = (messages.iter())
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, ["system", "user", "assistant", "user"]);
        assert_eq!(messages[1]["content"], first.as_str());
        assert!(
            messages[3]["content"]
                .as_str()
                .unwrap()
                .starts_with("Change your last SQL as this says: no, by month\n")
        );
        let bytes = String::from_utf8(body(&payload)).unwrap();
        assert_eq!(bytes.matches("shop.orders (table)").count(), 1);
    }

    #[test]
    fn only_a_closed_sql_block_is_proposed() {
        assert_eq!(
            proposed_sql("Here:\n```sql\nSELECT 1;\n```\nDone.").as_deref(),
            Some("SELECT 1;")
        );
        assert_eq!(
            proposed_sql("```\nSELECT 2\n```").as_deref(),
            Some("SELECT 2")
        );
        // Prose, another language, a block still open, an empty one: nothing to accept.
        assert_eq!(proposed_sql("SELECT is not possible here."), None);
        assert_eq!(proposed_sql("```python\nx\n```"), None);
        assert_eq!(proposed_sql("```sql\nSELECT 3"), None);
        assert_eq!(proposed_sql("```sql\n\n```"), None);
    }

    #[test]
    fn the_diff_says_lines_kept_removed_and_added() {
        let old = "SELECT id\nFROM shop.orders\nORDER BY id";
        let new = "SELECT id, total\nFROM shop.orders\nWHERE total > 10\nORDER BY id";
        assert_eq!(
            signs(&diff(old, new)),
            [
                "−SELECT id",
                "+SELECT id, total",
                " FROM shop.orders",
                "+WHERE total > 10",
                " ORDER BY id"
            ]
        );
        // The same text: everything kept.
        assert!(diff(old, old).iter().all(|l| l.change == Change::Kept));
        // The lines removed, by their bytes in the old text, CRLF left out.
        let crlf = "SELECT id\r\nFROM shop.orders\r\nORDER BY id";
        let lines = diff(crlf, "SELECT id\nFROM shop.customers\nORDER BY id");
        let removed = removed_ranges(crlf, &lines);
        assert_eq!((removed.len(), removed[0].clone()), (1, 11..27));
        assert_eq!(&crlf[11..27], "FROM shop.orders");
        let removed = removed_ranges(old, &diff(old, new));
        assert_eq!((removed.len(), removed[0].clone()), (1, 0..9));
    }

    #[test]
    fn a_pure_insertion_and_a_whole_replacement() {
        assert_eq!(
            signs(&diff("", "SELECT 1\nFROM t")),
            ["+SELECT 1", "+FROM t"]
        );
        assert_eq!(
            signs(&diff("SELECT a\nFROM b", "DELETE FROM c")),
            ["−SELECT a", "−FROM b", "+DELETE FROM c"]
        );
        assert_eq!(signs(&diff("SELECT 1", "")), ["−SELECT 1"]);
    }

    /// CRLF and LF lines are the same line; tabs and spaces are not.
    #[test]
    fn line_endings_are_alike_and_tabs_count() {
        assert_eq!(
            signs(&diff("SELECT 1\r\nFROM t\r\n", "SELECT 1\nFROM t\n")),
            [" SELECT 1", " FROM t"]
        );
        assert_eq!(
            signs(&diff("\tSELECT 1", "    SELECT 1")),
            ["−\tSELECT 1", "+    SELECT 1"]
        );
        // Beyond the table's size, the old lines go and the new ones come.
        let many: String = (0..2001).map(|n| format!("{n}\n")).collect();
        let lines = diff(&many, &many);
        assert_eq!(lines.len(), 4002);
        assert_eq!(lines[0].change, Change::Removed);
    }

    #[test]
    fn the_replacement_fits_the_place_it_goes() {
        // A statement whose `;` stays in the text: the answer's own is left out.
        let text = "SELECT totl FROM t;\nSELECT 2;";
        assert_eq!(
            replacement(text, 0..18, "SELECT total FROM t;"),
            "SELECT total FROM t"
        );
        // A selection that holds its `;`: the answer's stays.
        assert_eq!(replacement(text, 0..19, "SELECT 1;"), "SELECT 1;");
        // CRLF in the buffer: CRLF in the replacement.
        let crlf = "SELECT a\r\nFROM b;";
        assert_eq!(
            replacement(crlf, 0..16, "SELECT a, c\nFROM b;"),
            "SELECT a, c\r\nFROM b"
        );
        // At a caret on an empty line with text after it: on its own line, ended by `;`.
        let gap = "SELECT 1;\n\nSELECT 2;";
        assert_eq!(replacement(gap, 10..10, "SELECT 3"), "SELECT 3;");
        // At the end of a line of text: after a line break.
        assert_eq!(replacement("SELECT 1;", 9..9, "SELECT 2;"), "\nSELECT 2;");
        // At the end of the text, nothing after: as the answer wrote it.
        assert_eq!(replacement("", 0..0, "SELECT 4"), "SELECT 4");
    }

    /// An edit of the statement, or one before it, while the request waited: the answer is
    /// for another text.
    #[test]
    fn a_statement_changed_under_the_request_is_seen() {
        let text = "SELECT 1;\nSELECT totl FROM t;";
        let range = 10..28;
        let original = &text[range.clone()];
        assert!(still_there(text, &range, original));
        assert!(!still_there(
            "SELECT 1;\nSELECT tot FROM t;",
            &range,
            original
        ));
        assert!(!still_there(
            "-- x\nSELECT 1;\nSELECT totl FROM t;",
            &range,
            original
        ));
        assert!(!still_there("SELECT 1;", &range, original));
    }
}
