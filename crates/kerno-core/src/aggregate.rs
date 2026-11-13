//! Figures of a selection of cells: how many, how many different, and for
//! numbers their sum, average, minimum and maximum.
//!
//! As DBeaver's aggregate functions (`model/data/aggregate/FunctionCount`,
//! `FunctionCountDistinct`, `FunctionSum`, `FunctionAvg`, `FunctionMin`,
//! `FunctionMax`, plugin `org.jkiss.dbeaver.model`): every cell counts, NULL
//! included; the number functions take the cells that read as numbers and
//! skip the others. Here only cells of a numeric column are numbers.

use std::collections::HashSet;

use crate::result::TypeKind;

/// Different values counted at most: past it, a selection says "over".
// ponytail: the set holds a slice per value (about 40 bytes); a whole 1M x
// 20 result would take most of a gigabyte. Count with a sketch
// (HyperLogLog) if exact figures past this matter.
pub const DISTINCT_CAP: usize = 1_000_000;

/// What the selected numbers add up to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Numbers {
    /// How many cells were numbers.
    pub count: u64,
    pub sum: f64,
    pub min: f64,
    pub max: f64,
}

impl Numbers {
    pub fn average(&self) -> f64 {
        self.sum / self.count as f64
    }
}

/// The figures of a selection.
#[derive(Debug, Clone, PartialEq)]
pub struct Stats {
    /// Cells, NULL included.
    pub count: u64,
    /// Different values, NULL being one; `None` past [`DISTINCT_CAP`].
    pub distinct: Option<u64>,
    /// `None` when no cell is a number.
    pub numbers: Option<Numbers>,
}

/// Takes cells one at a time; they stay borrowed until [`Self::finish`].
#[derive(Debug, Default)]
pub struct Accumulator<'a> {
    count: u64,
    seen: HashSet<Option<&'a [u8]>>,
    numbers: Option<Numbers>,
}

impl<'a> Accumulator<'a> {
    pub fn push(&mut self, kind: TypeKind, cell: Option<&'a [u8]>) {
        self.count += 1;
        if self.seen.len() <= DISTINCT_CAP {
            self.seen.insert(cell);
        }
        let Some(value) = cell
            .filter(|_| kind.is_numeric())
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .and_then(|text| text.trim().parse::<f64>().ok())
            .filter(|value| value.is_finite())
        else {
            return;
        };
        self.numbers = Some(match self.numbers {
            None => Numbers {
                count: 1,
                sum: value,
                min: value,
                max: value,
            },
            Some(numbers) => Numbers {
                count: numbers.count + 1,
                sum: numbers.sum + value,
                min: numbers.min.min(value),
                max: numbers.max.max(value),
            },
        });
    }

    pub fn finish(self) -> Stats {
        Stats {
            count: self.count,
            distinct: (self.seen.len() <= DISTINCT_CAP).then_some(self.seen.len() as u64),
            numbers: self.numbers,
        }
    }
}

/// A figure as the status shows it: whole numbers without a fraction,
/// others with at most six decimals.
pub fn figure(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        return format!("{value:.0}");
    }
    let text = format!("{value:.6}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(cells: &[(TypeKind, Option<&str>)]) -> Stats {
        let mut acc = Accumulator::default();
        for (kind, cell) in cells {
            acc.push(*kind, cell.map(str::as_bytes));
        }
        acc.finish()
    }

    #[test]
    fn every_cell_counts_and_null_is_one_value() {
        let text = TypeKind::Text;
        let got = stats(&[
            (text, Some("a")),
            (text, Some("b")),
            (text, Some("a")),
            (text, None),
            (text, None),
        ]);
        assert_eq!(got.count, 5);
        assert_eq!(got.distinct, Some(3));
        assert_eq!(got.numbers, None);
    }

    #[test]
    fn numbers_of_numeric_columns_are_summed_and_others_skipped() {
        let got = stats(&[
            (TypeKind::Integer, Some("1")),
            (TypeKind::Decimal, Some("2.50")),
            (TypeKind::Float, Some("-4")),
            (TypeKind::Integer, None),
            // A number in a text column is text.
            (TypeKind::Text, Some("100")),
            (TypeKind::Float, Some("NaN")),
        ]);
        assert_eq!(got.count, 6);
        let numbers = got.numbers.unwrap();
        assert_eq!(numbers.count, 3);
        assert_eq!(numbers.sum, -0.5);
        assert_eq!(numbers.min, -4.0);
        assert_eq!(numbers.max, 2.5);
        assert_eq!(numbers.average(), -0.5 / 3.0);
    }

    #[test]
    fn past_the_cap_distinct_values_are_not_counted() {
        let values: Vec<String> = (0..=DISTINCT_CAP).map(|n| n.to_string()).collect();
        let mut acc = Accumulator::default();
        for value in &values {
            acc.push(TypeKind::Text, Some(value.as_bytes()));
        }
        let got = acc.finish();
        assert_eq!(got.count, DISTINCT_CAP as u64 + 1);
        assert_eq!(got.distinct, None);
    }

    #[test]
    fn figures_drop_what_says_nothing() {
        assert_eq!(figure(12.0), "12");
        assert_eq!(figure(-3.0), "-3");
        assert_eq!(figure(2.5), "2.5");
        assert_eq!(figure(10.0 / 3.0), "3.333333");
        assert_eq!(figure(0.1 + 0.2), "0.3");
        assert_eq!(figure(1e20), "100000000000000000000");
    }
}
