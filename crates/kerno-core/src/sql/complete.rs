//! Schema-aware completion for the SQL editor.
//!
//! [`complete`] works on whatever is being typed: it never needs the
//! statement to parse. It lexes the statement under the cursor, finds the
//! clause the cursor is in and the relations the statement names, and
//! offers what fits there from a [`Catalog`] the app loaded.

use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Range;

use super::lex::{LexRules, TokenKind, lex};
use super::split::split_statements;
use crate::schema::RelationKind;

/// The most items one completion returns.
const MAX_ITEMS: usize = 50;

/// What the editor knows of the database (loaded by the app; may be partial).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalog {
    /// Every schema known so far (databases, for MySQL).
    pub schemas: Vec<CatalogSchema>,
    /// Schemas searched for unqualified names, in order.
    pub search_path: Vec<String>,
}

/// One schema of a [`Catalog`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CatalogSchema {
    /// Schema name, unquoted.
    pub name: String,
    /// Tables and views.
    pub relations: Vec<CatalogRelation>,
    /// Names of functions and procedures.
    pub routines: Vec<String>,
}

/// A table or view of a [`CatalogSchema`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogRelation {
    /// Relation name, unquoted.
    pub name: String,
    /// Table, view, ...
    pub kind: RelationKind,
    /// Columns in table order; empty when not loaded yet.
    pub columns: Vec<CatalogColumn>,
    /// Foreign keys declared on this relation.
    pub foreign_keys: Vec<CatalogForeignKey>,
}

/// A column of a [`CatalogRelation`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogColumn {
    /// Column name, unquoted.
    pub name: String,
    /// Type as the server prints it; shown next to the column.
    pub data_type: String,
}

/// A foreign key of a [`CatalogRelation`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogForeignKey {
    /// Referencing columns, in key order.
    pub columns: Vec<String>,
    /// Schema of the referenced table.
    pub referenced_schema: String,
    /// The referenced table.
    pub referenced_table: String,
    /// Referenced columns, matching `columns` one to one.
    pub referenced_columns: Vec<String>,
}

/// What a [`CompletionItem`] is, for its icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompletionKind {
    /// An SQL keyword or keyword group (`LEFT JOIN`).
    Keyword,
    /// A schema (a database, for MySQL).
    Schema,
    /// A table.
    Table,
    /// A view or materialized view.
    View,
    /// A column.
    Column,
    /// A built-in function or a routine of the catalog.
    Function,
    /// A name the statement itself defines: a table alias or a CTE.
    Alias,
    /// A join, or a join condition, along a foreign key.
    Join,
}

/// One proposal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionItem {
    /// What the list shows.
    pub label: String,
    /// What is written, already quoted when the name needs it.
    pub insert: String,
    /// What the item is.
    pub kind: CompletionKind,
    /// Column type, "table in shop", the foreign key a join comes from ...
    pub detail: Option<String>,
}

/// The answer of [`complete`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// Byte range of the word being typed, which an accepted item replaces
    /// (empty at the cursor when there is none).
    pub replace: Range<usize>,
    /// Proposals, best first; at most 50.
    pub items: Vec<CompletionItem>,
}

/// Proposes what can be written at byte offset `cursor` of `sql`.
///
/// Only the statement under the cursor is looked at. The word the cursor is
/// in or right after is the one being typed: its part before the cursor
/// filters the proposals (case-insensitive prefix matches first, then
/// substring matches) and the whole word is [`Completion::replace`].
/// Nothing is offered inside a string, a comment or a closed quoted
/// identifier. `quote` is the dialect's `quote_ident`; it is only called for
/// names that need quoting.
pub fn complete(
    sql: &str,
    cursor: usize,
    catalog: &Catalog,
    rules: LexRules,
    quote: &dyn Fn(&str) -> String,
) -> Completion {
    let mut cursor = cursor.min(sql.len());
    while !sql.is_char_boundary(cursor) {
        cursor -= 1;
    }
    let nothing = Completion {
        replace: cursor..cursor,
        items: Vec::new(),
    };
    // A DELIMITER line is a command of the client, not SQL.
    if rules.delimiter_command {
        let line = &sql[sql[..cursor].rfind('\n').map_or(0, |at| at + 1)..cursor];
        let command = line.trim_start().get(..10);
        if command.is_some_and(|word| word.eq_ignore_ascii_case("delimiter ")) {
            return nothing;
        }
    }

    let (start, end) = statement_bounds(sql, cursor, rules);
    let text = &sql[start..end];
    let at = cursor - start;
    let tokens = lex(text, rules);

    // The word being typed is the one the cursor is in or right after.
    let mut word = at..at;
    let mut open_quote = false;
    let under = tokens
        .iter()
        .find(|token| token.range.start < at && at <= token.range.end);
    if let Some(token) = under {
        let piece = &text[token.range.clone()];
        match token.kind {
            TokenKind::Word => word = token.range.clone(),
            TokenKind::Whitespace | TokenKind::Punct | TokenKind::Semicolon => {}
            // Right after a finished comment the statement goes on.
            TokenKind::BlockComment if at == token.range.end && !runs_on(piece, rules) => {}
            TokenKind::QuotedIdent if runs_on(piece, rules) => {
                word = token.range.start..at;
                open_quote = true;
            }
            _ => return nothing,
        }
    }

    let mut toks = Vec::with_capacity(tokens.len() / 2 + 1);
    let mut n = 0;
    let mut typing = None;
    for token in &tokens {
        if token.kind.is_trivia() {
            continue;
        }
        if !word.is_empty() && token.range.start == word.start {
            typing = Some(toks.len());
            n = toks.len();
        } else if token.range.end <= at {
            n = toks.len() + 1;
        }
        toks.push(Tok {
            kind: token.kind,
            text: &text[token.range.clone()],
        });
    }
    // Inside a routine body (MySQL, under a custom delimiter) semicolons
    // still separate statements.
    let semicolon = |tok: &Tok| tok.kind == TokenKind::Semicolon;
    let low = toks[..n].iter().rposition(semicolon).map_or(0, |i| i + 1);
    let high = toks[n..]
        .iter()
        .position(semicolon)
        .map_or(toks.len(), |i| n + i);
    toks.truncate(high);
    toks.drain(..low);
    n -= low;
    typing = typing.map(|index| index - low);

    let mut depth = Vec::with_capacity(toks.len());
    let mut level = 0u32;
    for tok in &toks {
        if tok.punct(")") {
            level = level.saturating_sub(1);
        }
        depth.push(level);
        if tok.punct("(") {
            level += 1;
        }
    }

    let mut sink = Sink {
        needle: &text[word.start + usize::from(open_quote)..at],
        quote,
        mysql: rules.backtick_identifiers,
        force_quote: open_quote,
        paren_follows: text[word.end..].starts_with('('),
        prefix: Vec::new(),
        inner: Vec::new(),
    };
    Query {
        toks,
        depth,
        n,
        typing,
        catalog,
    }
    .offer(&mut sink);
    sink.prefix.append(&mut sink.inner);
    sink.prefix.truncate(MAX_ITEMS);
    Completion {
        replace: start + word.start..start + word.end,
        items: sink.prefix,
    }
}

/// The byte range of `sql` to lex for a completion at `cursor`: the
/// statement the cursor is in or, right after one, what follows it.
fn statement_bounds(sql: &str, cursor: usize, rules: LexRules) -> (usize, usize) {
    let spans = split_statements(sql, rules);
    let before = spans.partition_point(|span| span.range.start <= cursor);
    let previous = before.checked_sub(1).map(|index| &spans[index].range);
    if let Some(range) = previous
        && cursor <= range.end
    {
        return (range.start, range.end);
    }
    let gap_start = previous.map_or(0, |range| range.end);
    let gap = lex(&sql[gap_start..cursor], rules);
    if let Some(range) = previous
        && gap.iter().all(|token| token.kind.is_trivia())
    {
        return (range.start, cursor);
    }
    // A new statement starts here; only a comment can be under the cursor.
    match gap.last() {
        Some(token) if token.kind.is_trivia() && token.kind != TokenKind::Whitespace => {
            (gap_start + token.range.start, cursor)
        }
        _ => (cursor, cursor),
    }
}

/// Whether a string, comment or quoted name is unterminated: it then
/// swallows whatever is written after it.
pub(super) fn runs_on(piece: &str, rules: LexRules) -> bool {
    let probe = format!("{piece} ");
    lex(&probe, rules)
        .first()
        .is_some_and(|token| token.range.end > piece.len())
}

/// `text` in upper case when it is a short ASCII word, to match keywords
/// without allocating; an empty string otherwise.
pub(super) fn upper<'b>(text: &str, buffer: &'b mut [u8; 16]) -> &'b str {
    let bytes = text.as_bytes();
    if bytes.len() > buffer.len() || !text.is_ascii() {
        return "";
    }
    let out = &mut buffer[..bytes.len()];
    out.copy_from_slice(bytes);
    out.make_ascii_uppercase();
    std::str::from_utf8(out).unwrap_or("")
}

/// A token that is not whitespace or a comment.
#[derive(Clone, Copy)]
struct Tok<'a> {
    kind: TokenKind,
    text: &'a str,
}

impl<'a> Tok<'a> {
    fn is(&self, keyword: &str) -> bool {
        self.kind == TokenKind::Word && self.text.eq_ignore_ascii_case(keyword)
    }

    fn is_any(&self, keywords: &[&str]) -> bool {
        keywords.iter().any(|keyword| self.is(keyword))
    }

    fn punct(&self, text: &str) -> bool {
        self.kind == TokenKind::Punct && self.text == text
    }

    fn is_name(&self) -> bool {
        matches!(self.kind, TokenKind::Word | TokenKind::QuotedIdent)
    }

    /// The identifier the token spells.
    fn name(&self) -> Name<'a> {
        if self.kind != TokenKind::QuotedIdent {
            return Name {
                text: Cow::Borrowed(self.text),
                quoted: false,
            };
        }
        // `U&"..."`: the prefix is not part of the name (its escapes stay
        // as written).
        let text = match self.text.get(..2) {
            Some(prefix) if prefix.eq_ignore_ascii_case("u&") => &self.text[2..],
            _ => self.text,
        };
        let (quote, inner) = text.split_at(1);
        let inner = inner.strip_suffix(quote).unwrap_or(inner);
        let text = if inner.contains(quote) {
            Cow::Owned(inner.replace(&quote.repeat(2), quote))
        } else {
            Cow::Borrowed(inner)
        };
        Name { text, quoted: true }
    }
}

/// An identifier of the statement.
struct Name<'a> {
    text: Cow<'a, str>,
    quoted: bool,
}

impl Name<'_> {
    /// Whether it names `candidate`: exactly when quoted, whatever the case
    /// otherwise.
    fn matches(&self, candidate: &str) -> bool {
        if self.quoted {
            candidate == self.text
        } else {
            candidate.eq_ignore_ascii_case(&self.text)
        }
    }
}

/// A relation the statement names in FROM, JOIN, UPDATE or INSERT INTO.
struct Relation<'a> {
    /// How the statement refers to it, as written: its alias, else its name.
    written: &'a str,
    qualifier: Name<'a>,
    aliased: bool,
    /// Its catalog entry. `None` for a CTE, a subquery, a function, or a
    /// table the catalog does not have: the columns are unknown.
    found: Option<(&'a CatalogSchema, &'a CatalogRelation)>,
    /// 0 for the query the cursor is in, 1 for the one around it, ...
    level: usize,
    /// Index of its first token.
    at: usize,
}

/// A query the cursor is in: the statement, or a subquery in parentheses.
struct Block {
    start: usize,
    end: usize,
    /// Parenthesis depth of its own keywords.
    base: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Clause {
    Start,
    Select,
    From,
    Join,
    Into,
    Update,
    Table,
    Using,
    Where,
    On,
    GroupBy,
    OrderBy,
    Having,
    Set,
    Values,
    Returning,
    Limit,
    With,
    Other,
}

const START: &[&str] = &[
    "SELECT", "INSERT", "UPDATE", "DELETE", "WITH", "CREATE", "ALTER", "DROP", "EXPLAIN",
    "TRUNCATE", "BEGIN", "COMMIT", "ROLLBACK", "SET", "SHOW", "CALL", "ANALYZE", "VACUUM",
    "DESCRIBE", "USE",
];
const AFTER_FROM: &[&str] = &[
    "WHERE",
    "JOIN",
    "LEFT JOIN",
    "RIGHT JOIN",
    "INNER JOIN",
    "FULL JOIN",
    "CROSS JOIN",
    "GROUP BY",
    "ORDER BY",
    "LIMIT",
    "UNION",
    "RETURNING",
];
const OBJECTS: &[&str] = &[
    "TABLE",
    "VIEW",
    "INDEX",
    "SCHEMA",
    "DATABASE",
    "FUNCTION",
    "PROCEDURE",
    "TRIGGER",
    "MATERIALIZED VIEW",
    "SEQUENCE",
    "TYPE",
    "EXTENSION",
];
const ALTER_TABLE: &[&str] = &[
    "ADD COLUMN",
    "DROP COLUMN",
    "ALTER COLUMN",
    "MODIFY COLUMN",
    "RENAME TO",
    "RENAME COLUMN",
    "ADD CONSTRAINT",
    "DROP CONSTRAINT",
];
/// Keywords offered where an operand is expected.
const OPERAND: &[&str] = &["CASE", "NOT", "NULL", "TRUE", "FALSE", "EXISTS"];
/// Keywords after which an operand is expected.
const BEFORE_OPERAND: &[&str] = &[
    "SELECT",
    "DISTINCT",
    "ALL",
    "WHERE",
    "AND",
    "OR",
    "NOT",
    "ON",
    "HAVING",
    "BY",
    "SET",
    "VALUES",
    "RETURNING",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "IN",
    "LIKE",
    "ILIKE",
    "BETWEEN",
    "EXISTS",
    "ANY",
    "SOME",
    "USING",
    "LIMIT",
    "OFFSET",
];
/// Keywords an opening parenthesis follows when it starts a subquery.
const BEFORE_SUBQUERY: &[&str] = &[
    "IN", "EXISTS", "ANY", "ALL", "SOME", "FROM", "JOIN", "AS", "LATERAL", "UNION", "AND", "OR",
    "NOT", "WHERE", "ON", "SELECT", "WHEN", "THEN", "ELSE",
];
/// Words that end a table reference: they are never its alias.
const AFTER_TABLE_REF: &[&str] = &[
    "WHERE",
    "JOIN",
    "INNER",
    "LEFT",
    "RIGHT",
    "FULL",
    "CROSS",
    "NATURAL",
    "OUTER",
    "ON",
    "USING",
    "GROUP",
    "ORDER",
    "HAVING",
    "LIMIT",
    "OFFSET",
    "UNION",
    "INTERSECT",
    "EXCEPT",
    "SET",
    "VALUES",
    "SELECT",
    "RETURNING",
    "WINDOW",
    "FOR",
    "FETCH",
    "DEFAULT",
    "AS",
    "WITH",
    "PARTITION",
    "TABLESAMPLE",
    "FORCE",
    "USE",
    "IGNORE",
    "FROM",
    "INTO",
    "UPDATE",
    "END",
];
const POSTGRES_ONLY: &[&str] = &[
    "FULL JOIN",
    "RETURNING",
    "DEFAULT VALUES",
    "VACUUM",
    "MATERIALIZED VIEW",
    "SEQUENCE",
    "TYPE",
    "EXTENSION",
    "ALTER COLUMN",
];
const MYSQL_ONLY: &[&str] = &["DESCRIBE", "USE", "MODIFY COLUMN"];

/// Built-in functions worth offering everywhere; a short list on purpose.
const FUNCTIONS: &[&str] = &[
    "count",
    "sum",
    "avg",
    "min",
    "max",
    "coalesce",
    "nullif",
    "lower",
    "upper",
    "length",
    "trim",
    "replace",
    "substring",
    "concat",
    "round",
    "abs",
    "now",
    "row_number",
];
const POSTGRES_FUNCTIONS: &[&str] = &[
    "string_agg",
    "array_agg",
    "date_trunc",
    "to_char",
    "jsonb_build_object",
    "generate_series",
];
const MYSQL_FUNCTIONS: &[&str] = &[
    "ifnull",
    "group_concat",
    "date_format",
    "curdate",
    "json_extract",
];

/// Names that must be quoted although they look plain. Only the words most
/// likely to name a table or a column are here, not either engine's full
/// list; quoting one name too many is harmless.
const RESERVED: &[&str] = &[
    "all",
    "and",
    "as",
    "asc",
    "between",
    "by",
    "case",
    "check",
    "column",
    "constraint",
    "create",
    "default",
    "delete",
    "desc",
    "distinct",
    "drop",
    "else",
    "end",
    "foreign",
    "from",
    "group",
    "having",
    "in",
    "index",
    "insert",
    "into",
    "is",
    "join",
    "key",
    "like",
    "limit",
    "not",
    "null",
    "offset",
    "on",
    "or",
    "order",
    "primary",
    "references",
    "select",
    "set",
    "table",
    "then",
    "to",
    "union",
    "unique",
    "update",
    "user",
    "using",
    "values",
    "when",
    "where",
    "with",
];

/// Whether `name` must be quoted to mean itself: PostgreSQL folds unquoted
/// names to lower case, MySQL keeps them as written.
fn needs_quotes(name: &str, mysql: bool) -> bool {
    let start = |byte: u8| {
        byte == b'_'
            || if mysql {
                byte.is_ascii_alphabetic()
            } else {
                byte.is_ascii_lowercase()
            }
    };
    let mut bytes = name.bytes();
    !bytes.next().is_some_and(start)
        || !bytes.all(|byte| start(byte) || byte.is_ascii_digit() || byte == b'$')
        || RESERVED.iter().any(|word| name.eq_ignore_ascii_case(word))
}

/// The keywords that can only follow `word`, if it is such a word.
fn follows(word: &str) -> Option<&'static [&'static str]> {
    let mut buffer = [0; 16];
    Some(match upper(word, &mut buffer) {
        "INSERT" => &["INTO"],
        "DELETE" => &["FROM"],
        "GROUP" | "ORDER" | "PARTITION" => &["BY"],
        "LEFT" | "RIGHT" | "FULL" => &["JOIN", "OUTER JOIN"],
        "INNER" | "CROSS" | "NATURAL" | "OUTER" => &["JOIN"],
        "IS" => &["NULL", "NOT NULL", "TRUE", "FALSE"],
        "CREATE" | "DROP" | "ALTER" => OBJECTS,
        // A name is being given: nothing to complete.
        "AS" => &[],
        _ => return None,
    })
}

/// The keywords that fit after a complete expression of `clause`.
fn after_operand(clause: Clause) -> &'static [&'static str] {
    match clause {
        Clause::Select => &["FROM", "AS"],
        Clause::Where => &[
            "AND",
            "OR",
            "IN",
            "NOT IN",
            "IS NULL",
            "IS NOT NULL",
            "LIKE",
            "BETWEEN",
            "GROUP BY",
            "ORDER BY",
            "LIMIT",
            "RETURNING",
        ],
        Clause::On => &["AND", "OR"],
        Clause::GroupBy => &["HAVING", "ORDER BY", "LIMIT"],
        Clause::Having => &["AND", "OR", "ORDER BY", "LIMIT"],
        Clause::OrderBy => &["ASC", "DESC", "LIMIT", "OFFSET"],
        Clause::Set => &["WHERE", "FROM", "RETURNING"],
        Clause::Values => &["RETURNING"],
        Clause::Returning => &["AS"],
        _ => &[],
    }
}

/// Whether an operand, rather than an operator or the next clause, is
/// expected after `prev` (`before` is the token before it).
fn expects_operand(prev: &Tok, before: Option<&Tok>) -> bool {
    match prev.kind {
        TokenKind::Word => prev.is_any(BEFORE_OPERAND),
        TokenKind::Punct => match prev.text {
            ")" | "]" => false,
            // `SELECT *`, `t.*` and `count(*)` are complete; `a *` is not.
            "*" => !before.is_none_or(|tok| {
                tok.is_any(&["SELECT", "DISTINCT", "ALL"])
                    || tok.punct(",")
                    || tok.punct("(")
                    || tok.punct(".")
            }),
            _ => true,
        },
        _ => false,
    }
}

fn relation_kind(kind: RelationKind) -> CompletionKind {
    if kind.is_table() {
        CompletionKind::Table
    } else {
        CompletionKind::View
    }
}

fn kind_name(kind: RelationKind) -> &'static str {
    match kind {
        RelationKind::Table => "table",
        RelationKind::PartitionedTable => "partitioned table",
        RelationKind::ForeignTable => "foreign table",
        RelationKind::View => "view",
        RelationKind::MaterializedView => "materialized view",
    }
}

/// The schemas unqualified names are looked up in, in order.
fn path_schemas(catalog: &Catalog) -> impl Iterator<Item = &CatalogSchema> {
    catalog
        .search_path
        .iter()
        .filter_map(|name| catalog.schemas.iter().find(|schema| schema.name == *name))
}

/// `FK child(a, b) → parent(c, d)`.
fn key_detail(child: &str, columns: &[String], parent: &str, referenced: &[String]) -> String {
    format!(
        "FK {child}({}) → {parent}({})",
        columns.join(", "),
        referenced.join(", ")
    )
}

/// Collects the proposals that match the word being typed.
struct Sink<'q> {
    /// The typed part of the word.
    needle: &'q str,
    quote: &'q dyn Fn(&str) -> String,
    mysql: bool,
    /// Inside an open quoted identifier: names only, always quoted.
    force_quote: bool,
    /// A `(` already follows the word.
    paren_follows: bool,
    /// Prefix matches, then substring matches, each in the order offered.
    prefix: Vec<CompletionItem>,
    inner: Vec<CompletionItem>,
}

impl Sink<'_> {
    /// Keeps the item `make` builds if `key` matches the word being typed.
    fn offer(&mut self, key: &str, make: impl FnOnce(&Self) -> CompletionItem) {
        // Nothing offered later can outrank fifty prefix matches.
        if self.prefix.len() >= MAX_ITEMS {
            return;
        }
        let (key, needle) = (key.as_bytes(), self.needle.as_bytes());
        if key.len() >= needle.len() && key[..needle.len()].eq_ignore_ascii_case(needle) {
            let item = make(self);
            self.prefix.push(item);
        } else if self.inner.len() < MAX_ITEMS
            && key
                .windows(needle.len())
                .any(|window| window.eq_ignore_ascii_case(needle))
        {
            let item = make(self);
            self.inner.push(item);
        }
    }

    /// `name` as it must be written.
    fn ident(&self, name: &str) -> String {
        if self.force_quote || needs_quotes(name, self.mysql) {
            (self.quote)(name)
        } else {
            name.to_string()
        }
    }

    fn keywords(&mut self, words: &[&str]) {
        if self.force_quote {
            return;
        }
        let foreign = if self.mysql {
            POSTGRES_ONLY
        } else {
            MYSQL_ONLY
        };
        for word in words {
            if foreign.contains(word) {
                continue;
            }
            self.offer(word, |_| CompletionItem {
                label: (*word).to_string(),
                insert: (*word).to_string(),
                kind: CompletionKind::Keyword,
                detail: None,
            });
        }
    }

    /// A function; `schema` is where a routine of the catalog lives.
    fn function(&mut self, name: &str, schema: Option<&str>) {
        if self.force_quote {
            return;
        }
        self.offer(name, |sink| {
            let mut insert = if schema.is_some() {
                sink.ident(name)
            } else {
                name.to_string()
            };
            if !sink.paren_follows {
                insert.push('(');
            }
            CompletionItem {
                label: name.to_string(),
                insert,
                kind: CompletionKind::Function,
                detail: Some(match schema {
                    Some(schema) => format!("function in {schema}"),
                    None => "function".to_string(),
                }),
            }
        });
    }

    fn functions(&mut self, catalog: &Catalog) {
        let engine = if self.mysql {
            MYSQL_FUNCTIONS
        } else {
            POSTGRES_FUNCTIONS
        };
        for name in FUNCTIONS.iter().chain(engine) {
            self.function(name, None);
        }
        for schema in path_schemas(catalog) {
            for routine in &schema.routines {
                self.function(routine, Some(&schema.name));
            }
        }
    }

    /// The relations of `schema`, then its routines if `routines`.
    fn schema(&mut self, schema: &CatalogSchema, routines: bool) {
        for relation in &schema.relations {
            self.offer(&relation.name, |sink| CompletionItem {
                label: relation.name.clone(),
                insert: sink.ident(&relation.name),
                kind: relation_kind(relation.kind),
                detail: Some(format!("{} in {}", kind_name(relation.kind), schema.name)),
            });
        }
        if routines {
            for routine in &schema.routines {
                self.function(routine, Some(&schema.name));
            }
        }
    }

    /// The columns of one relation, unqualified.
    fn columns(&mut self, relation: &CatalogRelation) {
        for column in &relation.columns {
            self.offer(&column.name, |sink| CompletionItem {
                label: column.name.clone(),
                insert: sink.ident(&column.name),
                kind: CompletionKind::Column,
                detail: Some(column.data_type.clone()),
            });
        }
    }

    /// The columns of the relations of the innermost query; a name that
    /// several of them have is qualified.
    fn scope_columns(&mut self, scope: &[Relation]) {
        let known = || {
            scope
                .iter()
                .filter(|relation| relation.level == 0)
                .filter_map(|relation| relation.found.map(|(_, found)| (relation, found)))
        };
        let mut seen: HashMap<&str, u32> = HashMap::new();
        if known().nth(1).is_some() {
            for column in known().flat_map(|(_, found)| &found.columns) {
                *seen.entry(column.name.as_str()).or_default() += 1;
            }
        }
        for (relation, found) in known() {
            for column in &found.columns {
                let shared = seen.get(column.name.as_str()).is_some_and(|&n| n > 1);
                self.offer(&column.name, |sink| {
                    let mut label = column.name.clone();
                    let mut insert = sink.ident(&column.name);
                    if shared {
                        label = format!("{}.{label}", relation.written);
                        insert = format!("{}.{insert}", relation.written);
                    }
                    CompletionItem {
                        label,
                        insert,
                        kind: CompletionKind::Column,
                        detail: Some(column.data_type.clone()),
                    }
                });
            }
        }
    }

    /// What the statement calls its relations.
    fn aliases(&mut self, scope: &[Relation]) {
        for relation in scope {
            self.offer(&relation.qualifier.text, |_| CompletionItem {
                label: relation.written.to_string(),
                insert: relation.written.to_string(),
                kind: match relation.found {
                    Some((_, found)) if !relation.aliased => relation_kind(found.kind),
                    _ => CompletionKind::Alias,
                },
                detail: relation
                    .found
                    .map(|(schema, found)| format!("{}.{}", schema.name, found.name)),
            });
        }
    }
}

/// The statement under the cursor, as tokens.
struct Query<'a> {
    toks: Vec<Tok<'a>>,
    /// Parenthesis depth of each token; a parenthesis has the depth of what
    /// is around it.
    depth: Vec<u32>,
    /// How many tokens come before the cursor and the word being typed.
    n: usize,
    /// Index of the word being typed.
    typing: Option<usize>,
    catalog: &'a Catalog,
}

impl<'a> Query<'a> {
    fn word(&self, index: usize, keyword: &str) -> bool {
        self.toks.get(index).is_some_and(|tok| tok.is(keyword))
    }

    fn punct(&self, index: usize, text: &str) -> bool {
        self.toks.get(index).is_some_and(|tok| tok.punct(text))
    }

    /// Index of the parenthesis closing the one at `open`, or the token count.
    fn close_of(&self, open: usize) -> usize {
        (open + 1..self.toks.len())
            .find(|&index| self.depth[index] == self.depth[open] && self.toks[index].punct(")"))
            .unwrap_or(self.toks.len())
    }

    /// Parenthesis depth just before token `pos`.
    fn depth_at(&self, pos: usize) -> u32 {
        match pos.checked_sub(1) {
            Some(prev) => self.depth[prev] + u32::from(self.toks[prev].punct("(")),
            None => 0,
        }
    }

    /// The queries around the cursor, innermost first; the last one is the
    /// statement itself.
    fn blocks(&self) -> Vec<Block> {
        let mut open = Vec::new();
        for (index, tok) in self.toks[..self.n].iter().enumerate() {
            if tok.punct("(") {
                open.push(index);
            } else if tok.punct(")") {
                open.pop();
            }
        }
        let mut blocks = Vec::new();
        for &index in open.iter().rev() {
            let first = index + 1;
            if first < self.n && (self.word(first, "SELECT") || self.word(first, "WITH")) {
                blocks.push(Block {
                    start: first,
                    end: self.close_of(index),
                    base: self.depth[index] + 1,
                });
            }
        }
        blocks.push(Block {
            start: 0,
            end: self.toks.len(),
            base: 0,
        });
        blocks
    }

    /// The part of `block` between the UNION, INTERSECT or EXCEPT around
    /// the cursor: each side has its own relations.
    fn segment(&self, block: &Block) -> Range<usize> {
        let set_operator = |index: &usize| {
            self.depth[*index] == block.base
                && self.toks[*index].is_any(&["UNION", "INTERSECT", "EXCEPT"])
        };
        let start = (block.start..self.n)
            .rev()
            .find(set_operator)
            .map_or(block.start, |index| index + 1);
        let end = (self.n..block.end).find(set_operator).unwrap_or(block.end);
        start..end
    }

    /// The names `WITH` defines at the start of `block`.
    fn cte_names(&self, block: &Block, names: &mut Vec<Name<'a>>) {
        let mut index = block.start;
        if !self.word(index, "WITH") {
            return;
        }
        index += 1;
        if self.word(index, "RECURSIVE") {
            index += 1;
        }
        loop {
            let Some(name) = self.toks.get(index).filter(|tok| tok.is_name()) else {
                return;
            };
            if Some(index) != self.typing {
                names.push(name.name());
            }
            index += 1;
            if self.punct(index, "(") {
                index = self.close_of(index) + 1;
            }
            if !self.word(index, "AS") {
                return;
            }
            index += 1;
            while self.word(index, "NOT") || self.word(index, "MATERIALIZED") {
                index += 1;
            }
            if !self.punct(index, "(") {
                return;
            }
            index = self.close_of(index) + 1;
            if !self.punct(index, ",") {
                return;
            }
            index += 1;
        }
    }

    /// Looks `path` (`table`, `schema.table`) up in the catalog. An
    /// unqualified name that a CTE defines is not a table.
    fn resolve(
        &self,
        path: &[Name<'a>],
        ctes: &[Name<'a>],
    ) -> Option<(&'a CatalogSchema, &'a CatalogRelation)> {
        let (schema, name) = match path {
            [name] => (None, name),
            [.., schema, name] => (Some(schema), name),
            [] => return None,
        };
        let relation_in = |schema: &'a CatalogSchema| {
            let relation = schema.relations.iter().find(|r| name.matches(&r.name))?;
            Some((schema, relation))
        };
        match schema {
            Some(schema) => {
                let schema = self
                    .catalog
                    .schemas
                    .iter()
                    .find(|s| schema.matches(&s.name))?;
                relation_in(schema)
            }
            None if ctes.iter().any(|cte| cte.matches(&name.text)) => None,
            None => path_schemas(self.catalog).find_map(relation_in),
        }
    }

    /// Adds to `scope` the relations named in `range` at depth `base`.
    fn collect(
        &self,
        range: Range<usize>,
        base: u32,
        level: usize,
        ctes: &[Name<'a>],
        scope: &mut Vec<Relation<'a>>,
    ) {
        let mut index = range.start;
        while index < range.end {
            let tok = &self.toks[index];
            if self.depth[index] != base || tok.kind != TokenKind::Word {
                index += 1;
                continue;
            }
            let mut buffer = [0; 16];
            let (list, into) = match upper(tok.text, &mut buffer) {
                "FROM" => (true, false),
                "JOIN" => (false, false),
                "INTO" => (false, true),
                "UPDATE" if !self.not_a_statement(index) => (true, false),
                _ => {
                    index += 1;
                    continue;
                }
            };
            index += 1;
            loop {
                index = self.table_reference(index, into, level, ctes, scope);
                if list && index < range.end && self.punct(index, ",") {
                    index += 1;
                } else {
                    break;
                }
            }
        }
    }

    /// Whether the `UPDATE` at `index` is part of `FOR UPDATE`, `ON
    /// CONFLICT ... DO UPDATE` or `ON DUPLICATE KEY UPDATE`.
    fn not_a_statement(&self, index: usize) -> bool {
        index
            .checked_sub(1)
            .is_some_and(|prev| self.toks[prev].is_any(&["FOR", "DO", "ON", "KEY"]))
    }

    /// Reads one table reference at `index` (`schema.table alias`, `(...)
    /// alias`, `function(...) alias`) into `scope`; returns the index after it.
    fn table_reference(
        &self,
        mut index: usize,
        into: bool,
        level: usize,
        ctes: &[Name<'a>],
        scope: &mut Vec<Relation<'a>>,
    ) -> usize {
        while self.word(index, "ONLY") || self.word(index, "LATERAL") {
            index += 1;
        }
        let at = index;
        let mut path = Vec::new();
        // Whether the catalog can know its columns, and whether the name
        // is finished (not the word being typed, no dangling dot).
        let mut known = true;
        let mut complete = true;
        if self.punct(index, "(") {
            index = self.close_of(index) + 1;
            known = false;
        } else {
            loop {
                let named = self.toks.get(index).is_some_and(|tok| {
                    tok.is_name() && !(tok.kind == TokenKind::Word && tok.is_any(AFTER_TABLE_REF))
                });
                if !named {
                    complete = false;
                    break;
                }
                complete &= Some(index) != self.typing;
                path.push(index);
                index += 1;
                if !self.punct(index, ".") {
                    break;
                }
                index += 1;
            }
            if path.is_empty() {
                return index;
            }
            // A function in FROM; after INTO it is the column list.
            if self.punct(index, "(") && !into {
                index = self.close_of(index) + 1;
                known = false;
            }
        }
        let mut alias = None;
        if !(into && self.punct(index, "(")) {
            if self.word(index, "AS") {
                index += 1;
                if self.toks.get(index).is_some_and(Tok::is_name) {
                    alias = Some(index);
                    index += 1;
                }
            } else if self.toks.get(index).is_some_and(|tok| {
                tok.kind == TokenKind::QuotedIdent
                    || (tok.kind == TokenKind::Word && !tok.is_any(AFTER_TABLE_REF))
            }) {
                alias = Some(index);
                index += 1;
            }
            // The column names of a derived table: `(...) AS t (a, b)`.
            if alias.is_some() && !known && self.punct(index, "(") {
                index = self.close_of(index) + 1;
            }
        }
        let alias = alias.filter(|&alias| Some(alias) != self.typing);
        let Some(&qualifier) = alias.as_ref().or(path.last()) else {
            // A subquery without an alias cannot be referred to.
            return index;
        };
        if complete {
            let names: Vec<Name> = path.iter().map(|&part| self.toks[part].name()).collect();
            scope.push(Relation {
                written: self.toks[qualifier].text,
                qualifier: self.toks[qualifier].name(),
                aliased: alias.is_some(),
                found: if known {
                    self.resolve(&names, ctes)
                } else {
                    None
                },
                level,
                at,
            });
        }
        index
    }

    /// The clause position `pos` of `segment` is in, and the index of its
    /// keyword. Only keywords of the query itself count, not those inside
    /// its parentheses (`extract(year FROM x)`).
    fn clause_at(&self, block: &Block, segment_start: usize, pos: usize) -> (Clause, usize) {
        for index in (segment_start..pos).rev() {
            let tok = &self.toks[index];
            if self.depth[index] != block.base || tok.kind != TokenKind::Word {
                continue;
            }
            let after = |keyword| index > 0 && self.word(index - 1, keyword);
            let mut buffer = [0; 16];
            let clause = match upper(tok.text, &mut buffer) {
                "SELECT" => Clause::Select,
                "FROM" => Clause::From,
                "JOIN" => Clause::Join,
                "INTO" => Clause::Into,
                "UPDATE" if self.not_a_statement(index) => Clause::Other,
                "UPDATE" => Clause::Update,
                // A new table has no name to complete.
                "TABLE" if self.word(block.start, "CREATE") => Clause::Other,
                "TABLE" | "TRUNCATE" | "DESCRIBE" => Clause::Table,
                "DESC" if index == block.start => Clause::Table,
                "USING" => Clause::Using,
                "WHERE" => Clause::Where,
                "ON" => Clause::On,
                "HAVING" => Clause::Having,
                "SET" => Clause::Set,
                "VALUES" => Clause::Values,
                "RETURNING" => Clause::Returning,
                "BY" if after("GROUP") => Clause::GroupBy,
                "BY" if after("ORDER") => Clause::OrderBy,
                "LIMIT" | "OFFSET" => Clause::Limit,
                "WITH" if index == block.start => Clause::With,
                _ => continue,
            };
            return (clause, index);
        }
        if pos == segment_start {
            (Clause::Start, pos)
        } else {
            (Clause::Other, segment_start)
        }
    }

    /// Whether a relation name is expected at `pos`, in the table clause
    /// whose keyword is at `keyword`.
    fn names_a_table(&self, clause: Clause, keyword: usize, pos: usize) -> bool {
        let mut first = keyword + 1;
        while first < pos
            && self.toks[first].is_any(&["IF", "NOT", "EXISTS", "ONLY", "LATERAL", "TABLE"])
        {
            first += 1;
        }
        first == pos
            || (self.punct(pos - 1, ",")
                && matches!(
                    clause,
                    Clause::From | Clause::Update | Clause::Table | Clause::Using
                ))
    }

    /// Offers everything that fits at the cursor.
    fn offer(&self, sink: &mut Sink) {
        let blocks = self.blocks();
        let mut ctes = Vec::new();
        for block in &blocks {
            self.cte_names(block, &mut ctes);
        }
        let mut scope = Vec::new();
        for (level, block) in blocks.iter().enumerate() {
            self.collect(self.segment(block), block.base, level, &ctes, &mut scope);
        }
        let block = &blocks[0];
        let segment = self.segment(block);
        let n = self.n;

        // `a.`, `a.b.`: what the qualifier holds.
        if n > 0 && self.punct(n - 1, ".") {
            let mut first = n;
            while first >= 2 && self.punct(first - 1, ".") && self.toks[first - 2].is_name() {
                first -= 2;
            }
            let path: Vec<Name> = (first..n)
                .step_by(2)
                .map(|index| self.toks[index].name())
                .collect();
            let (clause, keyword) = self.clause_at(block, segment.start, first);
            let table = matches!(
                clause,
                Clause::From | Clause::Join | Clause::Into | Clause::Update | Clause::Table
            ) && self.depth_at(first) == block.base
                && self.names_a_table(clause, keyword, first);
            self.offer_qualified(sink, &path, table, &scope, &ctes);
            return;
        }

        let prev = n.checked_sub(1).map(|index| self.toks[index]);
        if let Some(prev) = prev.filter(|prev| prev.kind == TokenKind::Word)
            && let Some(next) = follows(prev.text)
        {
            sink.keywords(next);
            return;
        }

        let (clause, keyword) = self.clause_at(block, segment.start, n);
        let nested = self.depth_at(n) > block.base;
        match clause {
            Clause::Start => sink.keywords(START),
            Clause::With => {
                // After the last CTE comes the statement; before that, a
                // CTE is being named.
                if prev.is_some_and(|prev| prev.punct(")")) {
                    sink.keywords(&["SELECT", "INSERT", "UPDATE", "DELETE"]);
                }
            }
            Clause::From
            | Clause::Join
            | Clause::Into
            | Clause::Update
            | Clause::Table
            | Clause::Using => {
                if nested {
                    match clause {
                        // `INSERT INTO t (`, `JOIN t USING (`: column names.
                        Clause::Into | Clause::Using => sink.scope_columns(&scope),
                        Clause::Table => {}
                        _ => self.offer_expression(sink, clause, &scope),
                    }
                } else if self.names_a_table(clause, keyword, n) {
                    if clause == Clause::Join {
                        self.offer_joins(sink, &scope);
                    }
                    self.offer_relations(sink, &ctes);
                } else {
                    sink.keywords(match clause {
                        Clause::From | Clause::Using => AFTER_FROM,
                        Clause::Join => &["ON", "USING"],
                        Clause::Update => &["SET"],
                        Clause::Into => &["VALUES", "SELECT", "DEFAULT VALUES"],
                        Clause::Table if self.word(block.start, "ALTER") => ALTER_TABLE,
                        _ => &[],
                    });
                }
            }
            // `FROM a JOIN b ON ..., c`: the FROM list goes on.
            Clause::On if !nested && prev.is_some_and(|prev| prev.punct(",")) => {
                self.offer_relations(sink, &ctes);
            }
            Clause::Other => {
                let lead = &self.toks[segment.start..n];
                if prev.is_some_and(|prev| prev.is("BEGIN"))
                    || lead
                        .iter()
                        .all(|tok| tok.is_any(&["EXPLAIN", "ANALYZE", "VERBOSE"]))
                {
                    sink.keywords(START);
                }
            }
            Clause::Limit => {
                let before = n.checked_sub(2).map(|index| &self.toks[index]);
                if prev.is_some_and(|prev| !expects_operand(&prev, before)) {
                    sink.keywords(&["OFFSET"]);
                }
            }
            _ => self.offer_expression(sink, clause, &scope),
        }
    }

    /// After `path` and a dot: the relations of a schema where a table is
    /// expected, else the columns of an alias or of a table.
    fn offer_qualified(
        &self,
        sink: &mut Sink,
        path: &[Name<'a>],
        table: bool,
        scope: &[Relation<'a>],
        ctes: &[Name<'a>],
    ) {
        let schema_named =
            |name: &Name| self.catalog.schemas.iter().find(|s| name.matches(&s.name));
        match path {
            [name] if table => {
                if let Some(schema) = schema_named(name) {
                    sink.schema(schema, false);
                }
            }
            [name] => {
                let in_scope = scope.iter().find(|relation| {
                    if name.quoted || relation.qualifier.quoted {
                        relation.qualifier.text == name.text
                    } else {
                        relation.qualifier.text.eq_ignore_ascii_case(&name.text)
                    }
                });
                if let Some(relation) = in_scope {
                    // Nothing rather than wrong columns for a CTE or a subquery.
                    if let Some((_, found)) = relation.found {
                        sink.columns(found);
                    }
                } else if let Some(schema) = schema_named(name) {
                    sink.schema(schema, true);
                } else if let Some((_, found)) = self.resolve(path, ctes) {
                    sink.columns(found);
                }
            }
            [_, _, ..] if !table => {
                if let Some((_, found)) = self.resolve(path, &[]) {
                    sink.columns(found);
                }
            }
            _ => {}
        }
    }

    /// CTE names, the relations of the search path, then the schemas.
    fn offer_relations(&self, sink: &mut Sink, ctes: &[Name<'a>]) {
        for cte in ctes {
            sink.offer(&cte.text, |sink| CompletionItem {
                label: cte.text.to_string(),
                insert: sink.ident(&cte.text),
                kind: CompletionKind::Alias,
                detail: Some("CTE".to_string()),
            });
        }
        for schema in path_schemas(self.catalog) {
            sink.schema(schema, false);
        }
        for schema in &self.catalog.schemas {
            sink.offer(&schema.name, |sink| CompletionItem {
                label: schema.name.clone(),
                insert: sink.ident(&schema.name),
                kind: CompletionKind::Schema,
                detail: None,
            });
        }
    }

    /// Columns in scope, aliases, functions and the keywords that fit an
    /// expression of `clause`.
    fn offer_expression(&self, sink: &mut Sink, clause: Clause, scope: &[Relation<'a>]) {
        let Some(prev) = self.n.checked_sub(1).map(|index| self.toks[index]) else {
            return;
        };
        let before = self.n.checked_sub(2).map(|index| &self.toks[index]);
        let operand = expects_operand(&prev, before);
        if operand {
            let subquery = prev.punct("(")
                && before.is_none_or(|tok| match tok.kind {
                    TokenKind::Word => tok.is_any(BEFORE_SUBQUERY),
                    TokenKind::Punct => !tok.punct(")") && !tok.punct("]"),
                    _ => false,
                });
            if subquery {
                sink.keywords(&["SELECT"]);
            }
            if prev.is("CASE") {
                sink.keywords(&["WHEN"]);
            }
            if clause == Clause::On && prev.is("ON") {
                self.offer_conditions(sink, scope);
            }
        } else {
            let open_cases = self.toks[..self.n].iter().fold(0u32, |open, tok| {
                if tok.is("CASE") {
                    open + 1
                } else if tok.is("END") {
                    open.saturating_sub(1)
                } else {
                    open
                }
            });
            if open_cases > 0 {
                sink.keywords(&["WHEN", "THEN", "ELSE", "END"]);
            }
            sink.keywords(after_operand(clause));
            if clause == Clause::On {
                sink.keywords(AFTER_FROM);
            }
        }
        sink.scope_columns(scope);
        sink.aliases(scope);
        sink.functions(self.catalog);
        if operand {
            if prev.is("SELECT") {
                sink.keywords(&["DISTINCT"]);
            }
            sink.keywords(OPERAND);
        }
    }

    /// After JOIN: the tables a foreign key links to the relations of the
    /// query, in either direction, with their ON condition.
    fn offer_joins(&self, sink: &mut Sink, scope: &[Relation<'a>]) {
        if sink.force_quote {
            return;
        }
        let here = || scope.iter().filter(|relation| relation.level == 0);
        let joined = |schema: &str, table: &str| {
            here().any(|relation| {
                relation
                    .found
                    .is_some_and(|(s, t)| s.name == schema && t.name == table)
            })
        };
        // Keys of the relations in scope.
        for relation in here() {
            let Some((schema, table)) = relation.found else {
                continue;
            };
            for key in &table.foreign_keys {
                let (to_schema, to_table) = (&key.referenced_schema, &key.referenced_table);
                let itself = *to_schema == schema.name && *to_table == table.name;
                if !itself && joined(to_schema, to_table) {
                    continue;
                }
                self.offer_join(
                    sink,
                    scope,
                    (to_schema, to_table, &key.referenced_columns),
                    (relation, &key.columns),
                    || key_detail(&table.name, &key.columns, to_table, &key.referenced_columns),
                );
            }
        }
        // Keys that reference them.
        for schema in &self.catalog.schemas {
            for table in &schema.relations {
                for key in &table.foreign_keys {
                    for relation in here() {
                        let Some((to_schema, to_table)) = relation.found else {
                            continue;
                        };
                        if key.referenced_schema != to_schema.name
                            || key.referenced_table != to_table.name
                            || (!std::ptr::eq(table, to_table) && joined(&schema.name, &table.name))
                        {
                            continue;
                        }
                        self.offer_join(
                            sink,
                            scope,
                            (&schema.name, &table.name, &key.columns),
                            (relation, &key.referenced_columns),
                            || {
                                key_detail(
                                    &table.name,
                                    &key.columns,
                                    &to_table.name,
                                    &key.referenced_columns,
                                )
                            },
                        );
                    }
                }
            }
        }
    }

    /// One `table ON table.a = other.b` proposal: `(schema, table,
    /// columns)` of the table to join, and the relation in scope with its
    /// matching columns.
    fn offer_join(
        &self,
        sink: &mut Sink,
        scope: &[Relation<'a>],
        (schema, table, columns): (&str, &str, &[String]),
        (other, other_columns): (&Relation<'a>, &[String]),
        detail: impl FnOnce() -> String,
    ) {
        sink.offer(table, |sink| {
            let mut target = sink.ident(table);
            if !self.catalog.search_path.iter().any(|name| name == schema) {
                target = format!("{}.{target}", sink.ident(schema));
            }
            let mut qualifier = sink.ident(table);
            let taken = scope.iter().any(|relation| {
                relation.level == 0 && relation.qualifier.text.eq_ignore_ascii_case(table)
            });
            if taken {
                qualifier = sink.ident(&format!("{table}2"));
                target = format!("{target} {qualifier}");
            }
            let condition: Vec<String> = columns
                .iter()
                .zip(other_columns)
                .map(|(column, other_column)| {
                    format!(
                        "{qualifier}.{} = {}.{}",
                        sink.ident(column),
                        other.written,
                        sink.ident(other_column)
                    )
                })
                .collect();
            let text = format!("{target} ON {}", condition.join(" AND "));
            CompletionItem {
                label: text.clone(),
                insert: text,
                kind: CompletionKind::Join,
                detail: Some(detail()),
            }
        });
    }

    /// After `JOIN x ON`: the foreign-key conditions between `x` and the
    /// other relations of the query.
    fn offer_conditions(&self, sink: &mut Sink, scope: &[Relation<'a>]) {
        if sink.force_quote {
            return;
        }
        let here = || {
            scope
                .iter()
                .filter(|relation| relation.level == 0 && relation.at < self.n)
        };
        let Some(joined) = here().max_by_key(|relation| relation.at) else {
            return;
        };
        let Some((schema, table)) = joined.found else {
            return;
        };
        let mut offer =
            |columns: &[String], other: &Relation, other_columns: &[String], detail: String| {
                let condition: Vec<String> = columns
                    .iter()
                    .zip(other_columns)
                    .map(|(column, other_column)| {
                        format!(
                            "{}.{} = {}.{}",
                            joined.written,
                            sink.ident(column),
                            other.written,
                            sink.ident(other_column)
                        )
                    })
                    .collect();
                let text = condition.join(" AND ");
                sink.offer(&text, |_| CompletionItem {
                    label: text.clone(),
                    insert: text.clone(),
                    kind: CompletionKind::Join,
                    detail: Some(detail),
                });
            };
        for other in here() {
            let Some((other_schema, other_table)) = other.found else {
                continue;
            };
            if std::ptr::eq(other, joined) {
                continue;
            }
            for key in &table.foreign_keys {
                if key.referenced_schema == other_schema.name
                    && key.referenced_table == other_table.name
                {
                    let detail = key_detail(
                        &table.name,
                        &key.columns,
                        &other_table.name,
                        &key.referenced_columns,
                    );
                    offer(&key.columns, other, &key.referenced_columns, detail);
                }
            }
            for key in &other_table.foreign_keys {
                if key.referenced_schema == schema.name && key.referenced_table == table.name {
                    let detail = key_detail(
                        &other_table.name,
                        &key.columns,
                        &table.name,
                        &key.referenced_columns,
                    );
                    offer(&key.referenced_columns, other, &key.columns, detail);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use CompletionKind::*;

    type Fk<'a> = (&'a [&'a str], &'a str, &'a [&'a str]);

    fn strings(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    fn relation(
        name: &str,
        kind: RelationKind,
        columns: &[(&str, &str)],
        foreign_keys: &[Fk],
    ) -> CatalogRelation {
        CatalogRelation {
            name: name.into(),
            kind,
            columns: columns
                .iter()
                .map(|(name, data_type)| CatalogColumn {
                    name: (*name).into(),
                    data_type: (*data_type).into(),
                })
                .collect(),
            foreign_keys: foreign_keys
                .iter()
                .map(|(columns, target, referenced)| {
                    let (schema, table) = target.split_once('.').expect("schema.table");
                    CatalogForeignKey {
                        columns: strings(columns),
                        referenced_schema: schema.into(),
                        referenced_table: table.into(),
                        referenced_columns: strings(referenced),
                    }
                })
                .collect(),
        }
    }

    fn table(name: &str, columns: &[(&str, &str)], foreign_keys: &[Fk]) -> CatalogRelation {
        relation(name, RelationKind::Table, columns, foreign_keys)
    }

    /// The `shop` fixture schema of `tests/fixtures/postgres/01_schema.sql`,
    /// trimmed, plus names that need quoting.
    fn catalog() -> Catalog {
        let shop = CatalogSchema {
            name: "shop".into(),
            relations: vec![
                table(
                    "customers",
                    &[
                        ("id", "bigint"),
                        ("email", "text"),
                        ("full_name", "text"),
                        ("phone", "varchar(32)"),
                        ("is_active", "boolean"),
                        ("created_at", "timestamptz"),
                    ],
                    &[],
                ),
                table(
                    "categories",
                    &[
                        ("id", "integer"),
                        ("parent_id", "integer"),
                        ("name", "text"),
                    ],
                    &[(&["parent_id"], "shop.categories", &["id"])],
                ),
                table(
                    "products",
                    &[
                        ("id", "bigint"),
                        ("category_id", "integer"),
                        ("sku", "varchar(32)"),
                        ("name", "text"),
                        ("price", "numeric(10,2)"),
                    ],
                    &[(&["category_id"], "shop.categories", &["id"])],
                ),
                table(
                    "orders",
                    &[
                        ("id", "bigint"),
                        ("customer_id", "bigint"),
                        ("status", "shop.order_status"),
                        ("placed_at", "timestamptz"),
                        ("total", "numeric(12,2)"),
                    ],
                    &[(&["customer_id"], "shop.customers", &["id"])],
                ),
                table(
                    "order_items",
                    &[
                        ("order_id", "bigint"),
                        ("line_no", "smallint"),
                        ("product_id", "bigint"),
                        ("quantity", "integer"),
                        ("unit_price", "numeric(10,2)"),
                    ],
                    &[
                        (&["order_id"], "shop.orders", &["id"]),
                        (&["product_id"], "shop.products", &["id"]),
                    ],
                ),
                table(
                    "reviews",
                    &[
                        ("id", "bigint"),
                        ("product_id", "bigint"),
                        ("customer_id", "bigint"),
                        ("rating", "smallint"),
                        ("body", "text"),
                    ],
                    &[
                        (&["product_id"], "shop.products", &["id"]),
                        (&["customer_id"], "shop.customers", &["id"]),
                    ],
                ),
                table(
                    "audit_log",
                    &[
                        ("happened_at", "timestamptz"),
                        ("actor", "text"),
                        ("action", "text"),
                    ],
                    &[],
                ),
                relation(
                    "order_totals",
                    RelationKind::View,
                    &[
                        ("order_id", "bigint"),
                        ("customer_id", "bigint"),
                        ("computed_total", "numeric"),
                    ],
                    &[],
                ),
                relation(
                    "product_sales",
                    RelationKind::MaterializedView,
                    &[("product_id", "bigint"), ("units_sold", "bigint")],
                    &[],
                ),
            ],
            routines: strings(&["customer_lifetime_value", "set_updated_at"]),
        };
        let analytics = CatalogSchema {
            name: "analytics".into(),
            relations: vec![
                table(
                    "events",
                    &[
                        ("id", "bigint"),
                        ("customer_id", "bigint"),
                        ("event_type", "text"),
                    ],
                    &[(&["customer_id"], "shop.customers", &["id"])],
                ),
                table(
                    "shipments",
                    &[
                        ("order_id", "bigint"),
                        ("line_no", "smallint"),
                        ("carrier", "text"),
                    ],
                    &[(
                        &["order_id", "line_no"],
                        "shop.order_items",
                        &["order_id", "line_no"],
                    )],
                ),
            ],
            routines: Vec::new(),
        };
        let public = CatalogSchema {
            name: "public".into(),
            relations: vec![
                table("all_types", &[("id", "integer"), ("c_text", "text")], &[]),
                table(
                    "Order Lines",
                    &[("Line No", "integer"), ("qty", "integer")],
                    &[],
                ),
                table("user", &[("id", "integer"), ("name", "text")], &[]),
            ],
            routines: Vec::new(),
        };
        Catalog {
            schemas: vec![shop, analytics, public],
            search_path: strings(&["shop", "public"]),
        }
    }

    fn pg_quote(name: &str) -> String {
        format!("\"{}\"", name.replace('"', "\"\""))
    }

    fn my_quote(name: &str) -> String {
        format!("`{}`", name.replace('`', "``"))
    }

    /// Completes at the `|` of `sql`.
    fn run(sql: &str, rules: LexRules) -> Completion {
        let cursor = sql.find('|').expect("a | marks the cursor");
        let text = format!("{}{}", &sql[..cursor], &sql[cursor + 1..]);
        let catalog = catalog();
        if rules == LexRules::MYSQL {
            complete(&text, cursor, &catalog, rules, &my_quote)
        } else {
            complete(&text, cursor, &catalog, rules, &pg_quote)
        }
    }

    fn pg(sql: &str) -> Completion {
        run(sql, LexRules::POSTGRES)
    }

    fn my(sql: &str) -> Completion {
        run(sql, LexRules::MYSQL)
    }

    fn labels(completion: &Completion) -> Vec<&str> {
        completion
            .items
            .iter()
            .map(|item| item.label.as_str())
            .collect()
    }

    /// Asserts the first items of `completion`, in order.
    #[track_caller]
    fn starts(completion: &Completion, expected: &[(&str, CompletionKind)]) {
        let first: Vec<(&str, CompletionKind)> = completion
            .items
            .iter()
            .take(expected.len())
            .map(|item| (item.label.as_str(), item.kind))
            .collect();
        assert_eq!(first, expected);
    }

    #[track_caller]
    fn nothing(completion: &Completion) {
        assert_eq!(labels(completion), Vec::<&str>::new());
    }

    const ORDER_COLUMNS: &[(&str, CompletionKind)] = &[
        ("id", Column),
        ("customer_id", Column),
        ("status", Column),
        ("placed_at", Column),
        ("total", Column),
    ];

    #[test]
    fn statement_start_offers_statement_keywords() {
        for rules in [LexRules::POSTGRES, LexRules::MYSQL] {
            let completion = run("|", rules);
            starts(
                &completion,
                &[
                    ("SELECT", Keyword),
                    ("INSERT", Keyword),
                    ("UPDATE", Keyword),
                    ("DELETE", Keyword),
                    ("WITH", Keyword),
                ],
            );
            assert_eq!(completion.replace, 0..0);
        }
        // Each engine has its own statements.
        assert!(labels(&my("|")).contains(&"DESCRIBE"));
        assert!(!labels(&pg("|")).contains(&"DESCRIBE"));
        assert!(labels(&pg("|")).contains(&"VACUUM"));
        assert!(!labels(&my("|")).contains(&"VACUUM"));
    }

    #[test]
    fn the_word_being_typed_filters_and_is_replaced() {
        let completion = pg("sel|");
        assert_eq!(labels(&completion), ["SELECT"]);
        assert_eq!(completion.replace, 0..3);
        assert_eq!(completion.items[0].insert, "SELECT");
        // The cursor at the very start of a word types a new one.
        let completion = pg("|SELECT 1");
        starts(&completion, &[("SELECT", Keyword), ("INSERT", Keyword)]);
        assert_eq!(completion.replace, 0..0);
    }

    #[test]
    fn the_cursor_inside_a_word_replaces_the_whole_word() {
        let completion = pg("SELECT * FROM cust|omers_old WHERE 1 = 1");
        assert_eq!(labels(&completion), ["customers"]);
        assert_eq!(completion.replace, 14..27);
    }

    #[test]
    fn after_from_come_relations_of_the_search_path_then_schemas() {
        let completion = pg("SELECT * FROM |");
        assert_eq!(
            labels(&completion),
            [
                "customers",
                "categories",
                "products",
                "orders",
                "order_items",
                "reviews",
                "audit_log",
                "order_totals",
                "product_sales",
                "all_types",
                "Order Lines",
                "user",
                "shop",
                "analytics",
                "public",
            ]
        );
        assert_eq!(completion.replace, 14..14);
        let kinds: Vec<CompletionKind> = completion.items.iter().map(|item| item.kind).collect();
        assert_eq!(kinds[0], Table);
        assert_eq!(kinds[7], View);
        assert_eq!(kinds[8], View);
        assert_eq!(kinds[12], Schema);
        assert_eq!(completion.items[0].detail.as_deref(), Some("table in shop"));
        assert_eq!(
            completion.items[8].detail.as_deref(),
            Some("materialized view in shop")
        );
    }

    #[test]
    fn prefix_matches_rank_before_substring_matches() {
        // Case-insensitive prefix, in catalog order.
        let completion = pg("SELECT * FROM ord|");
        assert_eq!(
            labels(&completion),
            ["orders", "order_items", "order_totals", "Order Lines"]
        );
        // "it" starts nothing; it is inside two names.
        let completion = pg("SELECT * FROM it|");
        assert_eq!(labels(&completion), ["order_items", "audit_log"]);
        // A prefix match of a later group still beats a substring match.
        let completion = pg("SELECT * FROM a|");
        starts(
            &completion,
            &[
                ("audit_log", Table),
                ("all_types", Table),
                ("analytics", Schema),
                ("categories", Table),
            ],
        );
    }

    #[test]
    fn names_are_quoted_only_when_they_need_it() {
        let completion = pg("SELECT * FROM |");
        let insert = |label: &str| {
            completion
                .items
                .iter()
                .find(|item| item.label == label)
                .map(|item| item.insert.clone())
        };
        assert_eq!(insert("orders").as_deref(), Some("orders"));
        assert_eq!(insert("Order Lines").as_deref(), Some("\"Order Lines\""));
        // A reserved word.
        assert_eq!(insert("user").as_deref(), Some("\"user\""));

        let completion = my("SELECT * FROM us|");
        starts(&completion, &[("user", Table)]);
        assert_eq!(completion.items[0].insert, "`user`");

        // PostgreSQL folds unquoted names to lower case; MySQL keeps them.
        assert!(needs_quotes("Users", false));
        assert!(!needs_quotes("Users", true));
        assert!(!needs_quotes("order_items2", false));
        assert!(needs_quotes("2fast", false));
        assert!(needs_quotes("with space", true));
        assert!(needs_quotes("select", true));
        assert!(needs_quotes("", false));
    }

    #[test]
    fn after_a_schema_and_a_dot_come_its_relations() {
        let completion = pg("SELECT * FROM shop.|");
        starts(
            &completion,
            &[
                ("customers", Table),
                ("categories", Table),
                ("products", Table),
            ],
        );
        assert_eq!(completion.items.len(), 9);
        assert_eq!(completion.replace, 19..19);

        let completion = pg("SELECT * FROM analytics.ev|");
        assert_eq!(labels(&completion), ["events"]);
        assert_eq!(completion.replace, 24..26);

        // Also after a comma of the FROM list, and after JOIN.
        starts(
            &pg("SELECT * FROM shop.orders, analytics.|"),
            &[("events", Table), ("shipments", Table)],
        );
        starts(
            &pg("SELECT * FROM shop.orders o JOIN analytics.|"),
            &[("events", Table), ("shipments", Table)],
        );
        // A schema nobody knows.
        nothing(&pg("SELECT * FROM nowhere.|"));

        let completion = my("SELECT * FROM `shop`.|");
        starts(&completion, &[("customers", Table)]);
    }

    #[test]
    fn after_an_alias_and_a_dot_come_its_columns() {
        let completion = pg("SELECT * FROM shop.orders o WHERE o.|");
        starts(&completion, ORDER_COLUMNS);
        assert_eq!(completion.items.len(), 5);
        assert_eq!(completion.items[0].detail.as_deref(), Some("bigint"));

        // The alias is defined after the cursor.
        let completion = pg("SELECT o.st| FROM shop.orders o");
        assert_eq!(labels(&completion), ["status", "customer_id"]);
        assert_eq!(completion.replace, 9..11);

        // AS, quoted aliases, quoted tables.
        starts(
            &pg("SELECT * FROM shop.orders AS \"Ord\" WHERE \"Ord\".|"),
            ORDER_COLUMNS,
        );
        let completion = pg("SELECT * FROM \"Order Lines\" ol WHERE ol.|");
        starts(&completion, &[("Line No", Column), ("qty", Column)]);
        assert_eq!(completion.items[0].insert, "\"Line No\"");

        // The table name itself, with or without its schema.
        starts(&pg("SELECT orders.| FROM shop.orders"), ORDER_COLUMNS);
        starts(&pg("SELECT shop.orders.| FROM shop.orders"), ORDER_COLUMNS);
        // A table of the search path the statement does not name yet.
        starts(&pg("SELECT orders.|"), ORDER_COLUMNS);

        let completion = my("SELECT * FROM `orders` `o` WHERE `o`.|");
        starts(&completion, ORDER_COLUMNS);
    }

    #[test]
    fn an_alias_shadows_the_table_of_the_same_name() {
        // `customers` is the orders table here.
        starts(
            &pg("SELECT * FROM shop.orders customers WHERE customers.|"),
            ORDER_COLUMNS,
        );
        // And an aliased table no longer answers to its own name.
        starts(
            &pg("SELECT * FROM shop.orders customers, shop.customers c WHERE c.|"),
            &[("id", Column), ("email", Column)],
        );
    }

    #[test]
    fn columns_of_ctes_and_subqueries_are_unknown() {
        nothing(&pg(
            "WITH recent AS (SELECT * FROM shop.orders) SELECT * FROM recent r WHERE r.|",
        ));
        nothing(&pg(
            "WITH recent AS (SELECT * FROM shop.orders) SELECT recent.| FROM recent",
        ));
        // A CTE hides the table of the same name.
        nothing(&pg(
            "WITH customers AS (SELECT 1 AS x) SELECT customers.| FROM customers",
        ));
        nothing(&pg(
            "SELECT * FROM (SELECT id FROM shop.orders) sub WHERE sub.|",
        ));
        nothing(&pg("SELECT * FROM generate_series(1, 3) g WHERE g.|"));
        // An alias nobody defined.
        nothing(&pg("SELECT * FROM shop.orders o WHERE x.|"));
    }

    #[test]
    fn cte_names_are_offered_as_relations() {
        let completion = pg("WITH recent AS (SELECT 1), old AS (SELECT 2) SELECT * FROM |");
        starts(
            &completion,
            &[("recent", Alias), ("old", Alias), ("customers", Table)],
        );
    }

    #[test]
    fn columns_in_scope_are_offered_in_expressions() {
        let completion = pg("SELECT | FROM shop.orders");
        starts(&completion, ORDER_COLUMNS);
        starts(
            &Completion {
                replace: 0..0,
                items: completion.items[5..].to_vec(),
            },
            &[("orders", Table), ("count", Function)],
        );
        assert_eq!(completion.replace, 7..7);

        for sql in [
            "SELECT * FROM shop.orders WHERE |",
            "SELECT * FROM shop.orders WHERE total > 5 AND |",
            "SELECT * FROM shop.orders GROUP BY |",
            "SELECT * FROM shop.orders ORDER BY |",
            "SELECT * FROM shop.orders ORDER BY total, |",
            "SELECT status FROM shop.orders GROUP BY status HAVING |",
            "UPDATE shop.orders SET |",
            "UPDATE shop.orders SET status = 'x', |",
            "UPDATE shop.orders SET status = 'x' WHERE |",
            "DELETE FROM shop.orders WHERE |",
            "INSERT INTO shop.orders (id) VALUES (1) RETURNING |",
            "INSERT INTO shop.orders (id) VALUES (|",
            "SELECT count(|) FROM shop.orders",
            "SELECT lower(status), | FROM shop.orders",
        ] {
            for rules in [LexRules::POSTGRES, LexRules::MYSQL] {
                let completion = run(sql, rules);
                let first: Vec<(&str, CompletionKind)> = completion
                    .items
                    .iter()
                    .take(5)
                    .map(|item| (item.label.as_str(), item.kind))
                    .collect();
                assert_eq!(first, ORDER_COLUMNS, "{sql}");
            }
        }
    }

    #[test]
    fn a_column_name_shared_by_two_relations_is_qualified() {
        let completion =
            pg("SELECT | FROM shop.orders o JOIN shop.customers c ON c.id = o.customer_id");
        starts(
            &completion,
            &[
                ("o.id", Column),
                ("customer_id", Column),
                ("status", Column),
                ("placed_at", Column),
                ("total", Column),
                ("c.id", Column),
                ("email", Column),
                ("full_name", Column),
                ("phone", Column),
                ("is_active", Column),
                ("created_at", Column),
                ("o", Alias),
                ("c", Alias),
                ("count", Function),
            ],
        );
        assert_eq!(completion.items[0].insert, "o.id");
        assert_eq!(completion.items[11].detail.as_deref(), Some("shop.orders"));

        // The word typed matches the column name, not the qualifier.
        let completion = pg(
            "SELECT * FROM shop.orders o JOIN shop.customers c ON c.id = o.customer_id WHERE i|",
        );
        starts(
            &completion,
            &[("o.id", Column), ("c.id", Column), ("is_active", Column)],
        );
    }

    #[test]
    fn join_proposes_joins_along_foreign_keys_first() {
        let completion = pg("SELECT * FROM shop.orders o JOIN |");
        starts(
            &completion,
            &[
                ("customers ON customers.id = o.customer_id", Join),
                ("order_items ON order_items.order_id = o.id", Join),
                ("customers", Table),
                ("categories", Table),
            ],
        );
        assert_eq!(
            completion.items[0].insert,
            "customers ON customers.id = o.customer_id"
        );
        assert_eq!(
            completion.items[0].detail.as_deref(),
            Some("FK orders(customer_id) → customers(id)")
        );
        assert_eq!(
            completion.items[1].detail.as_deref(),
            Some("FK order_items(order_id) → orders(id)")
        );

        // The word typed filters joins by the joined table.
        let completion = pg("SELECT * FROM shop.orders o LEFT JOIN cus|");
        assert_eq!(
            labels(&completion),
            ["customers ON customers.id = o.customer_id", "customers"]
        );
    }

    #[test]
    fn joins_follow_foreign_keys_in_both_directions() {
        // No alias: the table name qualifies. A schema outside the search
        // path is written.
        let completion = pg("SELECT * FROM shop.customers LEFT JOIN |");
        starts(
            &completion,
            &[
                ("orders ON orders.customer_id = customers.id", Join),
                ("reviews ON reviews.customer_id = customers.id", Join),
                (
                    "analytics.events ON events.customer_id = customers.id",
                    Join,
                ),
                ("customers", Table),
            ],
        );

        // Composite keys.
        let completion = pg("SELECT * FROM shop.order_items i JOIN |");
        starts(
            &completion,
            &[
                ("orders ON orders.id = i.order_id", Join),
                ("products ON products.id = i.product_id", Join),
                (
                    "analytics.shipments ON shipments.order_id = i.order_id AND shipments.line_no = i.line_no",
                    Join,
                ),
                ("customers", Table),
            ],
        );
    }

    #[test]
    fn joins_skip_relations_already_joined() {
        let completion =
            pg("SELECT * FROM shop.orders o JOIN shop.customers c ON c.id = o.customer_id JOIN |");
        starts(
            &completion,
            &[
                ("order_items ON order_items.order_id = o.id", Join),
                ("reviews ON reviews.customer_id = c.id", Join),
                ("analytics.events ON events.customer_id = c.id", Join),
                ("customers", Table),
            ],
        );
    }

    #[test]
    fn a_self_join_goes_both_ways_and_gets_an_alias_when_needed() {
        let completion = pg("SELECT * FROM shop.categories c JOIN |");
        starts(
            &completion,
            &[
                ("categories ON categories.id = c.parent_id", Join),
                ("categories ON categories.parent_id = c.id", Join),
                ("products ON products.category_id = c.id", Join),
                ("customers", Table),
            ],
        );
        // Without an alias the joined table needs one.
        let completion = pg("SELECT * FROM shop.categories JOIN |");
        starts(
            &completion,
            &[(
                "categories categories2 ON categories2.id = categories.parent_id",
                Join,
            )],
        );
    }

    #[test]
    fn on_proposes_the_foreign_key_condition() {
        let completion = pg("SELECT * FROM shop.orders o JOIN shop.customers c ON |");
        starts(
            &completion,
            &[
                ("c.id = o.customer_id", Join),
                ("o.id", Column),
                ("customer_id", Column),
            ],
        );
        assert_eq!(
            completion.items[0].detail.as_deref(),
            Some("FK orders(customer_id) → customers(id)")
        );
        // The key is on the joined table this time.
        let completion = pg("SELECT * FROM shop.customers c JOIN shop.orders o ON |");
        starts(&completion, &[("o.customer_id = c.id", Join)]);
        // Three relations: the condition is about the one just joined.
        let completion = pg(
            "SELECT * FROM shop.customers c JOIN shop.orders o ON o.customer_id = c.id JOIN shop.order_items i ON |",
        );
        starts(
            &completion,
            &[("i.order_id = o.id", Join), ("c.id", Column)],
        );
        // No foreign key between the two: just the columns.
        let completion = pg("SELECT * FROM shop.orders o JOIN shop.audit_log a ON |");
        starts(&completion, &[("id", Column)]);
        // MySQL, backticks.
        let completion = my("SELECT * FROM `orders` o JOIN `customers` c ON |");
        starts(&completion, &[("c.id = o.customer_id", Join)]);
    }

    #[test]
    fn a_subquery_has_its_own_scope_and_sees_outer_aliases() {
        let completion =
            pg("SELECT * FROM shop.customers c WHERE c.id IN (SELECT | FROM shop.orders o)");
        let mut expected = ORDER_COLUMNS.to_vec();
        expected.extend([("o", Alias), ("c", Alias), ("count", Function)]);
        starts(&completion, &expected);

        // A correlated reference to the outer alias.
        let completion = pg(
            "SELECT * FROM shop.customers c WHERE EXISTS (SELECT 1 FROM shop.orders o WHERE o.customer_id = c.|)",
        );
        starts(&completion, &[("id", Column), ("email", Column)]);

        // The inner alias wins over an outer one of the same name.
        let completion = pg(
            "SELECT * FROM shop.customers x WHERE EXISTS (SELECT 1 FROM shop.orders x WHERE x.|)",
        );
        starts(&completion, ORDER_COLUMNS);

        // Inside a CTE body.
        let completion = pg("WITH recent AS (SELECT | FROM shop.orders) SELECT * FROM recent");
        starts(&completion, ORDER_COLUMNS);

        // Each branch of a UNION has its own relations.
        let completion = pg("SELECT id FROM shop.customers UNION SELECT | FROM shop.orders");
        starts(&completion, ORDER_COLUMNS);
    }

    #[test]
    fn an_opening_parenthesis_can_start_a_subquery() {
        let completion = pg("SELECT * FROM shop.orders WHERE id IN (|");
        starts(&completion, &[("SELECT", Keyword), ("id", Column)]);
        let completion = pg("SELECT * FROM (|");
        starts(&completion, &[("SELECT", Keyword)]);
        // Not after a function name.
        let completion = pg("SELECT * FROM shop.orders WHERE lower(|");
        starts(&completion, &[("id", Column)]);
    }

    #[test]
    fn insert_column_lists_offer_the_target_columns_only() {
        let completion = pg("INSERT INTO shop.orders (|");
        assert_eq!(
            labels(&completion),
            ["id", "customer_id", "status", "placed_at", "total"]
        );
        let completion = pg("INSERT INTO shop.orders (id, cu|");
        assert_eq!(labels(&completion), ["customer_id"]);
        let completion = my("INSERT INTO orders (id, |) VALUES (1, 2)");
        starts(&completion, ORDER_COLUMNS);
        assert_eq!(completion.items.len(), 5);
    }

    #[test]
    fn statements_that_name_a_table_offer_relations() {
        for sql in [
            "INSERT INTO |",
            "UPDATE |",
            "DELETE FROM |",
            "TRUNCATE |",
            "TRUNCATE TABLE |",
            "DROP TABLE |",
            "DROP TABLE IF EXISTS |",
            "ALTER TABLE |",
            "SELECT * FROM shop.orders, |",
            "SELECT * FROM shop.orders o JOIN shop.customers c ON c.id = o.customer_id, |",
        ] {
            for rules in [LexRules::POSTGRES, LexRules::MYSQL] {
                starts(
                    &run(sql, rules),
                    &[("customers", Table), ("categories", Table)],
                );
            }
        }
        starts(&my("DESCRIBE |"), &[("customers", Table)]);
        starts(&my("DESC ord|"), &[("orders", Table)]);
        // A new table has no name to complete.
        nothing(&pg("CREATE TABLE |"));
    }

    #[test]
    fn keywords_follow_the_clause() {
        starts(
            &pg("SELECT id, total |"),
            &[("FROM", Keyword), ("AS", Keyword)],
        );
        starts(&pg("SELECT id fr|"), &[("FROM", Keyword)]);
        starts(&pg("SELECT * |"), &[("FROM", Keyword)]);
        starts(
            &pg("SELECT * FROM shop.orders |"),
            &[
                ("WHERE", Keyword),
                ("JOIN", Keyword),
                ("LEFT JOIN", Keyword),
            ],
        );
        starts(
            &pg("SELECT * FROM shop.orders o le|"),
            &[("LEFT JOIN", Keyword)],
        );
        assert_eq!(
            labels(&pg("SELECT * FROM shop.orders o LEFT |")),
            ["JOIN", "OUTER JOIN"]
        );
        starts(
            &pg("SELECT * FROM shop.orders o JOIN shop.customers c |"),
            &[("ON", Keyword), ("USING", Keyword)],
        );
        starts(
            &pg("SELECT * FROM shop.orders WHERE total > 5 |"),
            &[("AND", Keyword), ("OR", Keyword)],
        );
        starts(
            &pg("SELECT * FROM shop.orders o JOIN shop.customers c ON c.id = o.customer_id |"),
            &[("AND", Keyword), ("OR", Keyword), ("WHERE", Keyword)],
        );
        assert_eq!(labels(&pg("SELECT * FROM shop.orders GROUP |")), ["BY"]);
        assert_eq!(labels(&pg("SELECT * FROM shop.orders ORDER |")), ["BY"]);
        starts(
            &pg("SELECT * FROM shop.orders ORDER BY total |"),
            &[("ASC", Keyword), ("DESC", Keyword)],
        );
        starts(
            &pg("SELECT * FROM shop.orders GROUP BY status |"),
            &[("HAVING", Keyword), ("ORDER BY", Keyword)],
        );
        assert_eq!(
            labels(&pg("SELECT * FROM shop.orders LIMIT 5 |")),
            ["OFFSET"]
        );
        assert_eq!(labels(&pg("UPDATE shop.orders |")), ["SET"]);
        starts(
            &pg("UPDATE shop.orders SET total = 0 |"),
            &[("WHERE", Keyword)],
        );
        assert_eq!(
            labels(&pg("INSERT INTO shop.orders |")),
            ["VALUES", "SELECT", "DEFAULT VALUES"]
        );
        assert_eq!(labels(&my("INSERT INTO orders |")), ["VALUES", "SELECT"]);
        assert_eq!(labels(&pg("INSERT |")), ["INTO"]);
        assert_eq!(labels(&pg("DELETE |")), ["FROM"]);
        starts(&pg("CREATE |"), &[("TABLE", Keyword), ("VIEW", Keyword)]);
        starts(&pg("DROP |"), &[("TABLE", Keyword)]);
        starts(&pg("ALTER TABLE shop.orders |"), &[("ADD COLUMN", Keyword)]);
        starts(&pg("EXPLAIN |"), &[("SELECT", Keyword)]);
        starts(&pg("EXPLAIN ANALYZE sel|"), &[("SELECT", Keyword)]);
        assert_eq!(
            labels(&pg("WITH recent AS (SELECT 1) |")),
            ["SELECT", "INSERT", "UPDATE", "DELETE"]
        );
        starts(
            &pg("SELECT * FROM shop.orders WHERE status IS |"),
            &[("NULL", Keyword), ("NOT NULL", Keyword)],
        );
        // PostgreSQL only.
        assert!(labels(&pg("SELECT * FROM shop.orders |")).contains(&"FULL JOIN"));
        assert!(!labels(&my("SELECT * FROM orders |")).contains(&"FULL JOIN"));
        // An alias is being named: nothing to offer.
        nothing(&pg("SELECT id AS |"));
        nothing(&pg("SELECT * FROM shop.orders AS |"));
        nothing(&pg("SELECT * FROM shop.orders LIMIT |"));
    }

    #[test]
    fn case_expressions_offer_their_keywords() {
        starts(&pg("SELECT CASE |"), &[("WHEN", Keyword)]);
        starts(
            &pg("SELECT CASE WHEN total > 5 |"),
            &[
                ("WHEN", Keyword),
                ("THEN", Keyword),
                ("ELSE", Keyword),
                ("END", Keyword),
            ],
        );
        // A finished CASE is an ordinary expression again.
        starts(
            &pg("SELECT CASE WHEN total > 5 THEN 1 END |"),
            &[("FROM", Keyword)],
        );
    }

    #[test]
    fn functions_are_built_in_or_from_the_catalog() {
        let completion = pg("SELECT cou| FROM shop.orders");
        assert_eq!(labels(&completion), ["count"]);
        assert_eq!(completion.items[0].kind, Function);
        assert_eq!(completion.items[0].insert, "count(");
        // The parenthesis is already there.
        let completion = pg("SELECT coun|(id) FROM shop.orders");
        assert_eq!(completion.items[0].insert, "count");

        let completion = pg("SELECT customer_l|");
        starts(&completion, &[("customer_lifetime_value", Function)]);
        assert_eq!(
            completion.items[0].detail.as_deref(),
            Some("function in shop")
        );
        // Qualified: relations and routines of the schema.
        let completion = pg("SELECT shop.cust|");
        assert_eq!(
            labels(&completion),
            ["customers", "customer_lifetime_value"]
        );

        // Each engine has its own.
        assert_eq!(labels(&my("SELECT ifn|")), ["ifnull"]);
        nothing(&pg("SELECT ifn|"));
        assert_eq!(labels(&pg("SELECT string_a|")), ["string_agg"]);
        nothing(&my("SELECT string_a|"));
    }

    #[test]
    fn nothing_inside_strings_and_comments() {
        nothing(&pg("SELECT 'abc| FROM shop.orders"));
        nothing(&pg("SELECT 'abc|' FROM shop.orders"));
        nothing(&pg("SELECT 'abc'| FROM shop.orders"));
        nothing(&pg("SELECT $$ fr|"));
        nothing(&pg("SELECT $tag$ fr| $tag$"));
        nothing(&pg("SELECT 1 -- fr|"));
        nothing(&pg("SELECT 1 --|"));
        nothing(&pg("SELECT /* fr| */ 1"));
        nothing(&pg("SELECT /* fr|"));
        nothing(&pg("-- only a comment fr|"));
        nothing(&pg("SELECT 1; -- fr|"));
        nothing(&my("SELECT 1 # fr|"));
        nothing(&my("SELECT \"ab|"));
        nothing(&my("SELECT 'it\\'s fr|"));
        // Numbers and parameters are not words.
        nothing(&pg("SELECT * FROM shop.orders LIMIT 1|"));
        nothing(&pg("SELECT * FROM shop.orders WHERE id = :na|"));
        // After a finished comment the statement goes on.
        starts(
            &pg("SELECT * FROM /* the big one */ |"),
            &[("customers", Table)],
        );
        starts(
            &pg("SELECT * -- everything\nFROM |"),
            &[("customers", Table)],
        );
    }

    #[test]
    fn quoted_identifiers_complete_only_while_open() {
        nothing(&pg("SELECT * FROM \"Order| Lines\""));
        nothing(&pg("SELECT * FROM \"Order Lines\"|"));

        let completion = pg("SELECT * FROM \"Ord|");
        assert_eq!(
            labels(&completion),
            ["orders", "order_items", "order_totals", "Order Lines"]
        );
        // The quote is part of what gets replaced, and every name is quoted.
        assert_eq!(completion.replace, 14..18);
        assert_eq!(completion.items[0].insert, "\"orders\"");
        assert_eq!(completion.items[3].insert, "\"Order Lines\"");

        let completion = pg("SELECT * FROM \"Order Lines\" ol WHERE ol.\"|");
        assert_eq!(labels(&completion), ["Line No", "qty"]);
        assert_eq!(completion.items[0].insert, "\"Line No\"");

        let completion = my("SELECT * FROM `ord|");
        starts(&completion, &[("orders", Table)]);
        assert_eq!(completion.items[0].insert, "`orders`");
        // No keywords or functions inside quotes.
        nothing(&pg("SELECT \"cou|"));
    }

    #[test]
    fn only_the_statement_under_the_cursor_counts() {
        // `c` is the orders table in the second statement.
        starts(
            &pg("SELECT * FROM shop.customers c; SELECT c.| FROM shop.orders c"),
            ORDER_COLUMNS,
        );
        // `o` belongs to the first statement.
        nothing(&pg("SELECT * FROM shop.orders o; SELECT o.|"));
        nothing(&pg("SELECT o.|; SELECT * FROM shop.orders o"));
        // The first statement is still the one under the cursor.
        starts(
            &pg("SELECT o.| FROM shop.orders o; SELECT * FROM shop.customers o"),
            ORDER_COLUMNS,
        );
        // After the delimiter a new statement starts.
        starts(&pg("SELECT 1; |"), &[("SELECT", Keyword)]);
        starts(&pg("SELECT 1;|"), &[("SELECT", Keyword)]);
        let completion = pg("SELECT 1;\n\nup|\n\nSELECT 2;");
        assert_eq!(labels(&completion), ["UPDATE"]);
        assert_eq!(completion.replace, 11..13);
        // Trailing whitespace still belongs to the statement.
        starts(&pg("SELECT * FROM shop.orders\n  |"), &[("WHERE", Keyword)]);
    }

    #[test]
    fn mysql_delimiter_scripts() {
        let body = "DELIMITER $$\nCREATE PROCEDURE p()\nBEGIN\n  SELECT 1;\n  SELECT * FROM shop.orders o WHERE o.|;\nEND$$\nDELIMITER ;";
        starts(&my(body), ORDER_COLUMNS);
        // Statements of the body are separated by semicolons.
        let body = "DELIMITER $$\nCREATE PROCEDURE p()\nBEGIN\n  SELECT * FROM shop.orders o;\n  UPD|\nEND$$";
        assert_eq!(labels(&my(body)), ["UPDATE"]);
        nothing(&my(
            "DELIMITER $$\nCREATE PROCEDURE p()\nBEGIN\n  SELECT * FROM shop.orders o;\n  SELECT o.|\nEND$$",
        ));
        starts(&my("CREATE PROCEDURE p() BEGIN |"), &[("SELECT", Keyword)]);
        // The DELIMITER line is not SQL.
        nothing(&my("DELIMITER |"));
        nothing(&my("SELECT 1;\nDELIMITER $$|"));
        starts(&my("DELIMITER $$\n|"), &[("SELECT", Keyword)]);
    }

    #[test]
    fn garbage_never_panics() {
        let inputs = [
            "|",
            " | ",
            ".|",
            "..|",
            "|.",
            "a.b.c.d.|",
            "SELECT * FROM a.b.c.d.|",
            "((((|",
            "))))|",
            ")))) from ,, where ( select |",
            "select from where group by order by |",
            "FROM FROM FROM |",
            "JOIN |",
            "ON |",
            "select * from (select * from (select * from |",
            "with as ( as ( |",
            "with x as |",
            "with x (a, b) as (select 1), |",
            ", , , |",
            "; ; ; |",
            "select 'unterminated |",
            "select \"unterminated |",
            "select /* unterminated |",
            "select $q$ unterminated |",
            "sélect ☃ from ☃.|",
            "select * from shop.orders o where o.| = = = )))",
            "insert into (|",
            "insert into shop.orders (|) values",
            "update set |",
            "select case case case end |",
            "select * from shop.orders join join join |",
            "select * from shop.orders on on |",
            "select 1 union union |",
            "select * from shop.orders as as |",
            "select [|] from {|}",
            "\u{0}|\u{0}",
        ];
        for sql in inputs {
            for rules in [LexRules::POSTGRES, LexRules::MYSQL] {
                let completion = run(sql, rules);
                assert!(completion.items.len() <= MAX_ITEMS, "{sql}");
                assert!(completion.replace.start <= completion.replace.end, "{sql}");
            }
        }
        // Every cursor position of a text, including inside multi-byte characters.
        let text = "WITH é AS (SELECT 'ü' FROM \"Ö\".t x JOIN shop.orders o ON o.id = x.id) SELECT o.id, ☃ FROM é; -- fin\nSELECT 1";
        let catalog = catalog();
        for cursor in 0..text.len() + 3 {
            let completion = complete(text, cursor, &catalog, LexRules::POSTGRES, &pg_quote);
            assert!(completion.replace.end <= text.len());
            assert!(text.is_char_boundary(completion.replace.start));
            assert!(text.is_char_boundary(completion.replace.end));
            let completion = complete(text, cursor, &catalog, LexRules::MYSQL, &my_quote);
            assert!(completion.replace.end <= text.len());
        }
        // An empty catalog offers keywords and functions only.
        let empty = Catalog::default();
        let completion = complete("SELECT * FROM ", 14, &empty, LexRules::POSTGRES, &pg_quote);
        assert!(completion.items.is_empty());
    }

    /// Random sequences of SQL pieces, completed at many cursor positions.
    /// The generator is a fixed linear congruential one, so a failure repeats.
    #[test]
    fn random_token_soup_never_panics() {
        const PIECES: &[&str] = &[
            "select",
            "from",
            "where",
            "and",
            "join",
            "left",
            "on",
            "group",
            "by",
            "order",
            "case",
            "when",
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
            "create",
            "table",
            "in",
            "is",
            "not",
            "limit",
            "using",
            "begin",
            "delimiter",
            "shop",
            "orders",
            "customers",
            "o",
            "c",
            "\"Order Lines\"",
            "`user`",
            "é",
            "1",
            "'s'",
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
            ".",
            ";",
            "::",
            "=",
            "*",
            "-",
            "$",
            "\"",
            "`",
            "'",
            "-- c\n",
            "# h\n",
            "/* b */",
            "/*",
            " ",
            " ",
            " ",
            "\n",
            "",
        ];
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = |bound: usize| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as usize % bound
        };
        let catalog = catalog();
        for _ in 0..300 {
            let mut text = String::new();
            for _ in 0..next(30) + 1 {
                text.push_str(PIECES[next(PIECES.len())]);
                if next(3) > 0 {
                    text.push(' ');
                }
            }
            for cursor in (0..=text.len()).step_by(2) {
                for (rules, quote) in [
                    (LexRules::POSTGRES, &pg_quote as &dyn Fn(&str) -> String),
                    (LexRules::MYSQL, &my_quote),
                ] {
                    let completion = complete(&text, cursor, &catalog, rules, quote);
                    assert!(completion.items.len() <= MAX_ITEMS, "{text:?}");
                    assert!(
                        completion.replace.start <= completion.replace.end,
                        "{text:?}"
                    );
                    assert!(completion.replace.end <= text.len(), "{text:?}");
                    assert!(text.is_char_boundary(completion.replace.start), "{text:?}");
                    assert!(text.is_char_boundary(completion.replace.end), "{text:?}");
                }
            }
        }
    }

    #[test]
    fn at_most_fifty_items() {
        let catalog = big_catalog();
        let completion = complete(
            "SELECT * FROM ",
            14,
            &catalog,
            LexRules::POSTGRES,
            &pg_quote,
        );
        assert_eq!(completion.items.len(), MAX_ITEMS);
        assert_eq!(completion.items[0].label, "table_0000");
        // Prefix matches fill the list before any substring match.
        let completion = complete(
            "SELECT * FROM table_19",
            22,
            &catalog,
            LexRules::POSTGRES,
            &pg_quote,
        );
        assert_eq!(completion.items.len(), MAX_ITEMS);
        assert_eq!(completion.items[0].label, "table_1900");
        assert_eq!(completion.items[49].label, "table_1949");
    }

    /// 2,000 tables of 20 columns, each with a foreign key to the next.
    fn big_catalog() -> Catalog {
        let relations = (0..2000)
            .map(|index| CatalogRelation {
                name: format!("table_{index:04}"),
                kind: RelationKind::Table,
                columns: (0..20)
                    .map(|column| CatalogColumn {
                        name: format!("col_{column:02}"),
                        data_type: "integer".into(),
                    })
                    .collect(),
                foreign_keys: vec![CatalogForeignKey {
                    columns: vec!["col_01".into()],
                    referenced_schema: "big".into(),
                    referenced_table: format!("table_{:04}", (index + 1) % 2000),
                    referenced_columns: vec!["col_00".into()],
                }],
            })
            .collect();
        Catalog {
            schemas: vec![CatalogSchema {
                name: "big".into(),
                relations,
                routines: (0..200)
                    .map(|index| format!("routine_{index:03}"))
                    .collect(),
            }],
            search_path: vec!["big".into()],
        }
    }

    /// C2: completion on 2,000 tables and a 2 kB statement stays under a loose bound. Run
    /// with `--nocapture` to see the timings.
    #[test]
    fn a_large_catalog_and_a_long_statement_stay_fast() {
        let catalog = big_catalog();
        // A statement of about 2 kB: a long select list over five joined tables.
        let mut statement = String::from("SELECT ");
        for index in 0..110 {
            statement.push_str(&format!(
                "t{}.col_{:02} AS c{index}, ",
                index % 5,
                index % 20
            ));
        }
        statement.push_str("count(*)\nFROM big.table_0000 t0\n");
        for index in 1..5 {
            statement.push_str(&format!(
                "JOIN big.table_{index:04} t{index} ON t{index}.col_00 = t{}.col_01\n",
                index - 1
            ));
        }
        assert!(statement.len() > 2000, "{}", statement.len());

        let cases = [
            ("WHERE |", "t0.col_00", "columns in scope"),
            ("WHERE t3.|", "col_00", "columns of an alias"),
            (
                "JOIN |",
                "table_0005 ON table_0005.col_00 = t4.col_01",
                "joins",
            ),
            (
                "JOIN table_19|",
                "table_1999 ON table_1999.col_01 = t0.col_00",
                "relations by prefix",
            ),
            ("JOIN zzz|", "", "no match at all"),
            ("WHERE rout|", "routine_000", "routines"),
        ];
        for (tail, expected, what) in cases {
            let sql = format!("{statement}{tail}");
            let cursor = sql.find('|').expect("cursor");
            let text = sql.replace('|', "");
            let rounds = 200;
            let started = std::time::Instant::now();
            let mut completion = complete(&text, cursor, &catalog, LexRules::POSTGRES, &pg_quote);
            for _ in 1..rounds {
                completion = complete(&text, cursor, &catalog, LexRules::POSTGRES, &pg_quote);
            }
            let each = started.elapsed() / rounds;
            eprintln!("complete, {what}: {each:?} per call");
            // C2: 0.3 to 1.9 ms a call in a debug build (2026-11-19, load 5 to 11). The
            // bound catches a scan of the catalog per candidate (45 ms), not machine noise.
            assert!(
                each < std::time::Duration::from_millis(20),
                "{what}: {each:?} per call"
            );
            let first = completion
                .items
                .first()
                .map_or("", |item| item.label.as_str());
            assert_eq!(first, expected, "{what}");
        }
    }
}
