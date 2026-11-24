//! Writing rows to an Excel workbook (J12, DBeaver #18393).
//!
//! Rows go to `rust_xlsxwriter` worksheets in its constant-memory mode:
//! each row is written to a temporary file as soon as the next one starts,
//! so memory does not grow with the number of rows. The workbook is put
//! together when [`XlsxWriter::finish`] saves it.
//!
//! Cells keep their type when Excel can hold the value exactly: numbers
//! (at most 15 significant digits and below 2^53, otherwise text so nothing
//! is rounded), booleans, dates, times and timestamps (as Excel dates with a
//! number format; a timestamp with a zone is converted to UTC and its
//! header says so). NULL is an empty cell; everything else, JSON, arrays,
//! UUIDs and binary (`\x` and hex) included, is text.
//!
//! Excel's limits: a sheet holds 1,048,576 rows, so a new sheet (`name (2)`,
//! …) starts when one is full, header repeated; a cell holds 32,767
//! characters (UTF-16 units), so longer text is cut and counted.

use std::borrow::Cow;
use std::io;
use std::path::Path;

use rust_xlsxwriter::{Format, Workbook, XlsxError};

use crate::dialect::hex;
use crate::error::{Error, Result};
use crate::format::parse_bool;
use crate::result::{Column, TypeKind};

/// Rows of an Excel sheet.
const SHEET_ROWS: u32 = 1_048_576;
/// Columns of an Excel sheet.
pub const MAX_COLUMNS: usize = 16_384;
/// Characters of a cell, counted in UTF-16 units as Excel does.
pub const MAX_TEXT: usize = 32_767;
/// Characters of a sheet name.
const MAX_SHEET_NAME: usize = 31;

const DATE_FORMAT: &str = "yyyy-mm-dd";
const TIME_FORMAT: &str = "hh:mm:ss";
const DATE_TIME_FORMAT: &str = "yyyy-mm-dd hh:mm:ss";

/// How a workbook is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XlsxOptions {
    /// Whether the first row of each sheet names the columns.
    pub header: bool,
    /// The name of the first sheet; later ones add ` (2)`, ` (3)`…
    /// Characters Excel refuses in a sheet name are replaced by `_`.
    pub sheet_name: String,
}

impl Default for XlsxOptions {
    fn default() -> Self {
        Self {
            header: true,
            sheet_name: "Sheet1".into(),
        }
    }
}

/// What a finished workbook holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct XlsxReport {
    /// Data rows written.
    pub rows: u64,
    /// Sheets in the workbook.
    pub sheets: u32,
    /// Text cells cut at [`MAX_TEXT`] characters.
    pub truncated: u64,
}

/// A cell as the workbook stores it.
#[derive(Debug, Clone, PartialEq)]
enum Cell<'a> {
    Number(f64),
    Bool(bool),
    /// An Excel serial date (days since 1899-12-30) with the number format
    /// that shows it.
    Date(f64, &'static str),
    Text(Cow<'a, str>),
}

/// Writes rows of `columns` to a workbook, one sheet after another.
pub struct XlsxWriter {
    workbook: Workbook,
    kinds: Vec<TypeKind>,
    header: Option<Vec<String>>,
    sheet_name: String,
    /// Data rows a sheet holds; [`SHEET_ROWS`] less the header, smaller in
    /// tests.
    rows_per_sheet: u32,
    /// Data rows written to the current sheet.
    in_sheet: u32,
    report: XlsxReport,
    formats: [(&'static str, Format); 3],
}

impl XlsxWriter {
    /// A workbook for rows of `columns`; refused when Excel cannot hold
    /// that many columns. Nothing is written before the first row.
    pub fn new(columns: &[Column], options: &XlsxOptions) -> Result<Self> {
        let rows_per_sheet = SHEET_ROWS - u32::from(options.header);
        Self::with_sheet_rows(columns, options, rows_per_sheet)
    }

    fn with_sheet_rows(
        columns: &[Column],
        options: &XlsxOptions,
        rows_per_sheet: u32,
    ) -> Result<Self> {
        if columns.len() > MAX_COLUMNS {
            return Err(invalid(format!(
                "Excel holds at most {MAX_COLUMNS} columns and this result has {}: \
                 choose fewer columns or another format",
                columns.len()
            )));
        }
        let header = options.header.then(|| {
            columns
                .iter()
                .map(|column| match column.kind {
                    TypeKind::DateTimeTz => format!("{} (UTC)", column.name),
                    _ => column.name.clone(),
                })
                .collect()
        });
        let formats = [DATE_FORMAT, TIME_FORMAT, DATE_TIME_FORMAT]
            .map(|code| (code, Format::new().set_num_format(code)));
        Ok(Self {
            workbook: Workbook::new(),
            kinds: columns.iter().map(|column| column.kind).collect(),
            header,
            sheet_name: sanitized_sheet_name(&options.sheet_name),
            rows_per_sheet,
            in_sheet: 0,
            report: XlsxReport::default(),
            formats,
        })
    }

    /// Writes one row: a cell per column, `None` for NULL.
    pub fn write_row(&mut self, cells: &[Option<&[u8]>]) -> Result<()> {
        if cells.len() != self.kinds.len() {
            return Err(invalid(format!(
                "a row has {} cells for {} columns",
                cells.len(),
                self.kinds.len()
            )));
        }
        if self.report.sheets == 0 || self.in_sheet == self.rows_per_sheet {
            self.add_sheet()?;
        }
        let row = self.in_sheet + u32::from(self.header.is_some());
        let sheet = self
            .workbook
            .worksheet_from_index(self.report.sheets as usize - 1);
        let sheet = sheet.map_err(xlsx_error)?;
        for (index, (cell, kind)) in cells.iter().zip(&self.kinds).enumerate() {
            let Some(bytes) = cell else { continue };
            let column = index as u16;
            match cell_value(bytes, *kind) {
                Cell::Number(value) => sheet.write_number(row, column, value),
                Cell::Bool(value) => sheet.write_boolean(row, column, value),
                Cell::Date(serial, code) => {
                    let format = &self
                        .formats
                        .iter()
                        .find(|(c, _)| *c == code)
                        .expect("known")
                        .1;
                    sheet.write_number_with_format(row, column, serial, format)
                }
                Cell::Text(text) => {
                    let (text, cut) = capped(&text);
                    self.report.truncated += u64::from(cut);
                    sheet.write_string(row, column, text)
                }
            }
            .map_err(xlsx_error)?;
        }
        self.in_sheet += 1;
        self.report.rows += 1;
        Ok(())
    }

    /// Starts the next sheet, with the header row.
    fn add_sheet(&mut self) -> Result<()> {
        let name = sheet_name(&self.sheet_name, self.report.sheets + 1);
        let sheet = self
            .workbook
            .add_worksheet_with_constant_memory()
            .map_err(xlsx_error)?;
        sheet.set_name(name).map_err(xlsx_error)?;
        // Wide enough that dates do not show as `#####`.
        for (index, kind) in self.kinds.iter().enumerate() {
            let width = match kind {
                TypeKind::Date => 11.0,
                TypeKind::DateTime | TypeKind::DateTimeTz => 19.0,
                _ => continue,
            };
            sheet
                .set_column_width(index as u16, width)
                .map_err(xlsx_error)?;
        }
        if let Some(header) = &self.header {
            for (index, name) in header.iter().enumerate() {
                let (name, cut) = capped(name);
                self.report.truncated += u64::from(cut);
                sheet
                    .write_string(0, index as u16, name)
                    .map_err(xlsx_error)?;
            }
        }
        self.report.sheets += 1;
        self.in_sheet = 0;
        Ok(())
    }

    /// Saves the workbook to `path`; a result without rows still gets one
    /// sheet, with its header.
    pub fn finish(mut self, path: &Path) -> Result<XlsxReport> {
        if self.report.sheets == 0 {
            self.add_sheet()?;
        }
        self.workbook.save(path).map_err(|error| match error {
            XlsxError::IoError(error) => {
                Error::io(format!("cannot write {}", path.display()), error)
            }
            error => xlsx_error(error),
        })?;
        Ok(self.report)
    }
}

fn invalid(message: String) -> Error {
    Error::io(
        "export",
        io::Error::new(io::ErrorKind::InvalidInput, message),
    )
}

fn xlsx_error(error: XlsxError) -> Error {
    Error::io("writing the workbook", io::Error::other(error.to_string()))
}

/// `text` cut at [`MAX_TEXT`] UTF-16 units, never inside a character, and
/// whether it was cut.
fn capped(text: &str) -> (&str, bool) {
    let mut units = 0;
    for (at, c) in text.char_indices() {
        units += c.len_utf16();
        if units > MAX_TEXT {
            return (&text[..at], true);
        }
    }
    (text, false)
}

/// A sheet name Excel accepts: `[]:*?/\` replaced by `_`, no `'` at
/// either end, never empty.
fn sanitized_sheet_name(name: &str) -> String {
    let name: String = name
        .trim()
        .chars()
        .map(|c| match c {
            '[' | ']' | ':' | '*' | '?' | '/' | '\\' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let name = name.trim_matches('\'').trim();
    if name.is_empty() {
        "Sheet".into()
    } else {
        name.into()
    }
}

/// The name of sheet `number` (from 1): the base, then `base (2)`…, at
/// most 31 characters.
fn sheet_name(base: &str, number: u32) -> String {
    let suffix = if number == 1 {
        String::new()
    } else {
        format!(" ({number})")
    };
    let room = MAX_SHEET_NAME - suffix.chars().count();
    let base: String = base.chars().take(room).collect();
    format!("{}{suffix}", base.trim_end())
}

/// How a cell of `kind` holding `bytes` (the server's text) is stored.
fn cell_value(bytes: &[u8], kind: TypeKind) -> Cell<'_> {
    if kind == TypeKind::Binary {
        return Cell::Text(Cow::Owned(format!("\\x{}", hex(bytes))));
    }
    let text = String::from_utf8_lossy(bytes);
    let typed = match kind {
        TypeKind::Integer | TypeKind::Decimal => exact_number(&text).map(Cell::Number),
        TypeKind::Float => text
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
            .map(Cell::Number),
        TypeKind::Boolean => parse_bool(bytes).map(Cell::Bool),
        TypeKind::Date => date_serial(&text).map(|serial| Cell::Date(serial, DATE_FORMAT)),
        TypeKind::Time => time_fraction(&text).map(|day| Cell::Date(day, TIME_FORMAT)),
        TypeKind::DateTime => {
            date_time_serial(&text, false).map(|serial| Cell::Date(serial, DATE_TIME_FORMAT))
        }
        TypeKind::DateTimeTz => {
            date_time_serial(&text, true).map(|serial| Cell::Date(serial, DATE_TIME_FORMAT))
        }
        _ => None,
    };
    typed.unwrap_or(Cell::Text(text))
}

/// A decimal written in plain digits, when a double holds it exactly as
/// Excel shows it: at most 15 significant digits and below 2^53.
fn exact_number(text: &str) -> Option<f64> {
    let digits = text.strip_prefix(['-', '+']).unwrap_or(text);
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    if whole.is_empty() && fraction.is_empty()
        || !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let significant = format!("{whole}{}", fraction.trim_end_matches('0'));
    if significant.trim_start_matches('0').len() > 15 {
        return None;
    }
    let value: f64 = text.parse().ok()?;
    (value.abs() < 9_007_199_254_740_992.0).then_some(value)
}

/// Days from 1970-01-01 to a date of the proleptic Gregorian calendar
/// (Howard Hinnant's `days_from_civil`).
pub(crate) fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Days from 1899-12-30 (Excel's day 0) to 1970-01-01.
const EXCEL_EPOCH: i64 = 25_569;
/// Excel's day of 1900-03-01: earlier serials count a 29 February 1900
/// that never was, so earlier dates are written as text.
const FIRST_SERIAL: i64 = 61;
/// Excel's day of 9999-12-31, its last.
const LAST_SERIAL: i64 = 2_958_465;

/// `YYYY-MM-DD` as an Excel day number, when Excel has that day.
fn date_serial(text: &str) -> Option<f64> {
    let bytes = text.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let year: i64 = digits(&text[..4])?;
    let month: i64 = digits(&text[5..7])?;
    let day: i64 = digits(&text[8..])?;
    let length = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => return None,
    };
    if !(1..=length).contains(&day) {
        return None;
    }
    let serial = days_from_civil(year, month, day) + EXCEL_EPOCH;
    (FIRST_SERIAL..=LAST_SERIAL)
        .contains(&serial)
        .then_some(serial as f64)
}

/// Plain ASCII digits as a number.
fn digits(text: &str) -> Option<i64> {
    (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

/// `HH:MM:SS[.fff]` as seconds since midnight, below 24 hours.
fn seconds_of_day(text: &str) -> Option<f64> {
    let bytes = text.as_bytes();
    if bytes.len() < 8 || bytes[2] != b':' || bytes[5] != b':' {
        return None;
    }
    let hours = digits(&text[..2])?;
    let minutes = digits(&text[3..5])?;
    let (seconds, fraction) = text[6..].split_once('.').unwrap_or((&text[6..], ""));
    let seconds = digits(seconds).filter(|_| seconds.len() == 2)?;
    if hours > 23 || minutes > 59 || seconds > 59 {
        return None;
    }
    let fraction = match fraction {
        "" => 0.0,
        digits if digits.bytes().all(|b| b.is_ascii_digit()) => {
            format!("0.{digits}").parse::<f64>().ok()?
        }
        _ => return None,
    };
    Some((hours * 3600 + minutes * 60 + seconds) as f64 + fraction)
}

/// A time of day as the fraction of a day Excel stores. A time with a
/// zone, or outside 0–24 h (a MySQL `TIME` span), is text.
fn time_fraction(text: &str) -> Option<f64> {
    seconds_of_day(text).map(|seconds| seconds / 86_400.0)
}

/// `YYYY-MM-DD HH:MM:SS[.fff]`, with a zone offset (`+01`, `+05:30`,
/// `-03:00:15`) when `zoned`, as an Excel serial; zoned values in UTC.
fn date_time_serial(text: &str, zoned: bool) -> Option<f64> {
    let (date, time) = text.split_once([' ', 'T'])?;
    let (time, offset) = if zoned {
        let at = time.rfind(['+', '-'])?;
        (&time[..at], offset_seconds(&time[at..])?)
    } else {
        (time, 0)
    };
    let day = date_serial(date)?;
    let serial = day + (seconds_of_day(time)? - offset as f64) / 86_400.0;
    (FIRST_SERIAL as f64..(LAST_SERIAL + 1) as f64)
        .contains(&serial)
        .then_some(serial)
}

/// `+HH`, `+HH:MM` or `+HH:MM:SS` (or `-`) as seconds east of UTC.
fn offset_seconds(text: &str) -> Option<i64> {
    let sign = match text.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let mut seconds = 0;
    let mut parts = 0;
    for (index, part) in text[1..].split(':').enumerate() {
        if part.len() != 2 || index > 2 {
            return None;
        }
        seconds += digits(part)? * [3600, 60, 1][index];
        parts += 1;
    }
    (parts > 0).then_some(sign * seconds)
}

/// Reading a workbook back, for tests: the cells of each sheet.
#[cfg(feature = "test-support")]
pub mod read {
    use std::collections::BTreeMap;
    use std::io::Read;
    use std::path::Path;

    /// A cell read back from a workbook.
    #[derive(Debug, Clone, PartialEq)]
    pub enum ReadCell {
        Number(f64),
        Bool(bool),
        Text(String),
        /// A number shown with a date or time format (its format code).
        Date(f64, String),
    }

    /// A sheet: its name and its rows, each a map from column index to
    /// cell (an empty cell is absent).
    #[derive(Debug, Clone, PartialEq)]
    pub struct ReadSheet {
        pub name: String,
        pub rows: Vec<BTreeMap<usize, ReadCell>>,
    }

    /// Every sheet of the workbook at `path`, in order. Reads what
    /// [`super::XlsxWriter`] writes: inline strings, numbers, booleans and
    /// the number formats of `styles.xml`.
    pub fn read_sheets(path: &Path) -> Vec<ReadSheet> {
        let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
        let mut entry = |name: &str| {
            let mut text = String::new();
            archive
                .by_name(name)
                .unwrap_or_else(|_| panic!("{name} in the workbook"))
                .read_to_string(&mut text)
                .unwrap();
            text
        };
        let workbook = entry("xl/workbook.xml");
        let styles = entry("xl/styles.xml");
        let formats = number_formats(&styles);
        elements(&workbook, "sheet")
            .enumerate()
            .map(|(index, sheet)| {
                let xml = entry(&format!("xl/worksheets/sheet{}.xml", index + 1));
                ReadSheet {
                    name: unescape(&attribute(sheet, "name").unwrap_or_default()),
                    rows: rows(&xml, &formats),
                }
            })
            .collect()
    }

    /// The format code of each cell style (`s`), from `styles.xml`.
    fn number_formats(styles: &str) -> Vec<Option<String>> {
        let codes: BTreeMap<String, String> = elements(styles, "numFmt")
            .filter_map(|element| {
                Some((
                    attribute(element, "numFmtId")?,
                    unescape(&attribute(element, "formatCode")?),
                ))
            })
            .collect();
        let xfs = styles.split("<cellXfs").nth(1).unwrap_or_default();
        let xfs = xfs.split("</cellXfs>").next().unwrap_or_default();
        elements(xfs, "xf")
            .map(|xf| codes.get(&attribute(xf, "numFmtId")?).cloned())
            .collect()
    }

    fn rows(xml: &str, formats: &[Option<String>]) -> Vec<BTreeMap<usize, ReadCell>> {
        let mut rows = Vec::new();
        for row in xml.split("<row ").skip(1) {
            let row = row.split("</row>").next().unwrap_or_default();
            let number: usize = attribute(row, "r").unwrap().parse().unwrap();
            while rows.len() < number {
                rows.push(BTreeMap::new());
            }
            for cell in row.split("<c ").skip(1) {
                let reference = attribute(cell, "r").unwrap();
                let column = reference
                    .bytes()
                    .take_while(u8::is_ascii_uppercase)
                    .fold(0, |n, b| n * 26 + usize::from(b - b'A' + 1))
                    - 1;
                let between = |open: &str, close: &str| {
                    let start = cell.find(open)? + open.len();
                    let start = start + cell[start..].find('>')? + 1;
                    let end = start + cell[start..].find(close)?;
                    Some(unescape(&cell[start..end]))
                };
                let value = match attribute(cell, "t").as_deref() {
                    Some("inlineStr") => ReadCell::Text(between("<t", "</t>").unwrap_or_default()),
                    Some("str") | Some("s") => ReadCell::Text(between("<v", "</v>").unwrap()),
                    Some("b") => ReadCell::Bool(between("<v", "</v>").unwrap() == "1"),
                    _ => {
                        let Some(value) = between("<v", "</v>") else {
                            continue;
                        };
                        let number = value.parse().unwrap();
                        let style: usize = attribute(cell, "s").map_or(0, |s| s.parse().unwrap());
                        match formats.get(style).cloned().flatten() {
                            Some(code) => ReadCell::Date(number, code),
                            None => ReadCell::Number(number),
                        }
                    }
                };
                rows[number - 1].insert(column, value);
            }
        }
        rows
    }

    /// The opening tags `<name …>` of `xml`, without `<`.
    fn elements<'a>(xml: &'a str, name: &'a str) -> impl Iterator<Item = &'a str> {
        let open = format!("<{name} ");
        xml.split(open.as_str())
            .skip(1)
            .map(|rest| rest.split('>').next().unwrap_or_default())
            .collect::<Vec<_>>()
            .into_iter()
    }

    /// The value of `name="…"` in the start of `tag`.
    fn attribute(tag: &str, name: &str) -> Option<String> {
        let head = tag.split('>').next()?;
        let key = format!(" {name}=\"");
        let start = format!(" {head}").find(&key)? + key.len() - 1;
        let rest = &head[start..];
        Some(rest[..rest.find('"')?].to_string())
    }

    /// XML entities and Excel's `_xHHHH_` escapes undone.
    fn unescape(text: &str) -> String {
        let text = text
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'");
        let mut out = String::new();
        let mut rest = text.as_str();
        while let Some(at) = rest.find("_x") {
            let code = rest
                .get(at + 2..at + 6)
                .filter(|_| rest.get(at + 6..at + 7) == Some("_"));
            match code.and_then(|code| u32::from_str_radix(code, 16).ok()) {
                Some(code) => {
                    out.push_str(&rest[..at]);
                    out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                    rest = &rest[at + 7..];
                }
                None => {
                    out.push_str(&rest[..at + 2]);
                    rest = &rest[at + 2..];
                }
            }
        }
        out.push_str(rest);
        out.replace("&amp;", "&")
    }
}

#[cfg(test)]
mod tests {
    use super::read::{ReadCell, read_sheets};
    use super::*;

    fn column(name: &str, kind: TypeKind) -> Column {
        Column::new(name, "t", kind)
    }

    #[test]
    fn numbers_are_numbers_only_when_excel_holds_them_exactly() {
        let number = |text: &str, kind| match cell_value(text.as_bytes(), kind) {
            Cell::Number(value) => Some(value),
            _ => None,
        };
        let decimal = |text| number(text, TypeKind::Decimal);
        assert_eq!(decimal("12.50"), Some(12.5));
        assert_eq!(decimal("-0"), Some(0.0));
        assert_eq!(decimal("0.000123"), Some(0.000123));
        // 15 significant digits, then 16.
        assert_eq!(decimal("123456789012345"), Some(123_456_789_012_345.0));
        assert_eq!(decimal("1234567890.12345"), Some(1_234_567_890.123_45));
        assert_eq!(decimal("1234567890.123456"), None);
        assert_eq!(decimal("1234567890123456"), None);
        // Trailing zeros of a fraction are not significant.
        assert_eq!(decimal("12345678901234.50000"), Some(12_345_678_901_234.5));
        // Around 2^53: too many digits either way.
        assert_eq!(number("9007199254740991", TypeKind::Integer), None);
        assert_eq!(number("9007199254740993", TypeKind::Integer), None);
        assert_eq!(number("-42", TypeKind::Integer), Some(-42.0));
        for text in ["NaN", "Infinity", "-Infinity", "$1,234.50", "1e5", "", "."] {
            assert_eq!(decimal(text), None, "{text}");
        }
        // A binary floating point number is a double already.
        assert_eq!(number("1e+20", TypeKind::Float), Some(1e20));
        assert_eq!(number("-0.1", TypeKind::Float), Some(-0.1));
        for text in ["NaN", "Infinity", "-Infinity", "inf"] {
            assert_eq!(number(text, TypeKind::Float), None, "{text}");
        }
    }

    #[test]
    fn dates_and_times_are_excel_serials_in_their_range() {
        let date = |text: &str| date_serial(text);
        assert_eq!(date("1900-03-01"), Some(61.0));
        assert_eq!(date("1970-01-01"), Some(25_569.0));
        assert_eq!(date("2024-02-29"), Some(45_351.0));
        assert_eq!(date("9999-12-31"), Some(2_958_465.0));
        for text in [
            "1900-02-28",
            "0044-03-15 BC",
            "infinity",
            "-infinity",
            "0000-00-00",
            "2023-02-29",
            "2024-13-01",
            "10000-01-01",
            "24-02-29",
        ] {
            assert_eq!(date(text), None, "{text}");
        }
        assert_eq!(time_fraction("12:00:00"), Some(0.5));
        assert_eq!(time_fraction("06:00:00.5"), Some(21_600.5 / 86_400.0));
        for text in [
            "24:00:00",
            "-01:00:00",
            "838:59:59",
            "12:00:00+02",
            "1:00:00",
        ] {
            assert_eq!(time_fraction(text), None, "{text}");
        }
        let at = |text: &str, zoned| date_time_serial(text, zoned);
        assert_eq!(at("2024-02-29 12:00:00", false), Some(45_351.5));
        assert_eq!(at("2024-02-29 12:00:00.000000", false), Some(45_351.5));
        // Converted to UTC, whatever the offset's shape.
        assert_eq!(at("2024-02-29 13:00:00+01", true), Some(45_351.5));
        assert_eq!(at("2024-02-29 17:30:00+05:30", true), Some(45_351.5));
        assert_eq!(at("2024-02-29 11:59:45-00:00:15", true), Some(45_351.5));
        // A zoned value without its zone, or with one it cannot read.
        assert_eq!(at("2024-02-29 12:00:00", true), None);
        assert_eq!(at("2024-02-29 12:00:00+1", true), None);
        assert_eq!(at("infinity", true), None);
        // The first second of Excel's range, reached through the offset.
        assert_eq!(at("1900-03-01 00:30:00+01", true), None);
    }

    #[test]
    fn other_kinds_are_text_and_text_stays_text() {
        let text = |bytes: &[u8], kind| match cell_value(bytes, kind) {
            Cell::Text(text) => Some(text.into_owned()),
            _ => None,
        };
        assert_eq!(text(b"=1+1", TypeKind::Text).as_deref(), Some("=1+1"));
        assert_eq!(
            text(&[0xde, 0xad], TypeKind::Binary).as_deref(),
            Some("\\xdead")
        );
        assert_eq!(text(b"maybe", TypeKind::Boolean).as_deref(), Some("maybe"));
        assert_eq!(cell_value(b"t", TypeKind::Boolean), Cell::Bool(true));
        for kind in [
            TypeKind::Uuid,
            TypeKind::Json,
            TypeKind::Array,
            TypeKind::Interval,
        ] {
            assert!(text(b"{1,2}", kind).is_some(), "{kind:?}");
        }
    }

    #[test]
    fn long_text_is_cut_on_a_character_by_utf16_units() {
        let plain = "a".repeat(MAX_TEXT);
        assert_eq!(capped(&plain), (plain.as_str(), false));
        // An emoji is two UTF-16 units: the one that would cross the limit
        // goes whole.
        let emoji = format!("{}\u{1f600}", "a".repeat(MAX_TEXT - 1));
        assert_eq!(capped(&emoji).0.len(), MAX_TEXT - 1);
        assert!(capped(&emoji).1);
    }

    #[test]
    fn sheet_names_are_what_excel_accepts() {
        assert_eq!(sanitized_sheet_name("shop.orders"), "shop.orders");
        assert_eq!(sanitized_sheet_name(" a/b:c*?[d] "), "a_b_c___d_");
        assert_eq!(sanitized_sheet_name("'quoted'"), "quoted");
        assert_eq!(sanitized_sheet_name("  "), "Sheet");
        let long = "x".repeat(40);
        assert_eq!(sheet_name(&long, 1).chars().count(), 31);
        assert_eq!(sheet_name(&long, 12), format!("{} (12)", "x".repeat(26)));
        assert_eq!(sheet_name("orders", 2), "orders (2)");
    }

    #[test]
    fn a_workbook_is_written_typed_and_split_into_sheets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.xlsx");
        let columns = [
            column("id", TypeKind::Integer),
            column("name", TypeKind::Text),
            column("at", TypeKind::DateTimeTz),
            column("ok", TypeKind::Boolean),
        ];
        let options = XlsxOptions {
            header: true,
            sheet_name: "orders".into(),
        };
        let mut writer = XlsxWriter::with_sheet_rows(&columns, &options, 2).unwrap();
        let long = "\u{e9}".repeat(MAX_TEXT + 5);
        let rows: [[Option<&[u8]>; 4]; 5] = [
            [
                Some(b"1"),
                Some(b"a & <b> \x01"),
                Some(b"2024-02-29 13:00:00+01"),
                Some(b"t"),
            ],
            [Some(b"2"), None, None, Some(b"f")],
            [Some(b"3"), Some(long.as_bytes()), None, None],
            [Some(b"4"), Some(b"=1+1"), None, None],
            [Some(b"5"), Some(b""), None, None],
        ];
        for row in &rows {
            writer.write_row(row).unwrap();
        }
        assert!(writer.write_row(&[None]).is_err());
        let report = writer.finish(&path).unwrap();
        assert_eq!(
            report,
            XlsxReport {
                rows: 5,
                sheets: 3,
                truncated: 1
            }
        );
        let sheets = read_sheets(&path);
        let names: Vec<&str> = sheets.iter().map(|sheet| sheet.name.as_str()).collect();
        assert_eq!(names, ["orders", "orders (2)", "orders (3)"]);
        for sheet in &sheets {
            assert_eq!(
                sheet.rows[0].get(&2),
                Some(&ReadCell::Text("at (UTC)".into()))
            );
        }
        let first = &sheets[0].rows;
        assert_eq!(first.len(), 3);
        assert_eq!(first[1][&0], ReadCell::Number(1.0));
        assert_eq!(first[1][&1], ReadCell::Text("a & <b> \x01".into()));
        assert_eq!(
            first[1][&2],
            ReadCell::Date(45_351.5, DATE_TIME_FORMAT.into())
        );
        assert_eq!(first[1][&3], ReadCell::Bool(true));
        assert!(!first[2].contains_key(&1), "NULL is an empty cell");
        let second = &sheets[1].rows;
        let ReadCell::Text(cut) = &second[1][&1] else {
            panic!("{:?}", second[1]);
        };
        assert_eq!(cut.chars().count(), MAX_TEXT);
        assert_eq!(second[2][&1], ReadCell::Text("=1+1".into()));
        assert_eq!(sheets[2].rows[1][&0], ReadCell::Number(5.0));
    }

    #[test]
    fn a_result_without_rows_or_header_still_makes_a_workbook() {
        let dir = tempfile::tempdir().unwrap();
        let columns = [column("id", TypeKind::Integer)];
        let path = dir.path().join("empty.xlsx");
        let writer = XlsxWriter::new(&columns, &XlsxOptions::default()).unwrap();
        assert_eq!(writer.finish(&path).unwrap().sheets, 1);
        let sheets = read_sheets(&path);
        assert_eq!(sheets[0].name, "Sheet1");
        assert_eq!(sheets[0].rows.len(), 1);

        let path = dir.path().join("bare.xlsx");
        let options = XlsxOptions {
            header: false,
            ..XlsxOptions::default()
        };
        let mut writer = XlsxWriter::new(&columns, &options).unwrap();
        writer.write_row(&[Some(b"7")]).unwrap();
        writer.finish(&path).unwrap();
        assert_eq!(read_sheets(&path)[0].rows[0][&0], ReadCell::Number(7.0));
    }

    #[test]
    fn too_many_columns_are_refused_in_plain_words() {
        let columns: Vec<Column> = (0..=MAX_COLUMNS)
            .map(|index| column(&format!("c{index}"), TypeKind::Text))
            .collect();
        let error = XlsxWriter::new(&columns, &XlsxOptions::default())
            .err()
            .unwrap();
        assert!(error.to_string().contains("16384 columns"), "{error}");
    }
}
