//! Browsing a table page by page: the filter, the `SELECT` of one page and
//! the count of the rows the filter keeps.
//!
//! Values are written as literals by the dialect, never pasted: a value
//! typed in the filter bar cannot change the shape of the statement.

use crate::config::Engine;
use crate::dialect::Dialect;
use crate::error::{Error, Result};
use crate::result::{Column, TypeKind};
use crate::schema::{ColumnInfo, TableRef};
use crate::sql::{CellValue, StatementKind, TokenKind, classify, lex, split_statements};

/// How a condition compares its column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    Equals,
    NotEquals,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
    /// The value appears somewhere in the column's text.
    Contains,
    /// `LIKE` with the user's own `%` and `_`.
    Like,
    NotLike,
    /// One of a comma-separated list.
    In,
    IsNull,
    IsNotNull,
}

impl Operator {
    /// Every operator, in the order the filter bar lists them.
    pub const ALL: [Operator; 12] = [
        Operator::Equals,
        Operator::NotEquals,
        Operator::Less,
        Operator::LessOrEqual,
        Operator::Greater,
        Operator::GreaterOrEqual,
        Operator::Contains,
        Operator::Like,
        Operator::NotLike,
        Operator::In,
        Operator::IsNull,
        Operator::IsNotNull,
    ];

    /// What the filter bar calls it.
    pub fn label(self) -> &'static str {
        match self {
            Operator::Equals => "=",
            Operator::NotEquals => "<>",
            Operator::Less => "<",
            Operator::LessOrEqual => "<=",
            Operator::Greater => ">",
            Operator::GreaterOrEqual => ">=",
            Operator::Contains => "contains",
            Operator::Like => "LIKE",
            Operator::NotLike => "NOT LIKE",
            Operator::In => "IN",
            Operator::IsNull => "IS NULL",
            Operator::IsNotNull => "IS NOT NULL",
        }
    }

    /// Whether the condition needs a value.
    pub fn takes_value(self) -> bool {
        !matches!(self, Operator::IsNull | Operator::IsNotNull)
    }
}

/// One line of the filter bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Condition {
    pub column: String,
    pub operator: Operator,
    /// As typed. Ignored by operators that take no value.
    pub value: String,
}

impl Condition {
    /// `column = value`.
    pub fn equals(column: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            operator: Operator::Equals,
            value: value.into(),
        }
    }
}

/// Which rows of a table are shown: every condition must hold, and the raw
/// `WHERE` text too when there is one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    pub conditions: Vec<Condition>,
    /// SQL typed by the user, as the body of a `WHERE`.
    pub raw: String,
}

impl Filter {
    /// Whether every row passes.
    pub fn is_empty(&self) -> bool {
        self.conditions.is_empty() && self.raw.trim().is_empty()
    }

    /// The body of the `WHERE` clause, or `None` when every row passes.
    pub fn where_sql(
        &self,
        dialect: &dyn Dialect,
        columns: &[ColumnInfo],
    ) -> Result<Option<String>> {
        let mut parts = Vec::with_capacity(self.conditions.len() + 1);
        for condition in &self.conditions {
            parts.push(condition_sql(dialect, columns, condition)?);
        }
        let raw = self.raw.trim();
        if !raw.is_empty() {
            // One statement goes to the server, whatever is typed here.
            if lex(raw, dialect.lex_rules())
                .iter()
                .any(|token| token.kind == TokenKind::Semicolon)
            {
                return Err(Error::Config(
                    "the filter is one condition: remove the \";\"".into(),
                ));
            }
            // On lines of its own, so that a trailing `--` comment ends
            // with the line and not with the statement.
            parts.push(format!("(\n{raw}\n)"));
        }
        Ok((!parts.is_empty()).then(|| parts.join(" AND ")))
    }
}

/// Escapes what `LIKE` reads as a wildcard, with `!` as the escape.
fn like_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '%' | '_' | '!') {
            out.push('!');
        }
        out.push(c);
    }
    out
}

fn condition_sql(
    dialect: &dyn Dialect,
    columns: &[ColumnInfo],
    condition: &Condition,
) -> Result<String> {
    let column = columns
        .iter()
        .find(|column| column.name == condition.column)
        .ok_or_else(|| Error::Config(format!("no column \"{}\" to filter on", condition.column)))?;
    let ident = dialect.quote_ident(&column.name);
    let literal = |text: &str| dialect.literal(&CellValue::Text(text.to_string()), column);
    // Patterns match the column's text, whatever its type.
    let as_text = || {
        if matches!(column.kind, TypeKind::Text) {
            ident.clone()
        } else {
            dialect.cast_to_text(&ident)
        }
    };
    let compare = |operator: &str| format!("{ident} {operator} {}", literal(&condition.value));
    Ok(match condition.operator {
        Operator::Equals => compare("="),
        Operator::NotEquals => compare("<>"),
        Operator::Less => compare("<"),
        Operator::LessOrEqual => compare("<="),
        Operator::Greater => compare(">"),
        Operator::GreaterOrEqual => compare(">="),
        Operator::Contains => format!(
            // `!` escapes the same way on every engine; a backslash does not.
            "{} LIKE {} ESCAPE '!'",
            as_text(),
            dialect.quote_string(&format!("%{}%", like_escape(&condition.value)))
        ),
        Operator::Like => format!(
            "{} LIKE {}",
            as_text(),
            dialect.quote_string(&condition.value)
        ),
        Operator::NotLike => format!(
            "{} NOT LIKE {}",
            as_text(),
            dialect.quote_string(&condition.value)
        ),
        Operator::In => {
            let values: Vec<String> = condition
                .value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(literal)
                .collect();
            if values.is_empty() {
                return Err(Error::Config(format!(
                    "give \"{}\" at least one value for IN, separated by commas",
                    column.name
                )));
            }
            format!("{ident} IN ({})", values.join(", "))
        }
        Operator::IsNull => format!("{ident} IS NULL"),
        Operator::IsNotNull => format!("{ident} IS NOT NULL"),
    })
}

/// One page of a table.
#[derive(Debug, Clone, Copy)]
pub struct Page<'a> {
    pub table: &'a TableRef,
    pub filter: &'a Filter,
    /// The column sorted on and whether descending.
    pub sort: Option<(&'a str, bool)>,
    /// The columns that identify a row, when the table has some. They end
    /// the `ORDER BY`, so that pages do not overlap or skip rows.
    pub identity: &'a [String],
    pub limit: u64,
    pub offset: u64,
}

/// The `SELECT` that reads `page`: [`select_sql`] and its limit.
pub fn page_sql(dialect: &dyn Dialect, columns: &[ColumnInfo], page: Page<'_>) -> Result<String> {
    let mut sql = select_sql(
        dialect,
        columns,
        page.table,
        page.filter,
        page.sort,
        page.identity,
    )?;
    sql.push('\n');
    sql.push_str(&dialect.limit_offset(page.limit, page.offset));
    Ok(sql)
}

/// The `SELECT` that reads every row of `table` that `filter` keeps, in
/// the order of its pages (see [`Page`]), without a limit.
pub fn select_sql(
    dialect: &dyn Dialect,
    columns: &[ColumnInfo],
    table: &TableRef,
    filter: &Filter,
    sort: Option<(&str, bool)>,
    identity: &[String],
) -> Result<String> {
    let mut sql = format!("SELECT * FROM {}", dialect.table_name(table));
    if let Some(condition) = filter.where_sql(dialect, columns)? {
        sql.push_str("\nWHERE ");
        sql.push_str(&condition);
    }
    let mut order = Vec::with_capacity(identity.len() + 1);
    if let Some((column, descending)) = sort {
        let ident = dialect.quote_ident(column);
        order.push(if descending {
            format!("{ident} DESC")
        } else {
            ident
        });
    }
    order.extend(
        identity
            .iter()
            .filter(|column| sort.is_none_or(|(sorted, _)| sorted != column.as_str()))
            .map(|column| dialect.quote_ident(column)),
    );
    if !order.is_empty() {
        sql.push_str("\nORDER BY ");
        sql.push_str(&order.join(", "));
    }
    Ok(sql)
}

/// The statement that counts the rows `filter` keeps.
pub fn count_sql(
    dialect: &dyn Dialect,
    columns: &[ColumnInfo],
    table: &TableRef,
    filter: &Filter,
) -> Result<String> {
    count_from(dialect, Source::Table(table), columns, filter)
}

/// Where rows are read from: a table, or the result of a read statement
/// that the server treats as a derived table.
#[derive(Debug, Clone, Copy)]
pub enum Source<'a> {
    Table(&'a TableRef),
    Query(&'a Query),
}

/// The alias of a statement read as a derived table: PostgreSQL before 16
/// and MySQL want one, and both take it without `AS`.
pub const QUERY_ALIAS: &str = "kerno_q";

/// One read statement whose result the server can filter, order and
/// count by reading it as a derived table, as DBeaver does when it cannot
/// patch the statement (`StandardSQLDialectQueryGenerator
/// .getWrappedFilterQuery`, `SQLQueryTransformerCount`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    /// The statement without its `;`.
    text: String,
    /// The names more than one column of the result carries.
    duplicates: Vec<String>,
    /// It ends with its own `ORDER BY` and no `LIMIT`.
    own_order: bool,
}

impl Query {
    /// `statement`, whose result has columns named `columns` (empty when
    /// not known yet), when the server can read it as a derived table.
    /// Otherwise the error says why, in words for the user.
    pub fn new(dialect: &dyn Dialect, statement: &str, columns: &[String]) -> Result<Self> {
        let refuse = |reason: &str| Err(Error::Config(reason.to_string()));
        let rules = dialect.lex_rules();
        let spans = split_statements(statement, rules);
        let text = match spans.as_slice() {
            [] => return refuse("there is no statement"),
            [one] => one.text(statement),
            _ => return refuse("it holds several statements: run one SELECT"),
        };
        let class = classify(text, rules);
        if class.kind != StatementKind::Select {
            return refuse(
                "only the result of one SELECT can be filtered, ordered or counted on the server",
            );
        }
        if class.writes {
            return refuse("the statement writes (INTO, or a CTE that changes rows)");
        }
        let words = top_words(text, rules);
        if locks_rows(&words) {
            return refuse("the statement locks rows (FOR UPDATE, FOR SHARE, LOCK IN SHARE MODE)");
        }
        let own_order = words.windows(2).any(|pair| pair == ["ORDER", "BY"])
            && !words.iter().any(|word| word == "LIMIT" || word == "FETCH");
        let mut duplicates: Vec<String> = Vec::new();
        for (index, name) in columns.iter().enumerate() {
            if columns[..index].contains(name) && !duplicates.contains(name) {
                duplicates.push(name.clone());
            }
        }
        // MySQL and MariaDB refuse a derived table with two columns of one
        // name (error 1060), whatever the outer query names.
        if let Some(name) = duplicates.first()
            && dialect.engine() != Engine::Postgres
        {
            return Err(Error::Config(format!(
                "the result has more than one column named \"{name}\", which {} refuses in a \
                 derived table: give them different names with AS",
                dialect.engine().label()
            )));
        }
        Ok(Self {
            text: text.to_string(),
            duplicates,
            own_order,
        })
    }

    /// Whether the server ignores the statement's own `ORDER BY` once it
    /// is a derived table: MySQL and MariaDB do without a `LIMIT`, so a
    /// filtered result comes back in no given order.
    pub fn drops_own_order(&self, dialect: &dyn Dialect) -> bool {
        self.own_order && dialect.engine() != Engine::Postgres
    }

    /// The statement, without its `;`.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Refuses a column that names more than one column of the result:
    /// the server could not tell which one is meant.
    fn check(&self, column: &str) -> Result<()> {
        if self.duplicates.iter().any(|name| name == column) {
            return Err(Error::Config(format!(
                "the result has more than one column named \"{column}\": give them different \
                 names with AS"
            )));
        }
        Ok(())
    }
}

/// Whether a `SELECT` ends with a locking clause: reading it again would
/// lock the rows again, inside a filter's or a count's statement.
fn locks_rows(words: &[String]) -> bool {
    words.windows(2).any(|pair| {
        pair[0] == "FOR" && matches!(pair[1].as_str(), "UPDATE" | "SHARE" | "NO" | "KEY")
    }) || words
        .windows(4)
        .any(|four| four == ["LOCK", "IN", "SHARE", "MODE"])
}

/// The words of a statement outside parentheses, in upper case; anything
/// else outside them is an empty word, so that only neighbours pair up.
fn top_words(statement: &str, rules: crate::sql::LexRules) -> Vec<String> {
    let mut depth = 0usize;
    let mut words: Vec<String> = Vec::new();
    for token in lex(statement, rules) {
        let text = &statement[token.range];
        match token.kind {
            TokenKind::Punct if text == "(" => depth += 1,
            TokenKind::Punct if text == ")" => depth = depth.saturating_sub(1),
            TokenKind::Word if depth == 0 => words.push(text.to_ascii_uppercase()),
            kind if kind.is_trivia() => {}
            _ if depth == 0 => words.push(String::new()),
            _ => {}
        }
    }
    words
}

/// What `FROM` reads for `source`.
fn from_sql(dialect: &dyn Dialect, source: Source<'_>) -> String {
    match source {
        Source::Table(table) => dialect.table_name(table),
        // On lines of its own, so that a trailing `--` comment ends with
        // its line and not with the parenthesis.
        Source::Query(query) => format!("(\n{}\n) {QUERY_ALIAS}", query.text),
    }
}

/// `FROM <source> [WHERE <filter>]`.
fn from_where(
    dialect: &dyn Dialect,
    source: Source<'_>,
    columns: &[ColumnInfo],
    filter: &Filter,
) -> Result<String> {
    if let Source::Query(query) = source {
        for condition in &filter.conditions {
            query.check(&condition.column)?;
        }
    }
    let mut sql = format!("FROM {}", from_sql(dialect, source));
    if let Some(condition) = filter.where_sql(dialect, columns)? {
        sql.push_str("\nWHERE ");
        sql.push_str(&condition);
    }
    Ok(sql)
}

/// The rows of `source` that `filter` keeps, ordered by `sort` (a column
/// and whether descending), or in the source's own order without one. A
/// statement keeps its own `ORDER BY` and `LIMIT` inside.
pub fn select_from(
    dialect: &dyn Dialect,
    source: Source<'_>,
    columns: &[ColumnInfo],
    filter: &Filter,
    sort: Option<(&str, bool)>,
) -> Result<String> {
    let mut sql = format!("SELECT * {}", from_where(dialect, source, columns, filter)?);
    if let Some((column, descending)) = sort {
        if let Source::Query(query) = source {
            query.check(column)?;
        }
        sql.push_str("\nORDER BY ");
        sql.push_str(&dialect.quote_ident(column));
        if descending {
            sql.push_str(" DESC");
        }
    }
    Ok(sql)
}

/// The statement that counts the rows of `source` that `filter` keeps.
pub fn count_from(
    dialect: &dyn Dialect,
    source: Source<'_>,
    columns: &[ColumnInfo],
    filter: &Filter,
) -> Result<String> {
    Ok(format!(
        "SELECT count(*) {}",
        from_where(dialect, source, columns, filter)?
    ))
}

/// The distinct values of `column` among the rows of `source` that
/// `filter` keeps, with how many rows hold each, the most frequent first,
/// at most `limit` of them (DBeaver's "Filter by distinct values",
/// `valuefilter/`). NULL is one of the values.
pub fn distinct_sql(
    dialect: &dyn Dialect,
    source: Source<'_>,
    columns: &[ColumnInfo],
    filter: &Filter,
    column: &str,
    limit: u64,
) -> Result<String> {
    if let Source::Query(query) = source {
        query.check(column)?;
    }
    let ident = dialect.quote_ident(column);
    Ok(format!(
        "SELECT {ident}, count(*) {}\nGROUP BY {ident}\nORDER BY 2 DESC, 1\n{}",
        from_where(dialect, source, columns, filter)?,
        dialect.limit_offset(limit, 0)
    ))
}

/// The condition that keeps the rows whose `column` holds one of `values`
/// (`None` is NULL), values as literals: what picking distinct values
/// filters on. `None` when nothing is picked.
pub fn one_of_sql(
    dialect: &dyn Dialect,
    column: &ColumnInfo,
    values: &[Option<String>],
) -> Option<String> {
    let ident = dialect.quote_ident(&column.name);
    let literals: Vec<String> = values
        .iter()
        .flatten()
        .map(|value| dialect.literal(&CellValue::Text(value.clone()), column))
        .collect();
    let in_list = (!literals.is_empty()).then(|| format!("{ident} IN ({})", literals.join(", ")));
    let null = values
        .iter()
        .any(Option::is_none)
        .then(|| format!("{ident} IS NULL"));
    match (in_list, null) {
        (Some(in_list), Some(null)) => Some(format!("({in_list} OR {null})")),
        (one, other) => one.or(other),
    }
}

/// The body of a `WHERE` that keeps what `existing` keeps and `condition`
/// too.
pub fn and_where(existing: &str, condition: &str) -> String {
    let existing = existing.trim();
    if existing.is_empty() {
        condition.to_string()
    } else {
        format!("({existing}) AND {condition}")
    }
}

/// What a filter needs to know of the columns of a query's result: their
/// names and kinds, every one taking NULL.
pub fn described(columns: &[Column]) -> Vec<ColumnInfo> {
    columns
        .iter()
        .map(|column| ColumnInfo {
            name: column.name.clone(),
            data_type: column.type_name.clone(),
            kind: column.kind,
            nullable: true,
            default: None,
            generation: None,
            on_update: None,
            invisible: false,
            primary_key_position: None,
            comment: None,
            collation: None,
            enum_values: Vec::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::test_support::AnsiDialect;

    fn col(name: &str, kind: TypeKind) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: String::new(),
            kind,
            nullable: true,
            default: None,
            generation: None,
            on_update: None,
            invisible: false,
            primary_key_position: None,
            comment: None,
            collation: None,
            enum_values: Vec::new(),
        }
    }

    fn columns() -> Vec<ColumnInfo> {
        vec![
            col("id", TypeKind::Integer),
            col("full name", TypeKind::Text),
            col("born", TypeKind::Date),
        ]
    }

    fn condition(column: &str, operator: Operator, value: &str) -> Condition {
        Condition {
            column: column.into(),
            operator,
            value: value.into(),
        }
    }

    fn where_of(conditions: Vec<Condition>, raw: &str) -> Result<Option<String>> {
        Filter {
            conditions,
            raw: raw.into(),
        }
        .where_sql(&AnsiDialect, &columns())
    }

    #[test]
    fn conditions_are_written_with_literals() {
        use Operator::*;
        let one = |operator, column: &str, value: &str| {
            where_of(vec![condition(column, operator, value)], "")
                .unwrap()
                .unwrap()
        };
        assert_eq!(one(Equals, "id", "42"), "\"id\" = 42");
        // What is not a number is left to the server, as a string.
        assert_eq!(one(Equals, "id", "4 or 1=1"), "\"id\" = '4 or 1=1'");
        assert_eq!(
            one(NotEquals, "full name", "O'Neil"),
            "\"full name\" <> 'O''Neil'"
        );
        assert_eq!(one(Less, "born", "2000-01-01"), "\"born\" < '2000-01-01'");
        assert_eq!(one(LessOrEqual, "id", "7"), "\"id\" <= 7");
        assert_eq!(one(Greater, "id", "7"), "\"id\" > 7");
        assert_eq!(one(GreaterOrEqual, "id", "7"), "\"id\" >= 7");
        assert_eq!(one(IsNull, "born", "ignored"), "\"born\" IS NULL");
        assert_eq!(one(IsNotNull, "born", ""), "\"born\" IS NOT NULL");
        assert_eq!(one(In, "id", "1, 2,,3 "), "\"id\" IN (1, 2, 3)");
        assert_eq!(one(In, "full name", "a,b"), "\"full name\" IN ('a', 'b')");
    }

    #[test]
    fn patterns_match_the_text_of_any_column() {
        use Operator::*;
        let one = |operator, column: &str, value: &str| {
            where_of(vec![condition(column, operator, value)], "")
                .unwrap()
                .unwrap()
        };
        // The user's own wildcards.
        assert_eq!(one(Like, "full name", "A%"), "\"full name\" LIKE 'A%'");
        assert_eq!(
            one(NotLike, "full name", "A_"),
            "\"full name\" NOT LIKE 'A_'"
        );
        // "contains" looks for the text as it is.
        assert_eq!(
            one(Contains, "full name", "50%_!"),
            "\"full name\" LIKE '%50!%!_!!%' ESCAPE '!'"
        );
        // Other types are compared as text.
        assert_eq!(
            one(Contains, "id", "4"),
            "CAST(\"id\" AS CHAR) LIKE '%4%' ESCAPE '!'"
        );
        assert_eq!(
            one(Like, "born", "19%"),
            "CAST(\"born\" AS CHAR) LIKE '19%'"
        );
    }

    #[test]
    fn conditions_and_the_raw_text_must_all_hold() {
        assert_eq!(where_of(vec![], "  ").unwrap(), None);
        assert!(Filter::default().is_empty());
        assert_eq!(
            where_of(
                vec![
                    Condition::equals("id", "1"),
                    condition("born", Operator::IsNull, ""),
                ],
                " id > 5 or id < 2 -- both ends "
            )
            .unwrap()
            .unwrap(),
            "\"id\" = 1 AND \"born\" IS NULL AND (\nid > 5 or id < 2 -- both ends\n)"
        );
    }

    #[test]
    fn what_cannot_be_a_filter_is_refused() {
        let refused = |filter: Result<Option<String>>| filter.unwrap_err().to_string();
        assert!(refused(where_of(vec![], "1=1; drop table t")).contains("remove the \";\""));
        // A semicolon inside a string is a character like another.
        assert!(where_of(vec![], "\"full name\" = 'a;b'").is_ok());
        assert!(refused(where_of(vec![Condition::equals("nope", "1")], "")).contains("no column"));
        assert!(
            refused(where_of(vec![condition("id", Operator::In, " , ")], ""))
                .contains("at least one value")
        );
    }

    #[test]
    fn a_page_is_sorted_down_to_the_row_identity() {
        let table = TableRef::new("shop", "people");
        let identity = vec!["id".to_string()];
        let filter = Filter {
            conditions: vec![Condition::equals("id", "3")],
            raw: String::new(),
        };
        let page = Page {
            table: &table,
            filter: &filter,
            sort: Some(("full name", true)),
            identity: &identity,
            limit: 100,
            offset: 200,
        };
        assert_eq!(
            page_sql(&AnsiDialect, &columns(), page).unwrap(),
            "SELECT * FROM \"shop\".\"people\"\nWHERE \"id\" = 3\nORDER BY \"full name\" DESC, \"id\"\nLIMIT 100 OFFSET 200"
        );
        // Sorting on the identity itself does not repeat it.
        let none = Filter::default();
        let page = Page {
            filter: &none,
            sort: Some(("id", false)),
            offset: 0,
            ..page
        };
        assert_eq!(
            page_sql(&AnsiDialect, &columns(), page).unwrap(),
            "SELECT * FROM \"shop\".\"people\"\nORDER BY \"id\"\nLIMIT 100"
        );
        // A table without a key is read in the server's order.
        let page = Page {
            sort: None,
            identity: &[],
            ..page
        };
        assert_eq!(
            page_sql(&AnsiDialect, &columns(), page).unwrap(),
            "SELECT * FROM \"shop\".\"people\"\nLIMIT 100"
        );
        // Every row, in the same order, without the limit.
        assert_eq!(
            select_sql(
                &AnsiDialect,
                &columns(),
                &table,
                &filter,
                Some(("full name", true)),
                &identity
            )
            .unwrap(),
            "SELECT * FROM \"shop\".\"people\"\nWHERE \"id\" = 3\nORDER BY \"full name\" DESC, \"id\""
        );
        assert_eq!(
            select_sql(&AnsiDialect, &columns(), &table, &none, None, &[]).unwrap(),
            "SELECT * FROM \"shop\".\"people\""
        );
        assert_eq!(
            count_sql(&AnsiDialect, &columns(), &table, &filter).unwrap(),
            "SELECT count(*) FROM \"shop\".\"people\"\nWHERE \"id\" = 3"
        );
        assert_eq!(
            count_sql(&AnsiDialect, &columns(), &table, &none).unwrap(),
            "SELECT count(*) FROM \"shop\".\"people\""
        );
    }

    /// PostgreSQL's quoting and lexing, or MySQL's and MariaDB's.
    struct Engined(Engine);

    impl Dialect for Engined {
        fn engine(&self) -> Engine {
            self.0
        }
        fn has_schemas(&self) -> bool {
            self.0 == Engine::Postgres
        }
        fn lex_rules(&self) -> crate::sql::LexRules {
            match self.0 {
                Engine::Postgres => crate::sql::LexRules::POSTGRES,
                _ => crate::sql::LexRules::MYSQL,
            }
        }
        fn quote_ident(&self, ident: &str) -> String {
            match self.0 {
                Engine::Postgres => format!("\"{}\"", ident.replace('"', "\"\"")),
                _ => format!("`{}`", ident.replace('`', "``")),
            }
        }
        fn quote_string(&self, text: &str) -> String {
            format!("'{}'", text.replace('\'', "''"))
        }
        fn quote_bytes(&self, bytes: &[u8]) -> String {
            format!("X'{}'", crate::dialect::hex(bytes))
        }
        fn explain(&self, sql: &str, _: bool) -> String {
            format!("EXPLAIN {sql}")
        }
        fn type_for(&self, _: TypeKind) -> &'static str {
            "text"
        }
        fn estimated_rows_sql(&self, _: &TableRef) -> String {
            String::new()
        }
    }

    const PG: Engined = Engined(Engine::Postgres);
    const ENGINES: [Engined; 3] = [
        Engined(Engine::Postgres),
        Engined(Engine::MySql),
        Engined(Engine::MariaDb),
    ];

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    fn query(dialect: &Engined, statement: &str) -> Query {
        Query::new(dialect, statement, &names(&["id", "status"])).unwrap()
    }

    fn refused(dialect: &Engined, statement: &str, columns: &[&str]) -> String {
        Query::new(dialect, statement, &names(columns))
            .unwrap_err()
            .to_string()
    }

    fn status_is(value: &str) -> Filter {
        Filter {
            conditions: vec![Condition::equals("status", value)],
            raw: String::new(),
        }
    }

    #[test]
    fn a_select_is_read_as_a_derived_table_on_lines_of_its_own() {
        for dialect in &ENGINES {
            let q = |ident: &str| dialect.quote_ident(ident);
            // The `;` goes, and the comment after the last word with it; one
            // inside stays on its line.
            let shipped = query(
                dialect,
                "select * -- all of them\nfrom shop.orders -- end\n;",
            );
            assert_eq!(shipped.text(), "select * -- all of them\nfrom shop.orders");
            let columns = columns_named(&["id", "status"]);
            assert_eq!(
                select_from(
                    dialect,
                    Source::Query(&shipped),
                    &columns,
                    &status_is("shipped"),
                    Some(("id", true)),
                )
                .unwrap(),
                format!(
                    "SELECT * FROM (\nselect * -- all of them\nfrom shop.orders\n) kerno_q\n\
                     WHERE {} = 'shipped'\nORDER BY {} DESC",
                    q("status"),
                    q("id")
                )
            );
            // Its own ORDER BY and LIMIT stay inside; nothing is added
            // without a filter or an order.
            let ordered = query(dialect, "/* top */ SELECT id FROM t ORDER BY id LIMIT 5;");
            assert_eq!(
                select_from(
                    dialect,
                    Source::Query(&ordered),
                    &columns,
                    &Filter::default(),
                    None
                )
                .unwrap(),
                "SELECT * FROM (\nSELECT id FROM t ORDER BY id LIMIT 5\n) kerno_q"
            );
            // MySQL and MariaDB drop an ORDER BY without LIMIT in a derived
            // table; PostgreSQL keeps it.
            let sorted = query(dialect, "select id from t order by id desc");
            assert_eq!(
                sorted.drops_own_order(dialect),
                dialect.engine() != Engine::Postgres
            );
            assert!(!ordered.drops_own_order(dialect));
            assert!(
                !query(dialect, "select id from (select id from t order by id) s")
                    .drops_own_order(dialect)
            );
            // A CTE, a UNION in parentheses, VALUES: the server reads them.
            for statement in [
                "WITH s AS (SELECT 1 AS id) SELECT * FROM s",
                "(SELECT 1 AS id) UNION ALL (SELECT 2)",
                "VALUES (1)",
            ] {
                assert_eq!(query(dialect, statement).text(), statement);
            }
            // Counted through the same derived table.
            assert_eq!(
                count_from(dialect, Source::Query(&shipped), &columns, &status_is("x")).unwrap(),
                format!(
                    "SELECT count(*) FROM (\nselect * -- all of them\nfrom shop.orders\n) \
                     kerno_q\nWHERE {} = 'x'",
                    q("status")
                )
            );
        }
    }

    #[test]
    fn what_cannot_be_read_again_says_why() {
        for dialect in &ENGINES {
            let no = |statement: &str| refused(dialect, statement, &["id"]);
            assert!(no("").contains("no statement"));
            assert!(no("select 1; select 2").contains("several statements"));
            assert!(no("update t set a = 1").contains("only the result of one SELECT"));
            assert!(no("show tables").contains("only the result of one SELECT"));
            assert!(no("explain select 1").contains("only the result of one SELECT"));
            assert!(no("select * into t2 from t").contains("writes"));
            assert!(no("select * from t for update").contains("locks rows"));
            assert!(no("SELECT * FROM t FOR NO KEY UPDATE").contains("locks rows"));
            assert!(no("select * from t for share skip locked").contains("locks rows"));
            assert!(no("select * from t lock in share mode").contains("locks rows"));
            // A FOR inside parentheses locks nothing.
            assert!(Query::new(dialect, "select substring('abc' from 1 for 2)", &[]).is_ok());
        }
        assert!(
            refused(
                &PG,
                "with d as (delete from t returning *) select * from d",
                &["id"]
            )
            .contains("writes")
        );
    }

    #[test]
    fn duplicate_column_names_are_refused_where_the_server_would() {
        let statement = "select a.id, b.id, a.status from a join b using (status)";
        // MySQL and MariaDB refuse any derived table with them.
        for engine in [Engine::MySql, Engine::MariaDb] {
            let message = refused(&Engined(engine), statement, &["id", "id", "status"]);
            assert!(
                message.contains("more than one column named \"id\"")
                    && message.contains(engine.label()),
                "{message}"
            );
        }
        // PostgreSQL reads them; only naming one is ambiguous.
        let both = Query::new(&PG, statement, &names(&["id", "id", "status"])).unwrap();
        let columns = columns_named(&["id", "id", "status"]);
        let source = Source::Query(&both);
        assert!(
            select_from(
                &PG,
                source,
                &columns,
                &status_is("x"),
                Some(("status", false))
            )
            .is_ok()
        );
        let ambiguous = |result: Result<String>| result.unwrap_err().to_string();
        let on_id = Filter {
            conditions: vec![Condition::equals("id", "1")],
            raw: String::new(),
        };
        assert!(
            ambiguous(select_from(&PG, source, &columns, &on_id, None))
                .contains("more than one column named \"id\"")
        );
        assert!(
            ambiguous(select_from(
                &PG,
                source,
                &columns,
                &Filter::default(),
                Some(("id", true))
            ))
            .contains("AS")
        );
        assert!(
            ambiguous(distinct_sql(
                &PG,
                source,
                &columns,
                &Filter::default(),
                "id",
                10
            ))
            .contains("\"id\"")
        );
        assert!(count_from(&PG, source, &columns, &Filter::default()).is_ok());
    }

    fn columns_named(names: &[&str]) -> Vec<ColumnInfo> {
        names.iter().map(|name| col(name, TypeKind::Text)).collect()
    }

    #[test]
    fn distinct_values_come_with_their_counts_most_frequent_first() {
        let table = TableRef::new("shop", "orders");
        let columns = columns_named(&["id", "status"]);
        assert_eq!(
            distinct_sql(
                &PG,
                Source::Table(&table),
                &columns,
                &status_is("paid"),
                "status",
                200
            )
            .unwrap(),
            "SELECT \"status\", count(*) FROM \"shop\".\"orders\"\nWHERE \"status\" = 'paid'\n\
             GROUP BY \"status\"\nORDER BY 2 DESC, 1\nLIMIT 200"
        );
        let my = Engined(Engine::MySql);
        let wrapped = query(&my, "select * from orders");
        assert_eq!(
            distinct_sql(
                &my,
                Source::Query(&wrapped),
                &columns,
                &Filter::default(),
                "status",
                5
            )
            .unwrap(),
            "SELECT `status`, count(*) FROM (\nselect * from orders\n) kerno_q\n\
             GROUP BY `status`\nORDER BY 2 DESC, 1\nLIMIT 5"
        );
    }

    #[test]
    fn picked_values_become_one_condition_with_literals() {
        let text = col("full name", TypeKind::Text);
        let number = col("id", TypeKind::Integer);
        let one_of = |column: &ColumnInfo, values: &[Option<&str>]| {
            let values: Vec<Option<String>> =
                values.iter().map(|value| value.map(String::from)).collect();
            one_of_sql(&AnsiDialect, column, &values)
        };
        assert_eq!(one_of(&text, &[]), None);
        // A comma, a quote and spaces are part of the value.
        assert_eq!(
            one_of(&text, &[Some("a, b"), Some(" O'Neil")]).unwrap(),
            "\"full name\" IN ('a, b', ' O''Neil')"
        );
        assert_eq!(
            one_of(&number, &[Some("7"), None]).unwrap(),
            "(\"id\" IN (7) OR \"id\" IS NULL)"
        );
        assert_eq!(one_of(&number, &[None]).unwrap(), "\"id\" IS NULL");
        // Added to what is typed, which keeps its own precedence.
        assert_eq!(and_where("  ", "a = 1"), "a = 1");
        assert_eq!(
            and_where("a = 1 OR b = 2", "c = 3"),
            "(a = 1 OR b = 2) AND c = 3"
        );
    }

    #[test]
    fn a_result_is_described_by_its_columns() {
        let described = described(&[Column::new("n", "int4", TypeKind::Integer)]);
        assert_eq!(described[0].name, "n");
        assert_eq!(described[0].data_type, "int4");
        assert_eq!(described[0].kind, TypeKind::Integer);
        assert!(described[0].nullable);
    }
}
