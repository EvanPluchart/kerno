//! Query parameters: finding `:name`, `$1` and `?` in a statement and
//! replacing them with SQL literals before it runs.
//!
//! Values are written into the SQL text rather than bound on the wire, so
//! the statement Kerno shows is exactly the statement the server receives.

use std::ops::Range;

use super::lex::{LexRules, TokenKind, lex};
use crate::dialect::Dialect;

/// Which parameter an occurrence refers to.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ParamKey {
    /// `:name`. Every occurrence of the same name is one parameter.
    Named(String),
    /// `$n` (PostgreSQL). Every occurrence of the same number is one parameter.
    Numbered(u32),
    /// The n-th `?` of the statement, counting from 1 (MySQL). Each `?` is
    /// its own parameter.
    Positional(u32),
}

impl ParamKey {
    /// How the parameter is written, for the prompt.
    pub fn label(&self) -> String {
        match self {
            Self::Named(name) => format!(":{name}"),
            Self::Numbered(number) => format!("${number}"),
            Self::Positional(position) => format!("?{position}"),
        }
    }
}

/// A parameter of a statement and where it appears.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    /// Which parameter.
    pub key: ParamKey,
    /// Byte ranges of its occurrences in the statement.
    pub occurrences: Vec<Range<usize>>,
}

/// The value the user gave for a parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamValue {
    /// SQL `NULL`.
    Null,
    /// A value whose form decides its literal: a number or `true`/`false`
    /// is written bare, anything else as a string.
    Auto(String),
    /// Always a string literal, even when it looks like a number.
    Text(String),
    /// SQL written by the user, inserted as it is (an expression, a list).
    Raw(String),
}

impl ParamValue {
    /// The SQL text that replaces the parameter.
    pub fn to_sql(&self, dialect: &dyn Dialect) -> String {
        match self {
            Self::Null => "NULL".into(),
            Self::Auto(text) if is_number(text) => text.trim().to_string(),
            Self::Auto(text) if text.eq_ignore_ascii_case("true") => "TRUE".into(),
            Self::Auto(text) if text.eq_ignore_ascii_case("false") => "FALSE".into(),
            Self::Auto(text) | Self::Text(text) => dialect.quote_string(text),
            Self::Raw(sql) => sql.clone(),
        }
    }
}

/// Whether `text` is a plain decimal number: optional sign, digits, an
/// optional fraction and an optional exponent. Hex, `NaN`, `Infinity`,
/// thousands separators and surrounding garbage are not.
pub fn is_number(text: &str) -> bool {
    let text = text.trim();
    let unsigned = text.strip_prefix(['-', '+']).unwrap_or(text);
    let (mantissa, exponent) = match unsigned.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, Some(exponent)),
        None => (unsigned, None),
    };
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    let mantissa_ok = match mantissa.split_once('.') {
        Some((whole, fraction)) => {
            (digits(whole) || whole.is_empty())
                && (digits(fraction) || fraction.is_empty())
                && !(whole.is_empty() && fraction.is_empty())
        }
        None => digits(mantissa),
    };
    let exponent_ok = exponent
        .is_none_or(|exponent| digits(exponent.strip_prefix(['-', '+']).unwrap_or(exponent)));
    mantissa_ok && exponent_ok
}

/// The parameters of `sql`, in order of first appearance.
pub fn find_params(sql: &str, rules: LexRules) -> Vec<Param> {
    let tokens = lex(sql, rules);
    let mut params: Vec<Param> = Vec::new();
    let mut positional = 0;
    for (index, token) in tokens.iter().enumerate() {
        let text = &sql[token.range.clone()];
        let key = match token.kind {
            TokenKind::NamedParam => {
                // `a[lo:hi]` is an array slice and `x:y` is not SQL Kerno
                // should second-guess: a colon glued to what precedes it is
                // not the start of a parameter.
                let glued = index > 0
                    && matches!(
                        tokens[index - 1].kind,
                        TokenKind::Word
                            | TokenKind::Number
                            | TokenKind::QuotedIdent
                            | TokenKind::String
                    );
                let after_bracket =
                    index > 0 && matches!(&sql[tokens[index - 1].range.clone()], "]" | ")");
                if glued || after_bracket {
                    continue;
                }
                ParamKey::Named(text[1..].to_string())
            }
            TokenKind::PositionalParam if text == "?" => {
                positional += 1;
                ParamKey::Positional(positional)
            }
            TokenKind::PositionalParam => match text[1..].parse() {
                Ok(number) => ParamKey::Numbered(number),
                Err(_) => continue,
            },
            _ => continue,
        };
        match params.iter_mut().find(|param| param.key == key) {
            Some(param) => param.occurrences.push(token.range.clone()),
            None => params.push(Param {
                key,
                occurrences: vec![token.range.clone()],
            }),
        }
    }
    params
}

/// Replaces every occurrence of each parameter with the SQL from `value`.
/// A parameter for which `value` returns `None` is left as written.
pub fn bind_params(
    sql: &str,
    params: &[Param],
    mut value: impl FnMut(&ParamKey) -> Option<String>,
) -> String {
    let mut replacements: Vec<(Range<usize>, String)> = Vec::new();
    for param in params {
        if let Some(replacement) = value(&param.key) {
            for occurrence in &param.occurrences {
                replacements.push((occurrence.clone(), replacement.clone()));
            }
        }
    }
    replacements.sort_by_key(|(range, _)| range.start);
    let mut out = String::with_capacity(sql.len());
    let mut position = 0;
    for (range, replacement) in replacements {
        out.push_str(&sql[position..range.start]);
        out.push_str(&replacement);
        position = range.end;
    }
    out.push_str(&sql[position..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::test_support::AnsiDialect;

    fn keys(sql: &str, rules: LexRules) -> Vec<String> {
        find_params(sql, rules)
            .iter()
            .map(|p| p.key.label())
            .collect()
    }

    #[test]
    fn finds_named_and_numbered_parameters_once_each() {
        let sql = "select * from t where a = :id and b = $2 or c = :id and d = $1 and e = $2";
        let params = find_params(sql, LexRules::POSTGRES);
        let labels: Vec<_> = params.iter().map(|p| p.key.label()).collect();
        assert_eq!(labels, [":id", "$2", "$1"]);
        assert_eq!(params[0].occurrences.len(), 2);
        assert_eq!(params[1].occurrences.len(), 2);
        for param in &params {
            for range in &param.occurrences {
                assert_eq!(&sql[range.clone()], param.key.label());
            }
        }
    }

    #[test]
    fn question_marks_are_separate_parameters_on_mysql_only() {
        assert_eq!(
            keys("select ? + ? where x = ?", LexRules::MYSQL),
            ["?1", "?2", "?3"]
        );
        // On PostgreSQL `?` is a JSON operator.
        assert_eq!(
            keys("select doc ? 'key'", LexRules::POSTGRES),
            Vec::<String>::new()
        );
    }

    #[test]
    fn things_that_look_like_parameters_but_are_not() {
        for sql in [
            "select ':a', \":b\" -- :c\n /* :d */",
            "select x::int, now()::date",
            "select arr[lo:hi], arr[1:2], (arr)[i:j]",
            "select '12:30:00'::time",
            "select $$ :inside $1 $$",
            "do $body$ begin x := :y; end $body$",
        ] {
            assert_eq!(keys(sql, LexRules::POSTGRES), Vec::<String>::new(), "{sql}");
        }
        // MySQL user variables and assignment are not parameters either.
        assert_eq!(
            keys("set @a := 1; select @a", LexRules::MYSQL),
            Vec::<String>::new()
        );
        // A real parameter after an operator or an opening bracket still counts.
        assert_eq!(
            keys(
                "select arr[:lo] where a=:b and c in (:d)",
                LexRules::POSTGRES
            ),
            [":lo", ":b", ":d"]
        );
    }

    #[test]
    fn binds_every_occurrence_and_leaves_the_rest() {
        let sql = "select :a, ':a', :b, :a";
        let params = find_params(sql, LexRules::POSTGRES);
        let bound = bind_params(sql, &params, |key| match key {
            ParamKey::Named(name) if name == "a" => Some("42".into()),
            _ => None,
        });
        assert_eq!(bound, "select 42, ':a', :b, 42");

        let sql = "select ?, ?";
        let params = find_params(sql, LexRules::MYSQL);
        let bound = bind_params(sql, &params, |key| match key {
            ParamKey::Positional(n) => Some(format!("'v{n}'")),
            _ => None,
        });
        assert_eq!(bound, "select 'v1', 'v2'");
    }

    #[test]
    fn replacement_text_is_never_rescanned() {
        // A value containing something that looks like another parameter.
        let sql = "select :a, :b";
        let params = find_params(sql, LexRules::POSTGRES);
        let bound = bind_params(sql, &params, |key| match key {
            ParamKey::Named(name) if name == "a" => Some("':b'".into()),
            ParamKey::Named(name) if name == "b" => Some("2".into()),
            _ => None,
        });
        assert_eq!(bound, "select ':b', 2");
    }

    #[test]
    fn values_become_literals() {
        let d = AnsiDialect;
        assert_eq!(ParamValue::Null.to_sql(&d), "NULL");
        assert_eq!(ParamValue::Auto("42".into()).to_sql(&d), "42");
        assert_eq!(ParamValue::Auto(" -3.5e10 ".into()).to_sql(&d), "-3.5e10");
        assert_eq!(ParamValue::Auto("TRUE".into()).to_sql(&d), "TRUE");
        assert_eq!(ParamValue::Auto("O'Brien".into()).to_sql(&d), "'O''Brien'");
        assert_eq!(ParamValue::Auto("".into()).to_sql(&d), "''");
        // An injection attempt is just a string.
        assert_eq!(
            ParamValue::Auto("1; drop table t".into()).to_sql(&d),
            "'1; drop table t'"
        );
        assert_eq!(ParamValue::Text("42".into()).to_sql(&d), "'42'");
        assert_eq!(
            ParamValue::Raw("now() - interval '1 day'".into()).to_sql(&d),
            "now() - interval '1 day'"
        );
    }

    #[test]
    fn what_counts_as_a_number() {
        for yes in [
            "0", "42", "-7", "+7", "3.14", ".5", "5.", "1e9", "1.5E-3", " 12 ",
        ] {
            assert!(is_number(yes), "{yes}");
        }
        for no in [
            "", " ", "-", ".", "e5", "1e", "1e+", "0x1F", "1,000", "1 000", "NaN", "Infinity",
            "12abc", "1.2.3", "--1", "1;drop",
        ] {
            assert!(!is_number(no), "{no:?}");
        }
    }
}
