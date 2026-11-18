//! What a statement does, read from its text: whether it writes, whether
//! it destroys, whether rows come back.
//!
//! The app refuses writes on read-only connections and asks before
//! destructive statements with this, so every doubt is settled on the
//! careful side: a statement that is not recognised counts as a write.
//! What a function called from a `SELECT` does cannot be seen here; the
//! read-only session opened on the server covers that.

use super::lex::{LexRules, TokenKind, lex};

/// What a statement does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatementClass {
    /// The family of the statement.
    pub kind: StatementKind,
    /// It would change data or schema: blocked on a read-only connection.
    pub writes: bool,
    /// It can lose data at once (`DROP`, `TRUNCATE`, `DELETE` or `UPDATE`
    /// without `WHERE`, `ALTER ... DROP`, a `MERGE` that deletes, or a
    /// `PREPARE` of one of them): asks for confirmation.
    pub destructive: bool,
    /// Best guess: a result grid is expected.
    pub returns_rows: bool,
}

/// The family of a statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatementKind {
    /// `SELECT`, `VALUES`, `TABLE`, and `WITH` ending in one of them.
    Select,
    /// `INSERT` and MySQL's `REPLACE`.
    Insert,
    /// `UPDATE`.
    Update,
    /// `DELETE`.
    Delete,
    /// `MERGE`.
    Merge,
    /// `CREATE`, `ALTER`, `DROP`, `RENAME`, `COMMENT ON`.
    Ddl,
    /// `TRUNCATE`.
    Truncate,
    /// Transaction control.
    Transaction(TxCommand),
    /// `EXPLAIN`, `DESCRIBE`, and MariaDB's `ANALYZE <statement>`.
    Explain,
    /// `SHOW`.
    Show,
    /// `SET` and `RESET`.
    Set,
    /// `CALL`.
    Call,
    /// PostgreSQL's `COPY`.
    Copy,
    /// `USE`.
    Use,
    /// Anything else, an empty statement included.
    Other,
}

/// A transaction control command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxCommand {
    /// `BEGIN`, `START TRANSACTION`.
    Begin,
    /// `COMMIT`, `END`.
    Commit,
    /// `ROLLBACK`, `ABORT`.
    Rollback,
    /// `SAVEPOINT name`.
    Savepoint,
    /// `RELEASE [SAVEPOINT] name`.
    Release,
    /// `ROLLBACK TO [SAVEPOINT] name`.
    RollbackTo,
}

/// A token that carries SQL meaning, and how many parentheses enclose it.
#[derive(Clone, Copy)]
struct Tok<'a> {
    kind: TokenKind,
    text: &'a str,
    depth: usize,
    /// The versioned comment it was read from, numbered from 1, or 0: a
    /// MySQL server may skip such a comment (see [`collect`]).
    comment: u32,
}

impl Tok<'_> {
    /// Whether this is the bare word `word`, in any case.
    fn is(&self, word: &str) -> bool {
        self.kind == TokenKind::Word && self.text.eq_ignore_ascii_case(word)
    }

    fn is_any(&self, words: &[&str]) -> bool {
        words.iter().any(|word| self.is(word))
    }
}

/// The statements a plan can be asked for, and the ones a `WITH` ends in.
const QUERY_WORDS: &[&str] = &[
    "SELECT", "INSERT", "UPDATE", "DELETE", "REPLACE", "MERGE", "WITH", "VALUES", "TABLE",
];

/// Says what `statement` does, from its text alone.
///
/// `statement` is one statement, as [`super::split_statements`] cuts them.
/// If it holds several anyway, it writes or destroys as soon as one of
/// them does.
///
/// A versioned comment (`/*!80000 … */`, `/*M! … */`) runs on some servers
/// and not on others, so the statement is read both ways: with every such
/// comment and without any, and it writes or destroys as soon as one of
/// the two readings does (C21). What it is (`kind`, `returns_rows`) is the
/// reading with the comments.
pub fn classify(statement: &str, rules: LexRules) -> StatementClass {
    let tokens = nested_tokens(statement, rules);
    let mut class = classify_all(&tokens, rules);
    if let Some(skipped) = without_versioned(&tokens) {
        let more = classify_all(&skipped, rules);
        class.writes |= more.writes;
        class.destructive |= more.destructive;
    }
    class
}

/// [`classify`] on one reading of the tokens.
fn classify_all(tokens: &[Tok], rules: LexRules) -> StatementClass {
    let mut parts = tokens
        .split(|token| token.kind == TokenKind::Semicolon)
        .filter(|part| !part.is_empty());
    let Some(first) = parts.next() else {
        return other(false);
    };
    let mut class = classify_tokens(first, rules);
    // The body of a routine, trigger or event is stored, not run.
    if !first[0].is("CREATE") {
        for part in parts {
            let more = classify_tokens(part, rules);
            class.writes |= more.writes;
            class.destructive |= more.destructive;
        }
    }
    class
}

/// The meaningful tokens of `statement`, each with the number of
/// parentheses around it (a parenthesis counts as outside itself).
fn nested_tokens(statement: &str, rules: LexRules) -> Vec<Tok<'_>> {
    let mut tokens = Vec::new();
    collect(statement, rules, 0, &mut 0, &mut tokens);
    set_depths(&mut tokens);
    tokens
}

/// The tokens as a server that skips every versioned comment reads them,
/// or `None` when there is no such comment.
fn without_versioned<'a>(tokens: &[Tok<'a>]) -> Option<Vec<Tok<'a>>> {
    if tokens.iter().all(|token| token.comment == 0) {
        return None;
    }
    let mut kept: Vec<Tok> = tokens
        .iter()
        .filter(|token| token.comment == 0)
        .copied()
        .collect();
    set_depths(&mut kept);
    Some(kept)
}

fn set_depths(tokens: &mut [Tok]) {
    let mut depth = 0usize;
    for token in tokens {
        if token.kind == TokenKind::Punct && token.text == ")" {
            depth = depth.saturating_sub(1);
        }
        token.depth = depth;
        if token.kind == TokenKind::Punct && token.text == "(" {
            depth += 1;
        }
    }
}

/// What [`single_table_select`] says of a statement that is no `SELECT`
/// at all, or several statements.
pub const NOT_A_SELECT: &str = "is not a single SELECT";

/// Functions that return several rows for one: in the select list they
/// repeat the row of the table they are called on (PostgreSQL).
const SET_RETURNING: &[&str] = &[
    "UNNEST",
    "GENERATE_SERIES",
    "GENERATE_SUBSCRIPTS",
    "JSON_ARRAY_ELEMENTS",
    "JSON_ARRAY_ELEMENTS_TEXT",
    "JSONB_ARRAY_ELEMENTS",
    "JSONB_ARRAY_ELEMENTS_TEXT",
    "JSON_EACH",
    "JSON_EACH_TEXT",
    "JSONB_EACH",
    "JSONB_EACH_TEXT",
    "JSON_OBJECT_KEYS",
    "JSONB_OBJECT_KEYS",
    "JSONB_PATH_QUERY",
    "REGEXP_MATCHES",
    "REGEXP_SPLIT_TO_TABLE",
    "STRING_TO_TABLE",
];

/// Why the rows of `statement` may not be the rows of one table as they
/// are stored, each shown once, read from its text: [`NOT_A_SELECT`], or
/// what the statement does that prevents it ("joins tables", "uses
/// DISTINCT"…), to follow "the statement". `None` for a plain `SELECT`
/// from one table, whatever its aliases, `WHERE`, `ORDER BY` or `LIMIT`.
///
/// The rules are DBeaver's single-source query (`SQLQuery`,
/// `isPotentiallySingleSourceSelect`: no join, no grouping, no `INTO`, a
/// table as the `FROM` item), read on tokens instead of a parse tree, plus
/// what repeats or merges rows: `DISTINCT`, set operations, `WITH`, and a
/// set-returning function in the select list. The columns a result shows
/// are told by the server ([`crate::result::ColumnOrigin`]); this covers
/// what origins cannot see, such as a table joined to itself.
pub fn single_table_select(statement: &str, rules: LexRules) -> Option<&'static str> {
    let tokens = nested_tokens(statement, rules);
    // Either reading of a versioned comment may be the one that runs.
    single_table_tokens(&tokens, rules).or_else(|| {
        without_versioned(&tokens).and_then(|skipped| single_table_tokens(&skipped, rules))
    })
}

/// [`single_table_select`] on one reading of the tokens.
fn single_table_tokens(tokens: &[Tok], rules: LexRules) -> Option<&'static str> {
    let mut parts = tokens
        .split(|token| token.kind == TokenKind::Semicolon)
        .filter(|part| !part.is_empty());
    let (Some(tokens), None) = (parts.next(), parts.next()) else {
        return Some(NOT_A_SELECT);
    };
    let class = classify_tokens(tokens, rules);
    if class.kind != StatementKind::Select || class.writes {
        return Some(NOT_A_SELECT);
    }
    let first = &tokens[0];
    if first.is("WITH") {
        return Some("uses WITH");
    }
    if first.is("TABLE") {
        return None;
    }
    if !first.is("SELECT") {
        // `VALUES …`, or `(SELECT …) UNION (SELECT …)`.
        return Some(NOT_A_SELECT);
    }
    let top = |token: &Tok| token.depth == first.depth;
    // Where the FROM item ends: a clause, or what the engine adds to a
    // table (sampling, index hints, partitions) and opens a parenthesis on.
    const AFTER_FROM: &[&str] = &[
        "WHERE",
        "GROUP",
        "HAVING",
        "WINDOW",
        "ORDER",
        "LIMIT",
        "OFFSET",
        "FETCH",
        "FOR",
        "LOCK",
        "TABLESAMPLE",
        "USE",
        "FORCE",
        "IGNORE",
        "PARTITION",
    ];
    let mut in_select = true;
    let mut in_from = false;
    for (index, token) in tokens.iter().enumerate().skip(1) {
        let next_opens = tokens
            .get(index + 1)
            .is_some_and(|next| is_punct(next, "("));
        if in_select && token.is_any(SET_RETURNING) && next_opens {
            return Some("repeats rows with a set-returning function");
        }
        if !top(token) {
            continue;
        }
        if token.is("DISTINCT") {
            return Some("uses DISTINCT");
        }
        if token.is_any(&["GROUP", "HAVING"]) {
            return Some("groups rows");
        }
        if token.is_any(&["UNION", "INTERSECT", "EXCEPT", "MINUS"]) {
            return Some("combines several results");
        }
        if token.is_any(&["JOIN", "STRAIGHT_JOIN"]) {
            return Some("joins tables");
        }
        if token.is("FROM") {
            (in_select, in_from) = (false, true);
        } else if in_from && token.is_any(AFTER_FROM) {
            in_from = false;
        } else if in_from && is_punct(token, ",") {
            return Some("joins tables");
        } else if in_from && is_punct(token, "(") {
            return Some("reads from a subquery or a function");
        }
    }
    None
}

/// Appends the meaningful tokens of `source` to `out`, each marked with
/// `comment`. MySQL runs what is inside `/*! ... */` comments (dumps are
/// full of them), so there the content of such a comment counts as SQL.
/// `/*!NNNNN ... */` runs only from server version NNNNN, and MariaDB's
/// `/*M! ... */` only on MariaDB (MySQL skips it), so their tokens are
/// marked with a number of their own (`last` counts them): a keyword there
/// may make a statement more dangerous, never less (C21, D-079). On
/// PostgreSQL rules they are plain comments.
fn collect<'a>(
    source: &'a str,
    rules: LexRules,
    comment: u32,
    last: &mut u32,
    out: &mut Vec<Tok<'a>>,
) {
    for token in lex(source, rules) {
        let text = &source[token.range];
        match token.kind {
            TokenKind::BlockComment if rules.hash_comments => {
                let (code, mariadb) = match text.strip_prefix("/*M!") {
                    Some(code) => (Some(code), true),
                    None => (text.strip_prefix("/*!"), false),
                };
                if let Some(code) = code {
                    let code = code.strip_suffix("*/").unwrap_or(code);
                    // An optional version number comes first.
                    let body = code.trim_start_matches(|c: char| c.is_ascii_digit());
                    let versioned = mariadb || body.len() < code.len();
                    let inner = if versioned {
                        *last += 1;
                        *last
                    } else {
                        comment
                    };
                    collect(body, rules, inner, last, out);
                }
            }
            kind if kind.is_trivia() => {}
            kind => out.push(Tok {
                kind,
                text,
                depth: 0,
                comment,
            }),
        }
    }
}

fn class(
    kind: StatementKind,
    writes: bool,
    destructive: bool,
    returns_rows: bool,
) -> StatementClass {
    StatementClass {
        kind,
        writes,
        destructive,
        returns_rows,
    }
}

fn other(writes: bool) -> StatementClass {
    class(StatementKind::Other, writes, false, false)
}

fn is_punct(token: &Tok, text: &str) -> bool {
    token.kind == TokenKind::Punct && token.text == text
}

/// Classifies one statement. `tokens` is not empty.
fn classify_tokens(tokens: &[Tok], rules: LexRules) -> StatementClass {
    use StatementKind::*;
    // `(SELECT ...) UNION (SELECT ...)`: the statement is what the
    // parentheses open on.
    let start = tokens
        .iter()
        .take_while(|token| is_punct(token, "("))
        .count();
    let tokens = &tokens[start..];
    let Some(first) = tokens.first().filter(|token| token.kind == TokenKind::Word) else {
        return other(true);
    };
    let base = first.depth;
    let top = |word: &str| {
        tokens
            .iter()
            .any(|token| token.depth == base && token.is(word))
    };
    let tx = |command| class(Transaction(command), false, false, false);

    match first.text.to_ascii_uppercase().as_str() {
        "SELECT" | "VALUES" | "TABLE" => {
            // `SELECT ... INTO` creates a table on PostgreSQL, and writes a
            // file or a variable on MySQL.
            let into = top("INTO");
            class(Select, into, false, !into)
        }
        "WITH" => with(tokens),
        "INSERT" | "REPLACE" => class(Insert, true, false, top("RETURNING")),
        "UPDATE" => class(Update, true, !has_where(tokens, 0), top("RETURNING")),
        "DELETE" => class(Delete, true, !has_where(tokens, 0), top("RETURNING")),
        // `WHEN [NOT] MATCHED … THEN DELETE` deletes with no WHERE to
        // review (C21); `delete` is reserved, so the pair is the action.
        "MERGE" => {
            let deletes = tokens
                .windows(2)
                .any(|pair| pair[0].is("THEN") && pair[1].is("DELETE") && pair[1].depth == base);
            class(Merge, true, deletes, top("RETURNING"))
        }
        "TRUNCATE" => class(Truncate, true, true, false),
        "DROP" => class(Ddl, true, true, false),
        "ALTER" => {
            // Dropping a default or a NOT NULL loses no data, when both
            // words run together (not one in a versioned comment).
            let destroys = tokens.iter().enumerate().any(|(index, token)| {
                token.is_any(&["TRUNCATE", "DISCARD"])
                    || token.is("DROP")
                        && !tokens.get(index + 1).is_some_and(|next| {
                            next.comment == token.comment && next.is_any(&["NOT", "DEFAULT"])
                        })
            });
            class(Ddl, true, destroys, false)
        }
        "CREATE" | "RENAME" | "COMMENT" => class(Ddl, true, false, false),
        "BEGIN" => tx(TxCommand::Begin),
        "START" if tokens.get(1).is_some_and(|token| token.is("TRANSACTION")) => {
            tx(TxCommand::Begin)
        }
        "COMMIT" | "END" => tx(TxCommand::Commit),
        "ROLLBACK" | "ABORT" if top("TO") => tx(TxCommand::RollbackTo),
        "ROLLBACK" | "ABORT" => tx(TxCommand::Rollback),
        "SAVEPOINT" => tx(TxCommand::Savepoint),
        "RELEASE" => tx(TxCommand::Release),
        "EXPLAIN" | "DESCRIBE" | "DESC" => explain(&tokens[1..], false, rules),
        "ANALYZE" | "ANALYSE" => {
            // MariaDB: `ANALYZE [FORMAT=JSON] <statement>` runs the
            // statement. Otherwise it gathers statistics on tables.
            let rest = skip_format(&tokens[1..]);
            let statement = rest
                .first()
                .is_some_and(|token| token.is_any(QUERY_WORDS) && !token.is("TABLE"));
            if statement {
                explain(rest, true, rules)
            } else {
                // MySQL answers with a status table.
                class(Other, true, false, rules.hash_comments)
            }
        }
        "SHOW" => class(Show, false, false, true),
        "SET" | "RESET" => {
            // Server-wide settings and passwords outlive the session.
            let scope = tokens[1..]
                .iter()
                .find(|token| token.kind == TokenKind::Word);
            let writes = scope.is_some_and(|token| {
                token.is_any(&["GLOBAL", "PERSIST", "PERSIST_ONLY", "PASSWORD"])
            });
            class(Set, writes, false, false)
        }
        "USE" => class(Use, false, false, false),
        "CALL" => class(Call, true, false, false),
        "COPY" => {
            // `COPY (query) TO` runs the query, which may be a DELETE.
            let inner = tokens.get(1).filter(|token| is_punct(token, "(")).map(|_| {
                let close = tokens
                    .iter()
                    .position(|token| token.depth == base && is_punct(token, ")"))
                    .unwrap_or(tokens.len());
                &tokens[2..close.max(2)]
            });
            let inner = match inner {
                Some(query) if !query.is_empty() => classify_tokens(query, rules),
                _ => other(false),
            };
            let writes = inner.writes || top("FROM") || top("PROGRAM") || !top("TO");
            class(Copy, writes, inner.destructive, false)
        }
        // PostgreSQL: `PREPARE name [(types)] AS statement` does what its
        // statement does, later, at `EXECUTE` (C21): the question is asked
        // here. MySQL's `PREPARE name FROM '…'` (or `FROM @variable`) and
        // `EXECUTE` hide the statement from a text classifier: they stay
        // writes (read-only and production guard them), never known to
        // destroy. So does `PREPARE TRANSACTION`.
        "PREPARE" => {
            let statement = tokens
                .iter()
                .position(|token| token.depth == base && token.is("AS"))
                .map(|index| &tokens[index + 1..])
                .filter(|statement| !statement.is_empty());
            match statement {
                Some(statement) => {
                    let inner = classify_tokens(statement, rules);
                    class(Other, inner.writes, inner.destructive, false)
                }
                None => other(true),
            }
        }
        "FETCH" | "HELP" => class(Other, false, false, true),
        "MOVE" | "CLOSE" | "DECLARE" | "DEALLOCATE" | "LISTEN" | "UNLISTEN" | "DISCARD"
        | "UNLOCK" => other(false),
        // MySQL table maintenance answers with a status table.
        "OPTIMIZE" | "REPAIR" | "CHECK" | "CHECKSUM" => class(Other, true, false, true),
        // DO, EXECUTE, LOAD, GRANT, VACUUM ... and whatever is
        // not known: it may write.
        _ => other(true),
    }
}

/// Whether the `UPDATE` or `DELETE` at `index` has a `WHERE` of its own:
/// one inside a subquery or another CTE does not count, nor one in a
/// versioned comment the statement is not in (it may be skipped, C21).
fn has_where(tokens: &[Tok], index: usize) -> bool {
    let Tok { depth, comment, .. } = tokens[index];
    tokens[index + 1..]
        .iter()
        .take_while(|token| token.depth >= depth)
        .any(|token| token.depth == depth && token.comment == comment && token.is("WHERE"))
}

/// `WITH ...`: what the statement the CTEs lead to does, plus what any
/// data-modifying CTE does (PostgreSQL).
fn with(tokens: &[Tok]) -> StatementClass {
    use StatementKind::*;
    let base = tokens[0].depth;
    let top = |word: &str| {
        tokens
            .iter()
            .any(|token| token.depth == base && token.is(word))
    };
    let main = tokens[1..]
        .iter()
        .find(|token| token.depth == base && token.is_any(QUERY_WORDS) && !token.is("WITH"));
    let kind = match main {
        Some(token) if token.is_any(&["SELECT", "VALUES", "TABLE"]) => Select,
        Some(token) if token.is_any(&["INSERT", "REPLACE"]) => Insert,
        Some(token) if token.is("UPDATE") => Update,
        Some(token) if token.is("DELETE") => Delete,
        Some(token) if token.is("MERGE") => Merge,
        _ => return other(true),
    };

    let mut writes = false;
    let mut destructive = false;
    for (index, token) in tokens.iter().enumerate() {
        let after = |words: &[&str]| {
            index > 0
                && tokens[index - 1].comment == token.comment
                && tokens[index - 1].is_any(words)
        };
        // `insert(...)` and `replace(...)` are functions.
        if tokens
            .get(index + 1)
            .is_some_and(|next| is_punct(next, "("))
        {
            continue;
        }
        // Not statements: FOR [NO KEY] UPDATE, ON CONFLICT DO UPDATE,
        // ON DUPLICATE KEY UPDATE, WHEN MATCHED THEN UPDATE / DELETE.
        let modifies = token.is("UPDATE") && !after(&["FOR", "KEY", "DO", "THEN"])
            || token.is("DELETE") && !after(&["THEN"]);
        writes |= modifies || token.is_any(&["INSERT", "MERGE"]);
        // A MERGE's `THEN DELETE` deletes with no WHERE to review (C21).
        destructive |=
            modifies && !has_where(tokens, index) || token.is("DELETE") && after(&["THEN"]);
    }

    if kind == Select {
        let into = top("INTO");
        class(Select, writes || into, destructive, !into)
    } else {
        class(kind, true, destructive, top("RETURNING"))
    }
}

/// Skips a leading `FORMAT [=] <name>`.
fn skip_format<'t, 'a>(tokens: &'t [Tok<'a>]) -> &'t [Tok<'a>] {
    if !tokens.first().is_some_and(|token| token.is("FORMAT")) {
        return tokens;
    }
    let equals = usize::from(tokens.get(1).is_some_and(|token| is_punct(token, "=")));
    &tokens[(2 + equals).min(tokens.len())..]
}

/// What follows `EXPLAIN`: options, then the statement. The statement runs
/// only when `ANALYZE` is among the options.
fn explain(mut rest: &[Tok], mut analyze: bool, rules: LexRules) -> StatementClass {
    const ANALYZE: &[&str] = &["ANALYZE", "ANALYSE"];
    // PostgreSQL: `EXPLAIN (ANALYZE, FORMAT JSON) ...`. A parenthesis that
    // opens on a statement is the statement itself.
    let options = rest.first().filter(|open| {
        is_punct(open, "(") && !rest.get(1).is_some_and(|token| token.is_any(QUERY_WORDS))
    });
    if let Some(open) = options {
        let close = rest
            .iter()
            .position(|token| token.depth == open.depth && is_punct(token, ")"))
            .unwrap_or(rest.len());
        let options = &rest[1..close];
        analyze |= options.iter().enumerate().any(|(index, option)| {
            let off = options
                .get(index + 1)
                .is_some_and(|value| value.is_any(&["FALSE", "OFF"]) || value.text == "0");
            option.is_any(ANALYZE) && !off
        });
        rest = &rest[(close + 1).min(rest.len())..];
    } else {
        while let Some(option) = rest.first() {
            if option.is_any(ANALYZE) {
                analyze = true;
                rest = &rest[1..];
            } else if option.is_any(&["VERBOSE", "EXTENDED", "PARTITIONS"]) {
                rest = &rest[1..];
            } else if option.is("FORMAT") {
                rest = skip_format(rest);
            } else {
                break;
            }
        }
    }
    // `EXPLAIN <table>` (MySQL) describes a table: nothing runs.
    let inner = if rest.is_empty() {
        other(true)
    } else {
        classify_tokens(rest, rules)
    };
    class(
        StatementKind::Explain,
        analyze && inner.writes,
        analyze && inner.destructive,
        true,
    )
}

#[cfg(test)]
mod tests {
    use super::StatementKind::*;
    use super::*;

    /// Statement, kind, writes, destructive, returns rows.
    type Case = (&'static str, StatementKind, bool, bool, bool);

    const T: bool = true;
    const F: bool = false;

    /// Statements that read the same on every engine.
    const COMMON: &[Case] = &[
        // Reads.
        ("select 1", Select, F, F, T),
        ("  \n-- first\n/* then */ SELECT * FROM t", Select, F, F, T),
        ("SELECT 'drop table x'", Select, F, F, T),
        (
            "SELECT * FROM t WHERE note = 'delete from t'",
            Select,
            F,
            F,
            T,
        ),
        ("select * from t -- delete from t", Select, F, F, T),
        ("select * from t /* ; drop table t */", Select, F, F, T),
        ("SELECT * FROM t FOR UPDATE", Select, F, F, T),
        ("(SELECT 1) UNION (SELECT 2)", Select, F, F, T),
        ("VALUES (1), (2)", Select, F, F, T),
        ("TABLE t", Select, F, F, T),
        ("SELECT \"delete\" FROM t", Select, F, F, T),
        // Common table expressions, both ways.
        ("WITH x AS (SELECT 1) SELECT * FROM x", Select, F, F, T),
        (
            "with recursive x as (select 1) select * from x",
            Select,
            F,
            F,
            T,
        ),
        (
            "WITH x AS (SELECT * FROM t FOR UPDATE) SELECT * FROM x",
            Select,
            F,
            F,
            T,
        ),
        (
            "WITH x AS (SELECT 1) INSERT INTO t SELECT * FROM x",
            Insert,
            T,
            F,
            F,
        ),
        (
            "WITH x AS (SELECT id FROM t WHERE a) DELETE FROM u",
            Delete,
            T,
            T,
            F,
        ),
        (
            "WITH x AS (SELECT 1) UPDATE t SET a = 1 WHERE id IN (SELECT * FROM x)",
            Update,
            T,
            F,
            F,
        ),
        ("WITH x AS (SELECT 1) UPDATE t SET a = 1", Update, T, T, F),
        ("(((SELECT 1)))", Select, F, F, T),
        (
            "WITH x AS (SELECT replace(a, 'x', 'y') FROM t) SELECT * FROM x",
            Select,
            F,
            F,
            T,
        ),
        (
            "WITH x AS (SELECT 1) SELECT * FROM t FOR NO KEY UPDATE",
            Select,
            F,
            F,
            T,
        ),
        (
            "WITH x AS (SELECT 1) DELETE FROM t WHERE id IN (SELECT * FROM x)",
            Delete,
            T,
            F,
            F,
        ),
        // C21: a MERGE that deletes can delete without a WHERE to review.
        (
            "WITH s AS (SELECT 1 AS id) MERGE INTO t USING s ON t.id = s.id \
             WHEN MATCHED THEN DELETE",
            Merge,
            T,
            T,
            F,
        ),
        // INSERT.
        ("INSERT INTO t (a) VALUES (1)", Insert, T, F, F),
        ("INSERT INTO t SELECT * FROM u WHERE x", Insert, T, F, F),
        ("insert into t values (1) returning id", Insert, T, F, T),
        // UPDATE: destructive without a WHERE of its own.
        ("UPDATE t SET a = 1", Update, T, T, F),
        ("UPDATE t SET a = 1 WHERE id = 2", Update, T, F, F),
        (
            "update t set a = 1 where id = 2 returning *",
            Update,
            T,
            F,
            T,
        ),
        (
            "UPDATE t SET a = (SELECT max(b) FROM u WHERE u.id = 1)",
            Update,
            T,
            T,
            F,
        ),
        ("update t set a = 'where'", Update, T, T, F),
        ("UPDATE t SET a = 1 -- WHERE id = 2", Update, T, T, F),
        ("UPDATE t SET a = 1 /* WHERE id = 2 */", Update, T, T, F),
        // DELETE.
        ("DELETE FROM t", Delete, T, T, F),
        ("DELETE FROM t WHERE id = 1", Delete, T, F, F),
        (
            "DELETE FROM t WHERE id IN (SELECT id FROM u)",
            Delete,
            T,
            F,
            F,
        ),
        (
            "DELETE FROM t USING (SELECT id FROM u WHERE x) s",
            Delete,
            T,
            T,
            F,
        ),
        ("DELETE FROM t WHERE id = 1 RETURNING *", Delete, T, F, T),
        (
            "\n\n  -- cleanup\n  DELETE\n  FROM t\n  -- WHERE id = 1\n",
            Delete,
            T,
            T,
            F,
        ),
        (
            "MERGE INTO t USING s ON t.id = s.id WHEN MATCHED THEN UPDATE SET a = s.a \
             WHEN NOT MATCHED THEN INSERT VALUES (s.id, s.a)",
            Merge,
            T,
            F,
            F,
        ),
        // An explicit WHERE is the user's decision, whatever it says (D-079).
        ("DELETE FROM t WHERE true", Delete, T, F, F),
        ("UPDATE t SET a = 1 WHERE 1=1", Update, T, F, F),
        // TRUNCATE and DDL.
        ("TRUNCATE TABLE t", Truncate, T, T, F),
        ("truncate t", Truncate, T, T, F),
        ("DROP TABLE t", Ddl, T, T, F),
        ("drop index if exists i", Ddl, T, T, F),
        ("ALTER TABLE t ADD COLUMN c int", Ddl, T, F, F),
        ("ALTER TABLE t DROP COLUMN c", Ddl, T, T, F),
        ("ALTER TABLE t ADD c int, DROP CONSTRAINT k", Ddl, T, T, F),
        ("ALTER TABLE t ALTER COLUMN c DROP NOT NULL", Ddl, T, F, F),
        ("ALTER TABLE t ALTER COLUMN c DROP DEFAULT", Ddl, T, F, F),
        ("ALTER TABLE t RENAME TO \"drop\"", Ddl, T, F, F),
        ("ALTER TABLE t TRUNCATE PARTITION p1", Ddl, T, T, F),
        ("CREATE TABLE t (id int)", Ddl, T, F, F),
        ("CREATE TABLE t AS SELECT * FROM u", Ddl, T, F, F),
        ("CREATE INDEX i ON t (a)", Ddl, T, F, F),
        (
            "CREATE TABLE c (p int REFERENCES p (id) ON DELETE CASCADE)",
            Ddl,
            T,
            F,
            F,
        ),
        ("COMMENT ON TABLE t IS 'drop it'", Ddl, T, F, F),
        ("GRANT SELECT ON t TO u", Other, T, F, F),
        ("REVOKE ALL ON t FROM u", Other, T, F, F),
        // Transactions.
        ("BEGIN", Transaction(TxCommand::Begin), F, F, F),
        ("START TRANSACTION", Transaction(TxCommand::Begin), F, F, F),
        (
            "start transaction read only",
            Transaction(TxCommand::Begin),
            F,
            F,
            F,
        ),
        ("COMMIT", Transaction(TxCommand::Commit), F, F, F),
        ("ROLLBACK", Transaction(TxCommand::Rollback), F, F, F),
        (
            "ROLLBACK TO SAVEPOINT s",
            Transaction(TxCommand::RollbackTo),
            F,
            F,
            F,
        ),
        (
            "rollback work to s",
            Transaction(TxCommand::RollbackTo),
            F,
            F,
            F,
        ),
        ("SAVEPOINT s", Transaction(TxCommand::Savepoint), F, F, F),
        (
            "RELEASE SAVEPOINT s",
            Transaction(TxCommand::Release),
            F,
            F,
            F,
        ),
        // Session and plans.
        ("SET search_path = shop", Set, F, F, F),
        ("SHOW TABLES", Show, F, F, T),
        ("USE kerno", Use, F, F, F),
        ("CALL do_things(1)", Call, T, F, F),
        ("EXPLAIN SELECT * FROM t", Explain, F, F, T),
        ("EXPLAIN DELETE FROM t", Explain, F, F, T),
        ("EXPLAIN ANALYZE SELECT * FROM t", Explain, F, F, T),
        ("EXPLAIN ANALYZE DELETE FROM t", Explain, T, T, T),
        (
            "explain analyze update t set a = 1 where id = 1",
            Explain,
            T,
            F,
            T,
        ),
        ("EXPLAIN ANALYZE INSERT INTO t VALUES (1)", Explain, T, F, T),
        ("EXPLAIN (SELECT 1) UNION (SELECT 2)", Explain, F, F, T),
        ("EXPLAIN ANALYZE frobnicate", Explain, T, F, T),
        // Unknown side effects count as writes.
        ("LOAD DATA INFILE 'f' INTO TABLE t", Other, T, F, F),
        ("LOCK TABLE t", Other, T, F, F),
        ("PREPARE p FROM 'select 1'", Other, T, F, F),
        ("EXECUTE p", Other, T, F, F),
        ("frobnicate the database", Other, T, F, F),
        // Nothing to run.
        ("", Other, F, F, F),
        ("   \n\t", Other, F, F, F),
        ("-- nothing\n/* at all */", Other, F, F, F),
        (";", Other, F, F, F),
        ("(", Other, T, F, F),
        ("42", Other, T, F, F),
        // Several statements: one that writes or destroys is enough.
        ("SELECT 1; DROP TABLE t", Select, T, T, T),
        (
            "BEGIN; DELETE FROM t; COMMIT",
            Transaction(TxCommand::Begin),
            T,
            T,
            F,
        ),
        ("SELECT 1;", Select, F, F, T),
    ];

    const POSTGRES: &[Case] = &[
        ("SELECT * INTO new_t FROM t", Select, T, F, F),
        (
            "WITH d AS (DELETE FROM t WHERE id = 1 RETURNING *) SELECT * FROM d",
            Select,
            T,
            F,
            T,
        ),
        (
            "WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d",
            Select,
            T,
            T,
            T,
        ),
        (
            "WITH u AS (UPDATE t SET a = 1 RETURNING *) INSERT INTO log SELECT * FROM u",
            Insert,
            T,
            T,
            F,
        ),
        (
            "WITH i AS (INSERT INTO t VALUES (1) RETURNING id) SELECT * FROM i",
            Select,
            T,
            F,
            T,
        ),
        (
            "WITH i AS (INSERT INTO t VALUES (1) ON CONFLICT (id) DO UPDATE SET a = 1 RETURNING id) \
             SELECT * FROM i",
            Select,
            T,
            F,
            T,
        ),
        (
            "INSERT INTO t VALUES (1) ON CONFLICT (id) DO UPDATE SET a = 1",
            Insert,
            T,
            F,
            F,
        ),
        (
            "UPDATE t SET a = u.a FROM u WHERE t.id = u.id",
            Update,
            T,
            F,
            F,
        ),
        (
            "UPDATE t SET a = 1 FROM (SELECT id FROM u WHERE x) s",
            Update,
            T,
            T,
            F,
        ),
        (
            "EXPLAIN (ANALYZE, BUFFERS) DELETE FROM t WHERE id = 1",
            Explain,
            T,
            F,
            T,
        ),
        ("EXPLAIN (ANALYZE false) DELETE FROM t", Explain, F, F, T),
        ("EXPLAIN (FORMAT JSON) DELETE FROM t", Explain, F, F, T),
        ("EXPLAIN (ANALYZE, FORMAT JSON) SELECT 1", Explain, F, F, T),
        (
            "EXPLAIN ANALYZE WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d",
            Explain,
            T,
            T,
            T,
        ),
        (
            "WITH x AS (SELECT 1) SELECT * INTO t FROM x",
            Select,
            T,
            F,
            F,
        ),
        (
            "explain (analyze off, costs off) delete from t",
            Explain,
            F,
            F,
            T,
        ),
        ("EXPLAIN (ANALYZE true) DELETE FROM t", Explain, T, T, T),
        ("COPY (SELECT 1) TO PROGRAM 'cat'", Copy, T, F, F),
        ("ANALYZE VERBOSE t", Other, T, F, F),
        ("DO $$ BEGIN DELETE FROM t; END $$", Other, T, F, F),
        ("SELECT $$ drop table t $$", Select, F, F, T),
        ("SELECT $tag$ ; delete from t; $tag$", Select, F, F, T),
        (
            "SELECT * FROM t WHERE a = E'it\\'s; drop table t'",
            Select,
            F,
            F,
            T,
        ),
        (
            "SELECT /* outer /* nested */ drop table t */ 1",
            Select,
            F,
            F,
            T,
        ),
        (
            "CREATE FUNCTION f() RETURNS void AS $$ DELETE FROM t $$ LANGUAGE sql",
            Ddl,
            T,
            F,
            F,
        ),
        ("COPY t FROM STDIN", Copy, T, F, F),
        ("COPY t TO STDOUT", Copy, F, F, F),
        ("COPY (SELECT * FROM t) TO STDOUT", Copy, F, F, F),
        ("COPY (DELETE FROM t RETURNING *) TO STDOUT", Copy, T, T, F),
        ("COPY t TO PROGRAM 'gzip > f'", Copy, T, F, F),
        ("END", Transaction(TxCommand::Commit), F, F, F),
        ("ABORT", Transaction(TxCommand::Rollback), F, F, F),
        (
            "BEGIN ISOLATION LEVEL SERIALIZABLE",
            Transaction(TxCommand::Begin),
            F,
            F,
            F,
        ),
        ("ANALYZE shop.orders", Other, T, F, F),
        ("ANALYZE", Other, T, F, F),
        ("VACUUM (FULL) t", Other, T, F, F),
        ("REINDEX TABLE t", Other, T, F, F),
        ("REFRESH MATERIALIZED VIEW v", Other, T, F, F),
        ("RESET search_path", Set, F, F, F),
        ("SHOW search_path", Show, F, F, T),
        ("FETCH 10 FROM c", Other, F, F, T),
        // C21: a prepared statement is the statement it prepares, run later.
        ("PREPARE p AS DELETE FROM t", Other, T, T, F),
        ("PREPARE p AS DELETE FROM t WHERE id = $1", Other, T, F, F),
        ("PREPARE p (int) AS UPDATE t SET a = $1", Other, T, T, F),
        (
            "prepare p (int, text) as update t set a = $1 where b = $2",
            Other,
            T,
            F,
            F,
        ),
        ("PREPARE p AS SELECT * FROM t", Other, F, F, F),
        ("PREPARE p AS VALUES (1)", Other, F, F, F),
        (
            "PREPARE p AS INSERT INTO t VALUES (1) RETURNING id",
            Other,
            T,
            F,
            F,
        ),
        (
            "PREPARE p AS WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d",
            Other,
            T,
            T,
            F,
        ),
        (
            "PREPARE p AS MERGE INTO t USING s ON true WHEN MATCHED THEN DELETE",
            Other,
            T,
            T,
            F,
        ),
        ("PREPARE p", Other, T, F, F),
        ("PREPARE TRANSACTION 'x'", Other, T, F, F),
        ("EXPLAIN ANALYZE EXECUTE p", Explain, T, F, T),
        // C21: MERGE destroys when one of its actions deletes.
        (
            "MERGE INTO t USING s ON true WHEN MATCHED THEN DELETE",
            Merge,
            T,
            T,
            F,
        ),
        (
            "MERGE INTO t USING s ON t.id = s.id WHEN MATCHED AND s.gone THEN DELETE",
            Merge,
            T,
            T,
            F,
        ),
        (
            "MERGE INTO t USING s ON t.id = s.id WHEN NOT MATCHED BY SOURCE THEN DELETE",
            Merge,
            T,
            T,
            F,
        ),
        (
            "MERGE INTO t USING s ON t.id = s.id \
             WHEN MATCHED AND s.a > 0 THEN UPDATE SET a = s.a \
             WHEN MATCHED THEN DELETE \
             WHEN NOT MATCHED THEN INSERT VALUES (s.id, s.a)",
            Merge,
            T,
            T,
            F,
        ),
        (
            "MERGE INTO t USING s ON true WHEN MATCHED THEN DELETE RETURNING *",
            Merge,
            T,
            T,
            T,
        ),
        (
            "MERGE INTO t USING s ON t.id = s.id WHEN MATCHED THEN DO NOTHING \
             WHEN NOT MATCHED THEN DO NOTHING",
            Merge,
            T,
            F,
            F,
        ),
        (
            "MERGE INTO t USING (SELECT id, \"delete\" FROM s WHERE note = 'then delete') s \
             ON t.id = s.id WHEN MATCHED THEN UPDATE SET note = 'THEN DELETE' \
             /* THEN DELETE */ -- THEN DELETE\n",
            Merge,
            T,
            F,
            F,
        ),
        (
            "EXPLAIN ANALYZE MERGE INTO t USING s ON true WHEN MATCHED THEN DELETE",
            Explain,
            T,
            T,
            T,
        ),
        // MySQL's executable comments are plain comments here.
        ("DELETE FROM t /*!90000 WHERE id = 1 */", Delete, T, T, F),
        ("DELETE FROM t /*! WHERE id = 1 */", Delete, T, T, F),
    ];

    const MYSQL: &[Case] = &[
        ("# first\nSELECT 1", Select, F, F, T),
        ("SELECT 'it\\'s; DROP TABLE t'", Select, F, F, T),
        ("SELECT \"drop table t\"", Select, F, F, T),
        ("SELECT `update` FROM `delete`", Select, F, F, T),
        ("SELECT 5--3", Select, F, F, T),
        // Multi-table and ORDER BY ... LIMIT forms.
        (
            "DELETE t1 FROM t1 JOIN t2 ON t1.id = t2.id WHERE t2.x = 1",
            Delete,
            T,
            F,
            F,
        ),
        (
            "DELETE t1, t2 FROM t1 JOIN t2 ON t1.id = t2.id",
            Delete,
            T,
            T,
            F,
        ),
        ("DELETE FROM t ORDER BY id LIMIT 10", Delete, T, T, F),
        (
            "DELETE FROM t WHERE a = 1 ORDER BY id LIMIT 10",
            Delete,
            T,
            F,
            F,
        ),
        ("UPDATE t SET a = 1 ORDER BY id LIMIT 5", Update, T, T, F),
        (
            "UPDATE t1 JOIN t2 ON t1.id = t2.id SET t1.a = t2.a WHERE t2.b > 0",
            Update,
            T,
            F,
            F,
        ),
        ("UPDATE t1, t2 SET t1.a = t2.a", Update, T, T, F),
        ("REPLACE INTO t VALUES (1)", Insert, T, F, F),
        (
            "INSERT INTO t VALUES (1) ON DUPLICATE KEY UPDATE a = 1",
            Insert,
            T,
            F,
            F,
        ),
        // Plans.
        ("EXPLAIN FORMAT=JSON SELECT 1", Explain, F, F, T),
        ("EXPLAIN FORMAT=JSON DELETE FROM t", Explain, F, F, T),
        (
            "EXPLAIN ANALYZE FORMAT=TREE DELETE FROM t WHERE id = 1",
            Explain,
            T,
            F,
            T,
        ),
        ("ANALYZE FORMAT=JSON SELECT * FROM t", Explain, F, F, T),
        ("ANALYZE FORMAT=JSON DELETE FROM t", Explain, T, T, T),
        ("ANALYZE SELECT 1", Explain, F, F, T),
        ("ANALYZE TABLE t", Other, T, F, T),
        ("OPTIMIZE TABLE t", Other, T, F, T),
        ("DESCRIBE t", Explain, F, F, T),
        ("DESC t", Explain, F, F, T),
        ("EXPLAIN orders", Explain, F, F, T),
        ("EXPLAIN FOR CONNECTION 12", Explain, F, F, T),
        ("DESCRIBE SELECT * FROM t", Explain, F, F, T),
        ("SHOW PROCESSLIST", Show, F, F, T),
        // Executable comments run; plain ones do not.
        ("/*!40101 SET NAMES utf8 */", Set, F, F, F),
        ("/*!50001 DROP VIEW IF EXISTS v */", Ddl, T, T, F),
        ("/*M!100100 DROP TABLE t */", Ddl, T, T, F),
        ("/* plain DROP TABLE t */", Other, F, F, F),
        ("SELECT /*! STRAIGHT_JOIN */ 1", Select, F, F, T),
        // `/*! … */` runs on every MySQL and MariaDB server: its WHERE holds.
        ("DELETE FROM t /*! WHERE id = 1 */", Delete, T, F, F),
        // C21: a versioned comment may not run (a newer version than the
        // server's; `/*M!` on MySQL): its WHERE may not hold (D-079).
        ("DELETE FROM t /*!90000 WHERE id = 1 */", Delete, T, T, F),
        (
            "UPDATE t SET a = 1 /*!999999 WHERE id = 1 */",
            Update,
            T,
            T,
            F,
        ),
        ("DELETE FROM t /*!80000 WHERE id = 1 */", Delete, T, T, F),
        ("DELETE FROM t /*M!100100 WHERE id = 1 */", Delete, T, T, F),
        ("DELETE FROM t /*M! WHERE id = 1 */", Delete, T, T, F),
        (
            "DELETE FROM t /*!90000 WHERE id = 1 */ ORDER BY id LIMIT 10",
            Delete,
            T,
            T,
            F,
        ),
        // A WHERE outside the comment is the user's own.
        (
            "DELETE FROM t WHERE /*!90000 id = 1 */ 1=1",
            Delete,
            T,
            F,
            F,
        ),
        (
            "DELETE FROM t WHERE /*!90000 id = 1 AND */ 1 = 1",
            Delete,
            T,
            F,
            F,
        ),
        (
            "DELETE FROM t WHERE id > 0 /*!90000 LIMIT 1 */",
            Delete,
            T,
            F,
            F,
        ),
        // A whole statement in one comment runs whole, or not at all.
        ("/*!50000 DELETE FROM t */", Delete, T, T, F),
        ("/*!50000 DELETE FROM t WHERE id = 1 */", Delete, T, F, F),
        (
            "/*!50000 DELETE FROM t */ /*!90000 WHERE id = 1 */",
            Delete,
            T,
            T,
            F,
        ),
        ("/*!40000 ALTER TABLE t DISABLE KEYS */", Ddl, T, F, F),
        ("/*!90000 TRUNCATE t */", Truncate, T, T, F),
        // What a skipped comment would hide counts too.
        ("ALTER TABLE t DROP /*!90000 DEFAULT */ c", Ddl, T, T, F),
        ("/*!90000 SELECT * FROM */ DELETE FROM t", Select, T, T, T),
        (
            "WITH x AS (SELECT 1) /*!90000 SELECT 1 FOR */ UPDATE t SET a = 1",
            Select,
            T,
            T,
            T,
        ),
        // A statement prepared from text cannot be read: it may write.
        ("PREPARE p FROM 'delete from t'", Other, T, F, F),
        ("EXECUTE p USING @a", Other, T, F, F),
        // Careful guesses.
        ("SELECT a INTO @v FROM t", Select, T, F, F),
        ("SELECT * FROM t INTO OUTFILE '/tmp/x'", Select, T, F, F),
        ("SET NAMES utf8mb4", Set, F, F, F),
        ("SET GLOBAL max_connections = 10", Set, T, F, F),
        ("SET @@GLOBAL.max_connections = 10", Set, T, F, F),
        ("SET PASSWORD = 'x'", Set, T, F, F),
        ("START REPLICA", Other, T, F, F),
        ("LOCK TABLES t WRITE", Other, T, F, F),
        ("UNLOCK TABLES", Other, F, F, F),
        (
            "CREATE PROCEDURE p() BEGIN DELETE FROM t; DROP TABLE u; END",
            Ddl,
            T,
            F,
            F,
        ),
        ("FLUSH PRIVILEGES", Other, T, F, F),
        ("KILL QUERY 12", Other, T, F, F),
        ("XA COMMIT 'x'", Other, T, F, F),
    ];

    fn check(rules: LexRules, cases: &[Case]) -> Vec<String> {
        cases
            .iter()
            .filter_map(|&(sql, kind, writes, destructive, returns_rows)| {
                let expected = class(kind, writes, destructive, returns_rows);
                let actual = classify(sql, rules);
                (actual != expected)
                    .then(|| format!("{sql:?}\n  expected {expected:?}\n  actual   {actual:?}"))
            })
            .collect()
    }

    #[test]
    fn statements_are_classified() {
        let mut failures = check(LexRules::POSTGRES, COMMON);
        failures.extend(check(LexRules::MYSQL, COMMON));
        failures.extend(check(LexRules::POSTGRES, POSTGRES));
        failures.extend(check(LexRules::MYSQL, MYSQL));
        assert!(
            failures.is_empty(),
            "{} wrong:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    #[test]
    fn the_table_covers_enough() {
        assert!(COMMON.len() + POSTGRES.len() + MYSQL.len() >= 60);
    }

    #[test]
    fn executable_comments_are_mysql_only() {
        // PostgreSQL does not run `/*! ... */`.
        assert_eq!(
            classify("/*!50001 DROP VIEW v */", LexRules::POSTGRES),
            other(false)
        );
    }

    #[test]
    fn single_table_selects_are_told_from_the_rest() {
        let cases: &[(&str, Option<&str>)] = &[
            ("select id, name from shop.customers where id < 5", None),
            (
                "SELECT c.id AS customer, c.name n FROM shop.customers c",
                None,
            ),
            ("select * from t order by id desc limit 10 offset 5", None),
            (
                "select *, id * 2 from t where x in (select y from a, b)",
                None,
            ),
            (
                "select id, (select count(*) from o where o.c = t.id) from t",
                None,
            ),
            ("select id from t for update", None),
            ("select id from t tablesample system (10)", None),
            ("select id, row_number() over (order by id) from t", None),
            ("select 1", None),
            ("table shop.customers", None),
            ("  -- a comment\n select id from t;", None),
            ("select id from t; select id from u", Some(NOT_A_SELECT)),
            ("insert into t values (1) returning id", Some(NOT_A_SELECT)),
            ("select id into other from t", Some(NOT_A_SELECT)),
            ("values (1), (2)", Some(NOT_A_SELECT)),
            (
                "(select id from t) union (select id from u)",
                Some(NOT_A_SELECT),
            ),
            ("explain select id from t", Some(NOT_A_SELECT)),
            (
                "with x as (select id from t) select * from x",
                Some("uses WITH"),
            ),
            ("select distinct id from t", Some("uses DISTINCT")),
            ("select distinct on (id) id from t", Some("uses DISTINCT")),
            (
                "select id, count(*) from t group by id",
                Some("groups rows"),
            ),
            (
                "select id from t union all select id from u",
                Some("combines several results"),
            ),
            (
                "select a.id from t a join t b on b.id = a.id",
                Some("joins tables"),
            ),
            (
                "select t.id from t left outer join u on u.t = t.id",
                Some("joins tables"),
            ),
            (
                "select t.id from t, u where u.t = t.id",
                Some("joins tables"),
            ),
            (
                "select id from (select id from t) s",
                Some("reads from a subquery or a function"),
            ),
            (
                "select * from generate_series(1, 3)",
                Some("reads from a subquery or a function"),
            ),
            (
                "select id, unnest(tags) from t",
                Some("repeats rows with a set-returning function"),
            ),
        ];
        for rules in [LexRules::POSTGRES, LexRules::MYSQL] {
            for (sql, expected) in cases {
                assert_eq!(single_table_select(sql, rules), *expected, "{sql}");
            }
        }
        // MySQL's index hints and partitions name the table, they join nothing.
        assert_eq!(
            single_table_select(
                "select id from t use index (primary) where id > 1",
                LexRules::MYSQL
            ),
            None
        );
        assert_eq!(
            single_table_select("select id from t straight_join u", LexRules::MYSQL),
            Some("joins tables")
        );
    }
}
