//! Finding text in the fetched rows of a result (DBeaver #38133, Beekeeper
//! #524, #941): which cells hold it, counted per row, and the rows that
//! hold it in the order they are shown.
//!
//! A cell matches when the text the grid draws for it holds the needle:
//! booleans as `true`/`false`, binary as its hex preview, everything else
//! as the server's text on one line, cut where the grid cuts it. NULL never
//! matches: it is no value. Nothing is allocated per cell for text that the
//! grid draws as it is.

use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::format::display;
use crate::result::{Column, ResultStore, TypeKind};

/// What is looked for, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Matcher {
    needle: String,
    /// Upper and lower case are told apart.
    case: bool,
    /// The grid's cut: text past this many characters is not drawn.
    max_chars: usize,
}

impl Matcher {
    /// A matcher for `needle`, or `None` when there is nothing to look for.
    pub fn new(needle: &str, case_sensitive: bool, max_chars: usize) -> Option<Self> {
        (!needle.is_empty()).then(|| Self {
            needle: if case_sensitive || needle.is_ascii() {
                needle.to_string()
            } else {
                needle.to_lowercase()
            },
            case: case_sensitive,
            max_chars,
        })
    }

    /// Whether `text` holds the needle.
    pub fn text_matches(&self, text: &str) -> bool {
        if self.case {
            return text.contains(&self.needle);
        }
        if self.needle.is_ascii() {
            return holds_ignoring_ascii_case(text.as_bytes(), self.needle.as_bytes());
        }
        // ponytail: a needle with letters outside ASCII lower-cases each
        // cell (an allocation per cell); fold case byte-wise if it shows.
        text.to_lowercase().contains(&self.needle)
    }

    /// Whether a cell of `column` matches, as the grid draws it.
    pub fn matches(&self, cell: Option<&[u8]>, column: &Column) -> bool {
        let Some(bytes) = cell else {
            return false;
        };
        // What the grid draws as it is: the bytes themselves, unless the
        // cell is longer than the grid shows, a boolean or binary (drawn
        // otherwise), or holds a character drawn as another one.
        let as_is = bytes.len() <= self.max_chars
            && !matches!(column.kind, TypeKind::Binary | TypeKind::Boolean)
            && !bytes.iter().any(|byte| byte.is_ascii_control());
        if as_is && let Ok(text) = std::str::from_utf8(bytes) {
            return self.text_matches(text);
        }
        self.text_matches(&display(cell, column, self.max_chars).text)
    }
}

/// Whether `haystack` holds `needle`, ASCII letters in either case.
fn holds_ignoring_ascii_case(haystack: &[u8], needle: &[u8]) -> bool {
    let Some((&first, rest)) = needle.split_first() else {
        return true;
    };
    if haystack.len() < needle.len() {
        return false;
    }
    let first = first.to_ascii_lowercase();
    (0..=haystack.len() - needle.len()).any(|at| {
        haystack[at].to_ascii_lowercase() == first
            && haystack[at + 1..at + needle.len()].eq_ignore_ascii_case(rest)
    })
}

/// How many cells of each row in `rows` match, among `columns` (columns of
/// the store); at most `u16::MAX` per row. `None` once `stop` is set: the
/// work was given up.
///
/// # Panics
/// Panics if a row or a column is out of the store's range.
pub fn count_rows(
    store: &ResultStore,
    columns: &[usize],
    matcher: &Matcher,
    rows: Range<usize>,
    stop: &AtomicBool,
) -> Option<Vec<u16>> {
    let described = store.columns();
    let mut counts = Vec::with_capacity(rows.len());
    for row in rows {
        if row % 4096 == 0 && stop.load(Ordering::Relaxed) {
            return None;
        }
        let matched = columns
            .iter()
            .filter(|&&column| matcher.matches(store.cell(row, column), &described[column]))
            .count();
        counts.push(u16::try_from(matched).unwrap_or(u16::MAX));
    }
    Some(counts)
}

/// The rows with a match, in the order they are shown: `order[i]` is the
/// row shown at position `i` (fetch order without one). Rows past the end
/// of `counts` (not counted yet) are left out.
pub fn matching_rows(counts: &[u16], order: Option<&[u32]>) -> Vec<u32> {
    let hit = |row: u32| counts.get(row as usize).is_some_and(|&count| count > 0);
    match order {
        Some(order) => order.iter().copied().filter(|&row| hit(row)).collect(),
        None => (0..u32::try_from(counts.len()).unwrap_or(u32::MAX))
            .filter(|&row| hit(row))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Instant;

    use super::*;
    use crate::result::BatchBuilder;
    use crate::sort::sorted_rows;

    fn column(kind: TypeKind) -> Column {
        Column::new("c", "t", kind)
    }

    fn matches(needle: &str, case: bool, cell: Option<&str>, kind: TypeKind) -> bool {
        Matcher::new(needle, case, 300)
            .unwrap()
            .matches(cell.map(str::as_bytes), &column(kind))
    }

    #[test]
    fn a_cell_matches_on_the_text_the_grid_draws() {
        // Substring, either case by default.
        assert!(matches("ship", false, Some("Shipped"), TypeKind::Text));
        assert!(matches("PED", false, Some("Shipped"), TypeKind::Text));
        assert!(!matches("PED", true, Some("Shipped"), TypeKind::Text));
        assert!(matches("ped", true, Some("Shipped"), TypeKind::Text));
        // Booleans as the grid spells them, not as the server sent them.
        assert!(matches("true", false, Some("t"), TypeKind::Boolean));
        assert!(!matches("t", false, Some("f"), TypeKind::Boolean));
        // Binary as its hex preview.
        assert!(matches("x0141", false, Some("\u{1}A"), TypeKind::Binary));
        // A line break is drawn as ↵: the text on each side still matches.
        assert!(matches("b", false, Some("a\nb"), TypeKind::Text));
        assert!(matches("a↵b", false, Some("a\nb"), TypeKind::Text));
        // Numbers and dates as the server wrote them.
        assert!(matches("12.5", false, Some("12.50"), TypeKind::Decimal));
        assert!(matches(
            "+02",
            false,
            Some("2026-11-14 01:23:45+02"),
            TypeKind::DateTimeTz
        ));
    }

    #[test]
    fn null_and_nothing_never_match() {
        assert!(!matches("NULL", false, None, TypeKind::Text));
        assert!(!matches("null", false, None, TypeKind::Integer));
        assert!(Matcher::new("", false, 300).is_none());
        // An empty value is a value, but holds nothing.
        assert!(!matches("a", false, Some(""), TypeKind::Text));
    }

    #[test]
    fn text_past_the_grids_cut_is_not_found() {
        let long = format!("{}needle", "x".repeat(300));
        assert!(!matches("needle", false, Some(&long), TypeKind::Text));
        let short = format!("{}needle", "x".repeat(294));
        assert!(matches("needle", false, Some(&short), TypeKind::Text));
    }

    #[test]
    fn letters_outside_ascii_match_in_either_case() {
        assert!(matches("é", false, Some("CAFÉ"), TypeKind::Text));
        assert!(matches("ÉTÉ", false, Some("été"), TypeKind::Text));
        assert!(!matches("É", true, Some("café"), TypeKind::Text));
        // An ASCII needle never matches inside a character of more bytes.
        assert!(!matches("A", false, Some("Å"), TypeKind::Text));
    }

    fn store(rows: &[[Option<&str>; 2]]) -> ResultStore {
        let columns: Arc<[Column]> = Arc::new([
            Column::new("id", "int4", TypeKind::Integer),
            Column::new("name", "text", TypeKind::Text),
        ]);
        let mut store = ResultStore::new(columns);
        let mut builder = BatchBuilder::new(2);
        for row in rows {
            for cell in row {
                builder.push_opt(cell.map(str::as_bytes));
            }
        }
        store.push(builder.finish());
        store
    }

    #[test]
    fn rows_count_their_matching_cells_in_the_shown_columns() {
        let store = store(&[
            [Some("1"), Some("a1")],
            [Some("2"), Some("b")],
            [Some("11"), None],
            [Some("3"), Some("x1y1")],
        ]);
        let stop = AtomicBool::new(false);
        let one = Matcher::new("1", false, 300).unwrap();
        assert_eq!(
            count_rows(&store, &[0, 1], &one, 0..4, &stop),
            Some(vec![2, 0, 1, 1])
        );
        // A hidden column does not count.
        assert_eq!(
            count_rows(&store, &[1], &one, 0..4, &stop),
            Some(vec![1, 0, 0, 1])
        );
        // Rows are counted from where asked: the rows that arrived later.
        assert_eq!(
            count_rows(&store, &[0, 1], &one, 2..4, &stop),
            Some(vec![1, 1])
        );
        stop.store(true, Ordering::Relaxed);
        assert_eq!(count_rows(&store, &[0], &one, 0..4, &stop), None);
    }

    #[test]
    fn matching_rows_follow_the_order_shown() {
        let counts = [1, 0, 2, 0, 1];
        assert_eq!(matching_rows(&counts, None), [0, 2, 4]);
        // Sorted, the matching rows come in the sorted order.
        assert_eq!(matching_rows(&counts, Some(&[4, 3, 2, 1, 0])), [4, 2, 0]);
        // Rows not counted yet are left out.
        assert_eq!(matching_rows(&counts[..3], Some(&[4, 2, 0])), [2, 0]);
    }

    #[test]
    fn matching_rows_compose_with_a_sort() {
        let store = store(&[
            [Some("3"), Some("pear")],
            [Some("1"), Some("apple")],
            [Some("2"), Some("grape")],
            [Some("4"), Some("melon")],
        ]);
        let stop = AtomicBool::new(false);
        let p = Matcher::new("p", false, 300).unwrap();
        let counts = count_rows(&store, &[1], &p, 0..4, &stop).unwrap();
        let ascending = sorted_rows(&store, 0, false);
        assert_eq!(matching_rows(&counts, Some(&ascending)), [1, 2, 0]);
        let descending = sorted_rows(&store, 1, true);
        assert_eq!(matching_rows(&counts, Some(&descending)), [0, 2, 1]);
    }

    /// 1,000,000 rows of 20 columns, as `analytics.big_table` holds them.
    fn big_store() -> ResultStore {
        let columns: Arc<[Column]> = (0..20)
            .map(|index| {
                let kind = match index % 4 {
                    0 => TypeKind::Integer,
                    1 => TypeKind::Text,
                    2 => TypeKind::Decimal,
                    _ => TypeKind::DateTimeTz,
                };
                Column::new(format!("c{index}"), "t", kind)
            })
            .collect();
        let mut store = ResultStore::new(columns);
        let mut builder = BatchBuilder::new(20);
        for row in 0..1_000_000u32 {
            for index in 0..20u32 {
                match index % 4 {
                    0 => builder.push_text(&(row * 20 + index).to_string()),
                    1 => builder.push_text(&format!("bucket-{}", (row + index) % 97)),
                    2 => builder.push_text(&format!("{}.{:02}", row % 10_000, index)),
                    _ => builder.push_text("2026-11-14 01:23:45+00"),
                }
            }
            if row % 10_000 == 9_999 {
                store.push(builder.finish());
            }
        }
        store
    }

    /// The cost of a find over a million fetched rows, for `docs/BENCH.md`.
    /// Run it in a release build:
    /// `cargo test --release -p kerno-core --lib -- --ignored --nocapture a_million`.
    #[test]
    #[ignore = "a measurement, slow in a debug build"]
    fn a_million_rows_are_searched() {
        let store = big_store();
        let columns: Vec<usize> = (0..20).collect();
        let stop = AtomicBool::new(false);
        for (needle, case) in [("bucket-42", false), ("BUCKET-42", true), ("é", false)] {
            let matcher = Matcher::new(needle, case, 300).unwrap();
            let started = Instant::now();
            let counts = count_rows(&store, &columns, &matcher, 0..1_000_000, &stop).unwrap();
            let elapsed = started.elapsed();
            let rows = matching_rows(&counts, None).len();
            eprintln!("{needle:?} (case {case}): {rows} rows in {elapsed:?}");
        }
    }
}
