//! Client-side sort of a fetched result.
//!
//! Cells are the server's text, so the order is worked out from that text:
//! numbers by value at any size or precision, without converting them to a
//! machine number; booleans false first; everything else byte by byte.
//! Dates and timestamps in ISO form sort correctly as bytes, except when
//! zone offsets differ within a column or a year is not four digits.

use std::cmp::Ordering;

use crate::format::parse_bool;
use crate::result::{ResultStore, TypeKind};

/// What a cell is for sorting, in ascending order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Class {
    NegativeInfinity,
    Negative,
    Zero,
    Positive,
    Infinity,
    /// Greater than every number, as PostgreSQL sorts it.
    NaN,
    False,
    True,
    /// Text, bytes, and what is not a number in a numeric column.
    Bytes,
    Null,
}

/// A cell prepared for comparison: comparing two keys allocates nothing.
#[derive(Clone, Copy)]
struct Key<'a> {
    class: Class,
    /// For a finite number other than zero, the power of ten above its
    /// first significant digit: the value is `0.<digits> × 10^magnitude`.
    magnitude: i64,
    /// For a number, its significant digits: no leading or trailing zero,
    /// but possibly the decimal point. For [`Class::Bytes`], the cell.
    bytes: &'a [u8],
}

impl<'a> Key<'a> {
    fn new(cell: Option<&'a [u8]>, kind: TypeKind) -> Self {
        let class = |class| Key {
            class,
            magnitude: 0,
            bytes: &[],
        };
        let Some(bytes) = cell else {
            return class(Class::Null);
        };
        let parsed = match kind {
            TypeKind::Integer | TypeKind::Float | TypeKind::Decimal => number(bytes),
            TypeKind::Boolean => {
                parse_bool(bytes).map(|value| class(if value { Class::True } else { Class::False }))
            }
            _ => None,
        };
        parsed.unwrap_or(Key {
            class: Class::Bytes,
            magnitude: 0,
            bytes,
        })
    }

    fn cmp(&self, other: &Self) -> Ordering {
        self.class.cmp(&other.class).then_with(|| match self.class {
            Class::Positive => self.cmp_magnitude(other),
            Class::Negative => other.cmp_magnitude(self),
            Class::Bytes => self.bytes.cmp(other.bytes),
            _ => Ordering::Equal,
        })
    }

    /// Compares the absolute values of two finite numbers that are not zero.
    fn cmp_magnitude(&self, other: &Self) -> Ordering {
        let digits = |key: &Self| key.bytes.iter().filter(|&&byte| byte != b'.');
        self.magnitude
            .cmp(&other.magnitude)
            .then_with(|| digits(self).cmp(digits(other)))
    }
}

/// Reads a number as the servers print them: an optional sign, digits
/// with an optional decimal point, an optional exponent; or `NaN`,
/// `Infinity`, `inf`. `None` for anything else (`$1,234.56`, an empty
/// cell).
fn number(text: &[u8]) -> Option<Key<'_>> {
    let text = text.trim_ascii();
    let (negative, body) = match text.split_first() {
        Some((b'-', rest)) => (true, rest),
        Some((b'+', rest)) => (false, rest),
        _ => (false, text),
    };
    let class = |class| {
        Some(Key {
            class,
            magnitude: 0,
            bytes: &[],
        })
    };
    if body.eq_ignore_ascii_case(b"nan") {
        return class(Class::NaN);
    }
    if body.eq_ignore_ascii_case(b"infinity") || body.eq_ignore_ascii_case(b"inf") {
        return class(if negative {
            Class::NegativeInfinity
        } else {
            Class::Infinity
        });
    }

    let (mantissa, exponent) = match body.iter().position(|byte| matches!(byte, b'e' | b'E')) {
        Some(at) => (&body[..at], parse_exponent(&body[at + 1..])?),
        None => (body, 0),
    };
    let point = mantissa.iter().position(|&byte| byte == b'.');
    let digits_and_point = mantissa
        .iter()
        .enumerate()
        .all(|(index, byte)| byte.is_ascii_digit() || Some(index) == point);
    // At least one digit: `.` alone is not a number.
    if !digits_and_point || mantissa.len() == usize::from(point.is_some()) {
        return None;
    }

    let significant = |byte: &u8| matches!(byte, b'1'..=b'9');
    let (Some(first), Some(last)) = (
        mantissa.iter().position(significant),
        mantissa.iter().rposition(significant),
    ) else {
        return class(Class::Zero);
    };
    let whole = point.unwrap_or(mantissa.len());
    let magnitude = if first < whole {
        (whole - first) as i64
    } else {
        // Zeros between the point and the first significant digit.
        -((first - whole - 1) as i64)
    };
    Some(Key {
        class: if negative {
            Class::Negative
        } else {
            Class::Positive
        },
        magnitude: magnitude.saturating_add(exponent),
        bytes: &mantissa[first..=last],
    })
}

/// The value of an exponent: an optional sign and digits. Saturates, so
/// an exponent no number can have still compares as huge or tiny.
fn parse_exponent(text: &[u8]) -> Option<i64> {
    let (negative, digits) = match text.split_first() {
        Some((b'-', rest)) => (true, rest),
        Some((b'+', rest)) => (false, rest),
        _ => (false, text),
    };
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let value = digits.iter().fold(0i64, |value, digit| {
        value
            .saturating_mul(10)
            .saturating_add(i64::from(digit - b'0'))
    });
    Some(if negative { -value } else { value })
}

/// Compares two cells of a column of the given kind, for an ascending
/// sort. NULL is greater than every value.
///
/// - Integer, float and decimal columns compare by value, whatever the
///   number of digits and however the number is written (`00012`, `1.2e1`,
///   `.5`); `-Infinity`, `Infinity` and `NaN` are in the order PostgreSQL
///   gives them, `NaN` last. A cell that is not a number (`$1,234.56`)
///   comes after the numbers, in byte order.
/// - Boolean columns put false before true, in any of the servers'
///   spellings.
/// - Everything else compares byte by byte: no collation.
pub fn compare_cells(a: Option<&[u8]>, b: Option<&[u8]>, kind: TypeKind) -> Ordering {
    Key::new(a, kind).cmp(&Key::new(b, kind))
}

/// The rows of `store` in the order of `column`: `result[i]` is the index
/// of the row to show at position `i`.
///
/// The sort is stable: rows that compare equal keep their fetch order,
/// descending too. NULLs come last in both directions. Every cell is read
/// once, then comparisons allocate nothing; the keys take 32 bytes per row
/// while the sort runs.
///
/// Only the first `u32::MAX` rows are sorted.
///
/// # Panics
/// Panics if `column` is out of range.
pub fn sorted_rows(store: &ResultStore, column: usize, descending: bool) -> Vec<u32> {
    let kind = store.columns()[column].kind;
    let count = u32::try_from(store.row_count()).unwrap_or(u32::MAX);
    let keys: Vec<Key<'_>> = (0..count)
        .map(|row| Key::new(store.cell(row as usize, column), kind))
        .collect();
    let mut rows: Vec<u32> = (0..count).collect();
    rows.sort_by(|&a, &b| {
        let (a, b) = (&keys[a as usize], &keys[b as usize]);
        match (a.class == Class::Null, b.class == Class::Null) {
            (false, false) if descending => b.cmp(a),
            (false, false) => a.cmp(b),
            (a_null, b_null) => a_null.cmp(&b_null),
        }
    });
    rows
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Instant;

    use super::*;
    use crate::result::{BatchBuilder, Column};

    fn compare(a: &str, b: &str, kind: TypeKind) -> Ordering {
        compare_cells(Some(a.as_bytes()), Some(b.as_bytes()), kind)
    }

    /// Every value is less than all the values after it.
    #[track_caller]
    fn assert_ascending(values: &[&str], kind: TypeKind) {
        for (i, a) in values.iter().enumerate() {
            assert_eq!(compare(a, a, kind), Ordering::Equal, "{a:?} = {a:?}");
            for b in &values[i + 1..] {
                assert_eq!(compare(a, b, kind), Ordering::Less, "{a:?} < {b:?}");
                assert_eq!(compare(b, a, kind), Ordering::Greater, "{b:?} > {a:?}");
            }
        }
    }

    /// All the values are the same value.
    #[track_caller]
    fn assert_same(values: &[&str], kind: TypeKind) {
        for a in values {
            for b in values {
                assert_eq!(compare(a, b, kind), Ordering::Equal, "{a:?} = {b:?}");
            }
        }
    }

    const NUMBERS_ASCENDING: [&str; 36] = [
        "-Infinity",
        "-1e400",
        "-1234567890123456789012345678901234567890",
        "-123456789012345678901234567890123456789",
        "-10",
        "-2",
        "-1.5",
        "-1.25",
        "-0.5",
        "-.25",
        "-1e-400",
        "0",
        "1e-400",
        "1.5e-7",
        "0.000001",
        "0.25",
        ".5",
        "1",
        "1.25",
        "1.5",
        "2",
        "9.99",
        "10",
        "9999999999",
        "1e10",
        "1.5e10",
        "123456789012345678901234567890123456789",
        "1234567890123456789012345678901234567890",
        "1234567890123456789012345678901234567891",
        "1e400",
        "Infinity",
        "NaN",
        // What is not a number comes after the numbers, in byte order.
        "",
        "$1,234.56",
        "1,000",
        "abc",
    ];

    #[test]
    fn numbers_compare_by_value() {
        for kind in [TypeKind::Integer, TypeKind::Float, TypeKind::Decimal] {
            assert_ascending(&NUMBERS_ASCENDING, kind);
        }
        // As text, the same values are in another order.
        assert_eq!(compare("10", "9", TypeKind::Text), Ordering::Less);
        assert_eq!(compare("10", "9", TypeKind::Integer), Ordering::Greater);
    }

    #[test]
    fn numbers_written_differently_are_equal() {
        for same in [
            &[
                "0", "-0", "+0", "0.0", ".0", "0.", "000", "0e5", "-0.000", "0E-3",
            ][..],
            &[
                "12", "00012", "+12", "12.0", "12.", "1.2e1", "120e-1", "0.12E+2", " 12 ",
            ],
            &["0.5", ".5", "0.50", "5e-1", "+.5", "00.5000"],
            &["-0.5", "-.5", "-5e-1"],
            &["100", "1e2", "1E+2", "10.0e1", "1000e-1", "100.00"],
            &["0.001", "1e-3", "0.0010", ".001"],
            &["NaN", "nan", "NAN"],
            &["Infinity", "inf", "+Infinity", "INFINITY"],
            &["-Infinity", "-inf"],
            // Exponents no machine number holds.
            &["1e99999999999999999999999", "1e99999999999999999999998"],
        ] {
            assert_same(same, TypeKind::Decimal);
            assert_same(same, TypeKind::Float);
        }
    }

    #[test]
    fn what_looks_like_a_number_but_is_not_sorts_as_text() {
        let garbage = [
            "-", "--1", ".", "0x10", "1 2", "1.2.3", "1e", "1e+", "1e1.5", "e5", "infinite",
        ];
        assert_ascending(&garbage, TypeKind::Decimal);
        for text in garbage {
            assert_eq!(compare("1e400", text, TypeKind::Float), Ordering::Less);
            assert_eq!(compare("NaN", text, TypeKind::Float), Ordering::Less);
        }
    }

    #[test]
    fn booleans_put_false_first() {
        assert_ascending(&["f", "t"], TypeKind::Boolean);
        assert_ascending(&["0", "1"], TypeKind::Boolean);
        assert_ascending(&["false", "true", "maybe"], TypeKind::Boolean);
        assert_same(&["f", "0", "false", "FALSE"], TypeKind::Boolean);
        assert_same(&["t", "1", "true", "True"], TypeKind::Boolean);
    }

    #[test]
    fn text_and_the_rest_compare_bytewise() {
        for kind in [
            TypeKind::Text,
            TypeKind::Uuid,
            TypeKind::Date,
            TypeKind::Other,
        ] {
            assert_ascending(&["", " ", "10", "9", "B", "a", "ab", "b", "\u{e9}"], kind);
        }
        let bytes: [&[u8]; 5] = [b"", b"\x00", b"\x00\x00", b"\x00\x01", b"\xff"];
        for (i, a) in bytes.iter().enumerate() {
            for b in &bytes[i + 1..] {
                assert_eq!(
                    compare_cells(Some(a), Some(b), TypeKind::Binary),
                    Ordering::Less
                );
            }
        }
    }

    #[test]
    fn nulls_are_greater_than_everything() {
        for kind in [TypeKind::Integer, TypeKind::Boolean, TypeKind::Text] {
            for value in ["", "0", "NaN", "t", "zzz", "\u{10ffff}"] {
                let cell = Some(value.as_bytes());
                assert_eq!(compare_cells(None, cell, kind), Ordering::Greater);
                assert_eq!(compare_cells(cell, None, kind), Ordering::Less);
            }
            assert_eq!(compare_cells(None, None, kind), Ordering::Equal);
        }
    }

    #[test]
    fn the_order_is_consistent() {
        // Sorting needs a total order: check it on every pair and triple.
        let mut values: Vec<&str> = NUMBERS_ASCENDING.to_vec();
        values.extend(["-0", "00012", "12", "1.2e1", ".", "-", "nan", "inf", "5e-1"]);
        let cells: Vec<Option<&[u8]>> = values
            .iter()
            .map(|value| Some(value.as_bytes()))
            .chain([None])
            .collect();
        for kind in [TypeKind::Decimal, TypeKind::Boolean, TypeKind::Text] {
            for &a in &cells {
                for &b in &cells {
                    let ab = compare_cells(a, b, kind);
                    assert_eq!(compare_cells(b, a, kind), ab.reverse());
                    for &c in &cells {
                        if ab != Ordering::Greater && compare_cells(b, c, kind) != Ordering::Greater
                        {
                            assert_ne!(compare_cells(a, c, kind), Ordering::Greater);
                        }
                    }
                }
            }
        }
    }

    /// A one-column store with one batch per slice.
    fn store(kind: TypeKind, batches: &[&[Option<&str>]]) -> ResultStore {
        let columns: Arc<[Column]> = Arc::new([Column::new("c", "t", kind)]);
        let mut store = ResultStore::new(columns);
        let mut builder = BatchBuilder::new(1);
        for batch in batches {
            for cell in *batch {
                builder.push_opt(cell.map(str::as_bytes));
            }
            store.push(builder.finish());
        }
        store
    }

    #[test]
    fn rows_are_sorted_with_ties_in_fetch_order_and_nulls_last() {
        let numbers = store(
            TypeKind::Integer,
            &[
                &[Some("10"), None, Some("2"), Some("2")],
                &[None, Some("-1"), Some("10"), Some("abc")],
            ],
        );
        assert_eq!(sorted_rows(&numbers, 0, false), [5, 2, 3, 0, 6, 7, 1, 4]);
        assert_eq!(sorted_rows(&numbers, 0, true), [7, 0, 6, 2, 3, 5, 1, 4]);

        let text = store(
            TypeKind::Text,
            &[&[Some("b"), Some("a"), Some(""), None, Some("B"), Some("a")]],
        );
        assert_eq!(sorted_rows(&text, 0, false), [2, 4, 1, 5, 0, 3]);
        assert_eq!(sorted_rows(&text, 0, true), [0, 1, 5, 4, 2, 3]);

        let booleans = store(
            TypeKind::Boolean,
            &[&[Some("t"), Some("f"), None], &[Some("t"), Some("f")]],
        );
        assert_eq!(sorted_rows(&booleans, 0, false), [1, 4, 0, 3, 2]);
        assert_eq!(sorted_rows(&booleans, 0, true), [0, 3, 1, 4, 2]);
    }

    #[test]
    fn values_equal_under_different_spellings_keep_their_fetch_order() {
        let decimals = store(
            TypeKind::Decimal,
            &[&[Some("1.0"), Some("0.5"), Some("1"), Some(".5"), Some("1e0")]],
        );
        assert_eq!(sorted_rows(&decimals, 0, false), [1, 3, 0, 2, 4]);
        assert_eq!(sorted_rows(&decimals, 0, true), [0, 2, 4, 1, 3]);
    }

    #[test]
    fn the_column_to_sort_by_is_the_one_asked_for() {
        let columns: Arc<[Column]> = Arc::new([
            Column::new("id", "int4", TypeKind::Integer),
            Column::new("name", "text", TypeKind::Text),
        ]);
        let mut store = ResultStore::new(columns);
        let mut builder = BatchBuilder::new(2);
        for (id, name) in [("3", "a"), ("1", "c"), ("2", "b")] {
            builder.push_text(id);
            builder.push_text(name);
        }
        store.push(builder.finish());
        assert_eq!(sorted_rows(&store, 0, false), [1, 2, 0]);
        assert_eq!(sorted_rows(&store, 1, false), [0, 2, 1]);
    }

    #[test]
    fn a_key_is_four_words() {
        // A million rows are sorted with 32 MB of keys, as the docs say.
        assert_eq!(std::mem::size_of::<Key<'_>>(), 32);
    }

    #[test]
    fn an_empty_result_sorts_to_nothing() {
        let empty = store(TypeKind::Integer, &[]);
        assert_eq!(sorted_rows(&empty, 0, false), [0u32; 0]);
        assert_eq!(sorted_rows(&empty, 0, true), [0u32; 0]);
    }

    /// Rows of integers, decimals, exponents, negative numbers, duplicates
    /// and NULLs, in batches of 10,000.
    fn mixed_rows(count: u32) -> ResultStore {
        let columns: Arc<[Column]> = Arc::new([Column::new("n", "numeric", TypeKind::Decimal)]);
        let mut store = ResultStore::new(columns);
        let mut builder = BatchBuilder::new(1);
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        for row in 0..count {
            // xorshift: the same rows on every run.
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let n = seed % 2_000_000;
            match seed >> 60 {
                0 => builder.push_null(),
                1..=6 => builder.push_text(&n.to_string()),
                7..=9 => builder.push_text(&format!("-{}.{:03}", n / 1000, n % 1000)),
                10..=12 => builder.push_text(&format!("{}.{:02}", n % 5000, n % 100)),
                13 => builder.push_text(&format!("{}e{}", n % 1000, n % 7)),
                14 => builder.push_text("42"),
                _ => builder.push_text(&format!("{n}{:020}", seed % 1_000_000_007)),
            }
            if row % 10_000 == 9_999 {
                store.push(builder.finish());
            }
        }
        store
    }

    fn assert_sorted(store: &ResultStore, rows: &[u32], descending: bool) {
        assert_eq!(rows.len(), store.row_count());
        let mut seen = vec![false; rows.len()];
        for pair in rows.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let (cell_a, cell_b) = (store.cell(a as usize, 0), store.cell(b as usize, 0));
            let order = compare_cells(cell_a, cell_b, TypeKind::Decimal);
            let in_order = match (cell_a, cell_b) {
                (_, None) => true,
                (None, Some(_)) => false,
                _ if descending => order != Ordering::Less,
                _ => order != Ordering::Greater,
            };
            assert!(in_order, "rows {a} and {b} are out of order");
            if order == Ordering::Equal {
                assert!(
                    a < b,
                    "rows {a} and {b} are equal and lost their fetch order"
                );
            }
        }
        for &row in rows {
            assert!(!std::mem::replace(&mut seen[row as usize], true));
        }
    }

    fn sort_mixed_rows(count: u32) {
        let store = mixed_rows(count);
        for descending in [false, true] {
            let started = Instant::now();
            let rows = sorted_rows(&store, 0, descending);
            let elapsed = started.elapsed();
            eprintln!("sorted {count} rows (descending: {descending}) in {elapsed:?}");
            assert_sorted(&store, &rows, descending);
        }
    }

    #[test]
    fn two_hundred_thousand_mixed_rows_sort_correctly() {
        sort_mixed_rows(200_000);
    }

    /// The size the grid sorts at most; the time is printed, for
    /// `docs/BENCH.md`. Run it in a release build:
    /// `cargo test --release -p kerno-core --lib -- --ignored --nocapture one_million`.
    #[test]
    #[ignore = "a measurement, slow in a debug build"]
    fn one_million_mixed_rows_sort_correctly() {
        sort_mixed_rows(1_000_000);
    }
}
