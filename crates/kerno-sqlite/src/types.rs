//! SQLite's types: a column declares a type, each value has a storage
//! class of its own (<https://www.sqlite.org/datatype3.html>).

use kerno_core::TypeKind;
use rusqlite::types::ValueRef;

/// The kind a declared type names: the affinity rules of section 3.1 of
/// SQLite's datatype page, with the names it recognises as dates,
/// booleans and JSON (all of NUMERIC affinity there) picked out first.
pub(crate) fn declared_kind(declared: &str) -> TypeKind {
    let upper = declared.trim().to_ascii_uppercase();
    // The name before any `(…)`: `VARCHAR(20)`, `DECIMAL(10,2)`.
    let name = upper.split('(').next().unwrap_or_default().trim();
    match name {
        "BOOLEAN" | "BOOL" => return TypeKind::Boolean,
        "DATE" => return TypeKind::Date,
        "DATETIME" | "TIMESTAMP" => return TypeKind::DateTime,
        "TIME" => return TypeKind::Time,
        "JSON" | "JSONB" => return TypeKind::Json,
        _ => {}
    }
    // Rules 1 to 5, in order.
    if upper.contains("INT") {
        TypeKind::Integer
    } else if ["CHAR", "CLOB", "TEXT"]
        .iter()
        .any(|word| upper.contains(word))
    {
        TypeKind::Text
    } else if upper.contains("BLOB") || upper.is_empty() {
        TypeKind::Binary
    } else if ["REAL", "FLOA", "DOUB"]
        .iter()
        .any(|word| upper.contains(word))
    {
        TypeKind::Float
    } else {
        TypeKind::Decimal
    }
}

/// The kind a value's storage class shows.
fn stored_kind(value: ValueRef<'_>) -> Option<TypeKind> {
    match value {
        ValueRef::Null => None,
        ValueRef::Integer(_) => Some(TypeKind::Integer),
        ValueRef::Real(_) => Some(TypeKind::Float),
        ValueRef::Text(_) => Some(TypeKind::Text),
        ValueRef::Blob(_) => Some(TypeKind::Binary),
    }
}

/// The kind of a result column, from its declared type (`None` for an
/// expression) and the first value it holds (`None` for an empty result).
///
/// A date, boolean or JSON type gets its rendering only when the value is
/// stored the way that rendering expects (a date as text, a boolean as an
/// integer, JSON as text); otherwise the column is what it holds: a date
/// kept as a Unix time is an integer. A column without a type, or whose
/// type is BLOB but which holds something else, also follows its value.
pub(crate) fn column_kind(declared: Option<&str>, first: Option<ValueRef<'_>>) -> TypeKind {
    let stored = first.and_then(stored_kind);
    let Some(declared) = declared.filter(|declared| !declared.trim().is_empty()) else {
        return stored.unwrap_or(TypeKind::Text);
    };
    let kind = declared_kind(declared);
    match (kind, stored) {
        (_, None) => kind,
        (TypeKind::Boolean, Some(TypeKind::Integer))
        | (
            TypeKind::Date | TypeKind::DateTime | TypeKind::Time | TypeKind::Json,
            Some(TypeKind::Text),
        )
        | (TypeKind::Binary, Some(TypeKind::Binary)) => kind,
        (
            TypeKind::Boolean | TypeKind::Date | TypeKind::DateTime | TypeKind::Time,
            Some(stored),
        )
        | (TypeKind::Json | TypeKind::Binary, Some(stored)) => stored,
        (kind, Some(_)) => kind,
    }
}

/// The name a result column shows in its header: the declared type, or
/// the storage class of its first value for an expression.
pub(crate) fn column_type_name(declared: Option<&str>, first: Option<ValueRef<'_>>) -> String {
    if let Some(declared) = declared.filter(|declared| !declared.trim().is_empty()) {
        return declared.to_string();
    }
    match first {
        Some(ValueRef::Integer(_)) => "integer",
        Some(ValueRef::Real(_)) => "real",
        Some(ValueRef::Text(_)) => "text",
        Some(ValueRef::Blob(_)) => "blob",
        Some(ValueRef::Null) | None => "",
    }
    .into()
}

/// A REAL in the shortest text that reads back as the same number, with a
/// `.0` when it is whole so that it stays a REAL in a literal. SQLite's own
/// text (`%!.15g`) does not round-trip, so an edit would miss the row.
pub(crate) fn real_text(value: f64) -> String {
    if value.is_infinite() {
        // What SQLite reads back as an infinity.
        return if value > 0.0 { "9e999" } else { "-9e999" }.into();
    }
    format!("{value:?}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_types_follow_the_affinity_rules() {
        use TypeKind::*;
        for (declared, kind) in [
            ("INTEGER", Integer),
            ("int", Integer),
            ("BIGINT", Integer),
            ("UNSIGNED BIG INT", Integer),
            // Rule 1 comes first: "INT" in POINT gives INTEGER affinity.
            ("POINT", Integer),
            ("TEXT", Text),
            ("VARCHAR(255)", Text),
            ("NCHAR(55)", Text),
            ("CLOB", Text),
            ("BLOB", Binary),
            ("", Binary),
            ("REAL", Float),
            ("DOUBLE PRECISION", Float),
            ("FLOAT", Float),
            ("NUMERIC", Decimal),
            ("DECIMAL(10,5)", Decimal),
            ("BOOLEAN", Boolean),
            ("bool", Boolean),
            ("DATE", Date),
            ("DATETIME", DateTime),
            ("TIMESTAMP", DateTime),
            ("TIME", Time),
            ("JSON", Json),
            ("STRING", Decimal),
        ] {
            assert_eq!(declared_kind(declared), kind, "{declared}");
        }
    }

    #[test]
    fn a_rendering_is_kept_only_when_the_value_fits_it() {
        use TypeKind::*;
        let text = ValueRef::Text(b"2024-01-02");
        let integer = ValueRef::Integer(1_700_000_000);
        assert_eq!(column_kind(Some("DATE"), Some(text)), Date);
        assert_eq!(column_kind(Some("DATE"), Some(integer)), Integer);
        assert_eq!(column_kind(Some("DATE"), Some(ValueRef::Null)), Date);
        assert_eq!(column_kind(Some("DATE"), None), Date);
        assert_eq!(
            column_kind(Some("BOOLEAN"), Some(ValueRef::Integer(1))),
            Boolean
        );
        assert_eq!(
            column_kind(Some("BOOLEAN"), Some(ValueRef::Text(b"yes"))),
            Text
        );
        assert_eq!(column_kind(Some("JSON"), Some(ValueRef::Text(b"{}"))), Json);
        assert_eq!(
            column_kind(Some("JSON"), Some(ValueRef::Blob(b"\x01"))),
            Binary
        );
        // A typeless column (an expression) shows what it holds.
        assert_eq!(column_kind(None, Some(ValueRef::Blob(b"\x00"))), Binary);
        assert_eq!(column_kind(None, Some(ValueRef::Real(1.5))), Float);
        assert_eq!(column_kind(None, None), Text);
        // BLOB affinity holding text is text.
        assert_eq!(column_kind(Some(""), Some(ValueRef::Text(b"a"))), Text);
        assert_eq!(
            column_kind(Some("BLOB"), Some(ValueRef::Integer(1))),
            Integer
        );
        // An INTEGER column keeps its kind whatever a row holds.
        assert_eq!(
            column_kind(Some("INTEGER"), Some(ValueRef::Text(b"a"))),
            Integer
        );
    }

    #[test]
    fn reals_round_trip_and_stay_reals() {
        for value in [0.1, 1.0, -2.5, 1e20, 1e-7, 123_456_789.123_456_78, f64::MAX] {
            let text = real_text(value);
            assert_eq!(text.parse::<f64>().unwrap(), value, "{text}");
            assert!(text.contains(['.', 'e']), "{text}");
        }
        assert_eq!(real_text(1.0), "1.0");
        assert_eq!(real_text(f64::INFINITY), "9e999");
    }

    #[test]
    fn headers_name_the_declared_type_or_the_storage_class() {
        assert_eq!(column_type_name(Some("VARCHAR(20)"), None), "VARCHAR(20)");
        assert_eq!(
            column_type_name(None, Some(ValueRef::Integer(1))),
            "integer"
        );
        assert_eq!(column_type_name(None, Some(ValueRef::Blob(b""))), "blob");
        assert_eq!(column_type_name(None, None), "");
    }
}
