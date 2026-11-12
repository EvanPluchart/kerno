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
    /// without `WHERE`, `ALTER ... DROP`): asks for confirmation.
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
struct Tok<'a> {
    kind: TokenKind,
    text: &'a str,
    depth: usize,
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
pub fn classify(statement: &str, rules: LexRules) -> StatementClass {
    let mut tokens = Vec::new();
    collect(statement, rules, &mut tokens);
    let mut depth = 0usize;
    for token in &mut tokens {
        if token.kind == TokenKind::Punct && token.text == ")" {
            depth = depth.saturating_sub(1);
        }
        token.depth = depth;
        if token.kind == TokenKind::Punct && token.text == "(" {
            depth += 1;
        }
    }
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

/// Appends the meaningful tokens of `source` to `out`. MySQL runs what is
/// inside `/*! ... */` and `/*M! ... */` comments (dumps are full of
/// them), so there the content of such a comment counts as SQL.
fn collect<'a>(source: &'a str, rules: LexRules, out: &mut Vec<Tok<'a>>) {
    for token in lex(source, rules) {
        let text = &source[token.range];
        match token.kind {
            TokenKind::BlockComment if rules.hash_comments => {
                let code = text
                    .strip_prefix("/*!")
                    .or_else(|| text.strip_prefix("/*M!"));
                if let Some(code) = code {
                    let code = code.strip_suffix("*/").unwrap_or(code);
                    // An optional version number comes first.
                    let code = code.trim_start_matches(|c: char| c.is_ascii_digit());
                    collect(code, rules, out);
                }
            }
            kind if kind.is_trivia() => {}
            kind => out.push(Tok {
                kind,
                text,
                depth: 0,
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
        "MERGE" => class(Merge, true, false, top("RETURNING")),
        "TRUNCATE" => class(Truncate, true, true, false),
        "DROP" => class(Ddl, true, true, false),
        "ALTER" => {
            // Dropping a default or a NOT NULL loses no data.
            let destroys = tokens.iter().enumerate().any(|(index, token)| {
                token.is_any(&["TRUNCATE", "DISCARD"])
                    || token.is("DROP")
                        && !tokens
                            .get(index + 1)
                            .is_some_and(|next| next.is_any(&["NOT", "DEFAULT"]))
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
        "FETCH" | "HELP" => class(Other, false, false, true),
        "MOVE" | "CLOSE" | "DECLARE" | "DEALLOCATE" | "LISTEN" | "UNLISTEN" | "DISCARD"
        | "UNLOCK" => other(false),
        // MySQL table maintenance answers with a status table.
        "OPTIMIZE" | "REPAIR" | "CHECK" | "CHECKSUM" => class(Other, true, false, true),
        // DO, EXECUTE, PREPARE, LOAD, GRANT, VACUUM ... and whatever is
        // not known: it may write.
        _ => other(true),
    }
}

/// Whether the `UPDATE` or `DELETE` at `index` has a `WHERE` of its own:
/// one inside a subquery or another CTE does not count.
fn has_where(tokens: &[Tok], index: usize) -> bool {
    let depth = tokens[index].depth;
    tokens[index + 1..]
        .iter()
        .take_while(|token| token.depth >= depth)
        .any(|token| token.depth == depth && token.is("WHERE"))
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
        let after = |words: &[&str]| index > 0 && tokens[index - 1].is_any(words);
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
        destructive |= modifies && !has_where(tokens, index);
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
        (
            "WITH s AS (SELECT 1 AS id) MERGE INTO t USING s ON t.id = s.id \
             WHEN MATCHED THEN DELETE",
            Merge,
            T,
            F,
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
        ("DELETE FROM t /*! WHERE id = 1 */", Delete, T, F, F),
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
}
