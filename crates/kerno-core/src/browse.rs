//! Browsing a table page by page: the filter, the `SELECT` of one page and
//! the count of the rows the filter keeps.
//!
//! Values are written as literals by the dialect, never pasted: a value
//! typed in the filter bar cannot change the shape of the statement.

use crate::dialect::Dialect;
use crate::error::{Error, Result};
use crate::result::TypeKind;
use crate::schema::{ColumnInfo, TableRef};
use crate::sql::{CellValue, TokenKind, lex};

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

/// The `SELECT` that reads `page`.
pub fn page_sql(dialect: &dyn Dialect, columns: &[ColumnInfo], page: Page<'_>) -> Result<String> {
    let mut sql = format!("SELECT * FROM {}", dialect.table_name(page.table));
    if let Some(condition) = page.filter.where_sql(dialect, columns)? {
        sql.push_str("\nWHERE ");
        sql.push_str(&condition);
    }
    let mut order = Vec::with_capacity(page.identity.len() + 1);
    if let Some((column, descending)) = page.sort {
        let ident = dialect.quote_ident(column);
        order.push(if descending {
            format!("{ident} DESC")
        } else {
            ident
        });
    }
    order.extend(
        page.identity
            .iter()
            .filter(|column| {
                page.sort
                    .is_none_or(|(sorted, _)| sorted != column.as_str())
            })
            .map(|column| dialect.quote_ident(column)),
    );
    if !order.is_empty() {
        sql.push_str("\nORDER BY ");
        sql.push_str(&order.join(", "));
    }
    sql.push('\n');
    sql.push_str(&dialect.limit_offset(page.limit, page.offset));
    Ok(sql)
}

/// The statement that counts the rows `filter` keeps.
pub fn count_sql(
    dialect: &dyn Dialect,
    columns: &[ColumnInfo],
    table: &TableRef,
    filter: &Filter,
) -> Result<String> {
    let mut sql = format!("SELECT count(*) FROM {}", dialect.table_name(table));
    if let Some(condition) = filter.where_sql(dialect, columns)? {
        sql.push_str("\nWHERE ");
        sql.push_str(&condition);
    }
    Ok(sql)
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
        assert_eq!(
            count_sql(&AnsiDialect, &columns(), &table, &filter).unwrap(),
            "SELECT count(*) FROM \"shop\".\"people\"\nWHERE \"id\" = 3"
        );
        assert_eq!(
            count_sql(&AnsiDialect, &columns(), &table, &none).unwrap(),
            "SELECT count(*) FROM \"shop\".\"people\""
        );
    }
}
