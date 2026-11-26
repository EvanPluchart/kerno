//! How a cell is shown: one line of text for the results grid, and the
//! value viewer's JSON, hex and size helpers.

use std::borrow::Cow;
use std::fmt::Write as _;

use crate::dialect::hex;
use crate::result::{Column, TypeKind};

/// What a cell is, for the grid to pick its colour and alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CellStyle {
    /// SQL `NULL`.
    Null,
    /// A number: right-aligned.
    Number,
    /// True or false.
    Boolean,
    /// Character data, enum labels included.
    Text,
    /// A date, a time, a timestamp or an interval.
    Temporal,
    /// A JSON document.
    Json,
    /// Raw bytes, shown as a hex preview.
    Binary,
    /// Anything else (UUID, array, bit string, engine-specific types).
    Other,
}

impl CellStyle {
    fn of(kind: TypeKind) -> Self {
        match kind {
            TypeKind::Integer | TypeKind::Float | TypeKind::Decimal => Self::Number,
            TypeKind::Boolean => Self::Boolean,
            TypeKind::Text | TypeKind::Enum => Self::Text,
            TypeKind::Date
            | TypeKind::Time
            | TypeKind::DateTime
            | TypeKind::DateTimeTz
            | TypeKind::Interval => Self::Temporal,
            TypeKind::Json => Self::Json,
            TypeKind::Binary => Self::Binary,
            TypeKind::Uuid | TypeKind::Array | TypeKind::Bit | TypeKind::Other => Self::Other,
        }
    }
}

/// A cell as the grid shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellText<'a> {
    /// The text to draw, on one line.
    pub text: Cow<'a, str>,
    /// What the cell is.
    pub style: CellStyle,
    /// Whether `text` is only the beginning of the value.
    pub truncated: bool,
}

/// Bytes of a binary cell shown before the preview stops.
const BINARY_PREVIEW_BYTES: usize = 16;

/// Stands for a control character, and for bytes that are not UTF-8.
const PLACEHOLDER: char = '\u{fffd}';

/// The text and style of a cell in the results grid.
///
/// - NULL is `NULL` with [`CellStyle::Null`]; an empty value is empty text.
/// - Booleans are spelled `true` and `false` whichever way the server
///   printed them (`t`/`f`, `1`/`0`); anything else is shown as it is.
/// - Binary is `\x` and the first bytes in hex (16 at most, fewer when
///   `max_chars` is small), then `…` and the size of the whole value when
///   there is more: `\x89504e47… (1.2 kB)`. The size is not counted in
///   `max_chars`.
/// - Everything else is the server's text on one line: a line break is
///   `↵`, a tab `→`, any other control character and any byte that is not
///   UTF-8 `�`. It is cut after `max_chars` characters (Unicode scalar
///   values), with `truncated` set and no ellipsis added.
///
/// Meant to be called for every visible cell on every frame: the text is
/// borrowed from the cell unless a character had to be replaced, and only
/// the first `4 * max_chars` bytes of the cell are read.
pub fn display<'a>(cell: Option<&'a [u8]>, column: &Column, max_chars: usize) -> CellText<'a> {
    let Some(bytes) = cell else {
        return CellText {
            text: Cow::Borrowed("NULL"),
            style: CellStyle::Null,
            truncated: false,
        };
    };
    let style = CellStyle::of(column.kind);
    match column.kind {
        TypeKind::Binary => {
            let shown = BINARY_PREVIEW_BYTES
                .min(max_chars.saturating_sub(2) / 2)
                .min(bytes.len());
            let truncated = shown < bytes.len();
            let mut text = format!("\\x{}", hex(&bytes[..shown]));
            if truncated {
                let _ = write!(text, "\u{2026} ({})", byte_size(bytes.len() as u64));
            }
            return CellText {
                text: Cow::Owned(text),
                style,
                truncated,
            };
        }
        TypeKind::Boolean => {
            if let Some(value) = parse_bool(bytes) {
                return CellText {
                    text: Cow::Borrowed(if value { "true" } else { "false" }),
                    style,
                    truncated: false,
                };
            }
        }
        _ => {}
    }
    let (text, truncated) = one_line(bytes, max_chars);
    CellText {
        text,
        style,
        truncated,
    }
}

/// The boolean a cell holds, in the spellings the servers print: `t`/`f`
/// (PostgreSQL), `1`/`0` (MySQL `TINYINT(1)`), `true`/`false`.
pub fn parse_bool(bytes: &[u8]) -> Option<bool> {
    let is = |spellings: [&[u8]; 3]| spellings.iter().any(|s| bytes.eq_ignore_ascii_case(s));
    if is([b"t", b"1", b"true"]) {
        Some(true)
    } else if is([b"f", b"0", b"false"]) {
        Some(false)
    } else {
        None
    }
}

/// The first `max_chars` characters of a cell on one line, and whether the
/// cell holds more.
fn one_line(bytes: &[u8], max_chars: usize) -> (Cow<'_, str>, bool) {
    // A character is at most 4 bytes, so this prefix holds every character
    // that can be shown: the rest of a huge value is never read.
    let window = &bytes[..bytes.len().min(max_chars.saturating_mul(4))];
    let more = window.len() < bytes.len();
    match std::str::from_utf8(window) {
        Ok(text) => cut(text, more, max_chars),
        // The window ends inside a character; the text before it is valid
        // and has at least `max_chars` characters.
        Err(error) if more && error.error_len().is_none() => cut(
            std::str::from_utf8(&window[..error.valid_up_to()]).unwrap_or_default(),
            more,
            max_chars,
        ),
        Err(_) => {
            let lossy = String::from_utf8_lossy(window);
            let (text, truncated) = cut(&lossy, more, max_chars);
            (Cow::Owned(text.into_owned()), truncated)
        }
    }
}

/// `text` cut after `max_chars` characters, with the characters that would
/// break the line replaced. `more` tells whether the value goes on after
/// `text`. Borrows `text` when nothing was replaced.
// ponytail: cuts between Unicode scalar values, so a cut can separate a
// letter from its combining accent or split an emoji sequence; cut on
// grapheme clusters (unicode-segmentation) if that shows in the grid.
fn cut(text: &str, more: bool, max_chars: usize) -> (Cow<'_, str>, bool) {
    let breaks_line = |c: char| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}');
    let mut shown = 0;
    let mut clean = text.len();
    for (index, c) in text.char_indices() {
        if shown == max_chars {
            return (Cow::Borrowed(&text[..index]), true);
        }
        if breaks_line(c) {
            clean = index;
            break;
        }
        shown += 1;
    }
    if clean == text.len() {
        return (Cow::Borrowed(text), more);
    }

    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..clean]);
    let mut rest = text[clean..].chars().peekable();
    while let Some(c) = rest.next() {
        if shown == max_chars {
            return (Cow::Owned(out), true);
        }
        out.push(match c {
            '\r' => {
                // CR LF is one line break.
                rest.next_if_eq(&'\n');
                '\u{21b5}'
            }
            '\n' | '\u{2028}' | '\u{2029}' => '\u{21b5}',
            '\t' => '\u{2192}',
            c if c.is_control() => PLACEHOLDER,
            c => c,
        });
        shown += 1;
    }
    (Cow::Owned(out), more)
}

/// Deepest nesting [`pretty_json`] indents. A document nested deeper is
/// either broken or hostile, and its indentation alone would take
/// quadratic space.
const MAX_PRETTY_DEPTH: usize = 256;

/// `text` re-indented with two spaces per level, or `None` when it is not
/// JSON (or is nested more than 256 levels deep).
///
/// Only the whitespace between tokens changes: numbers, strings and keys
/// are copied as they are written, so big integers, trailing zeros,
/// escapes and duplicate keys all survive.
pub fn pretty_json(text: &str) -> Option<String> {
    rewrite_json(text, true)
}

/// `text` without the whitespace between its tokens, so on one line, or
/// `None` when it is not JSON. Values are copied as they are written.
pub fn compact_json(text: &str) -> Option<String> {
    rewrite_json(text, false)
}

/// Whether `text` is a JSON number, and nothing else: `-0`, `12.50`,
/// `1e10`, but not `.5`, `00012`, `+1`, `NaN` or `Infinity`.
pub(crate) fn is_json_number(text: &str) -> bool {
    number_end(text.as_bytes(), 0) == Some(text.len())
}

/// Checks that `text` is one JSON value and copies it token by token with
/// new whitespace. Keeps its own stack of open containers, so nesting
/// depth costs memory, not call stack.
fn rewrite_json(text: &str, pretty: bool) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    // The open containers, innermost last: `{` or `[`.
    let mut open: Vec<u8> = Vec::new();
    let line = |out: &mut String, depth: usize| {
        if pretty {
            out.push('\n');
            out.extend(std::iter::repeat_n("  ", depth));
        }
    };
    // Copies the key and the colon at `at`; returns where the value starts.
    let key = |out: &mut String, at: usize| {
        let end = string_end(bytes, at)?;
        out.push_str(&text[at..end]);
        let colon = skip_whitespace(bytes, end);
        (bytes.get(colon) == Some(&b':')).then(|| {
            out.push_str(if pretty { ": " } else { ":" });
            skip_whitespace(bytes, colon + 1)
        })
    };

    let mut at = skip_whitespace(bytes, 0);
    loop {
        // A value starts at `at`.
        let start = *bytes.get(at)?;
        let end = match start {
            b'{' | b'[' => {
                let inside = skip_whitespace(bytes, at + 1);
                let close = if start == b'{' { b'}' } else { b']' };
                if bytes.get(inside) == Some(&close) {
                    out.push_str(if start == b'{' { "{}" } else { "[]" });
                    inside + 1
                } else {
                    if pretty && open.len() == MAX_PRETTY_DEPTH {
                        return None;
                    }
                    open.push(start);
                    out.push(start as char);
                    line(&mut out, open.len());
                    at = if start == b'{' {
                        key(&mut out, inside)?
                    } else {
                        inside
                    };
                    continue;
                }
            }
            b'"' => string_end(bytes, at)?,
            b'-' | b'0'..=b'9' => number_end(bytes, at)?,
            _ => {
                let word = [&b"true"[..], b"false", b"null"]
                    .into_iter()
                    .find(|word| bytes[at..].starts_with(word))?;
                at + word.len()
            }
        };
        if !matches!(start, b'{' | b'[') {
            out.push_str(&text[at..end]);
        }
        at = end;

        // After a value: close the containers it ends, or go on to the
        // next value of the innermost one.
        loop {
            at = skip_whitespace(bytes, at);
            let Some(&container) = open.last() else {
                return (at == bytes.len()).then_some(out);
            };
            match (*bytes.get(at)?, container) {
                (b',', _) => {
                    out.push(',');
                    line(&mut out, open.len());
                    at = skip_whitespace(bytes, at + 1);
                    if container == b'{' {
                        at = key(&mut out, at)?;
                    }
                    break;
                }
                (b'}', b'{') | (b']', b'[') => {
                    open.pop();
                    line(&mut out, open.len());
                    out.push(bytes[at] as char);
                    at += 1;
                }
                _ => return None,
            }
        }
    }
}

fn skip_whitespace(bytes: &[u8], mut at: usize) -> usize {
    while matches!(bytes.get(at), Some(b' ' | b'\t' | b'\n' | b'\r')) {
        at += 1;
    }
    at
}

/// The end of the JSON string that starts at `at`, after its closing quote.
fn string_end(bytes: &[u8], at: usize) -> Option<usize> {
    if bytes.get(at) != Some(&b'"') {
        return None;
    }
    let mut at = at + 1;
    loop {
        match *bytes.get(at)? {
            b'"' => return Some(at + 1),
            b'\\' => match *bytes.get(at + 1)? {
                b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => at += 2,
                b'u' => {
                    let digits = bytes.get(at + 2..at + 6)?;
                    if !digits.iter().all(u8::is_ascii_hexdigit) {
                        return None;
                    }
                    at += 6;
                }
                _ => return None,
            },
            0..=0x1f => return None,
            _ => at += 1,
        }
    }
}

/// The end of the JSON number that starts at `at`. What follows it is the
/// caller's to check.
fn number_end(bytes: &[u8], mut at: usize) -> Option<usize> {
    let digits = |mut at: usize| {
        let start = at;
        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
        (at > start).then_some(at)
    };
    if bytes.get(at) == Some(&b'-') {
        at += 1;
    }
    // No leading zeros: `0` alone, or digits that start with 1 to 9.
    at = if bytes.get(at) == Some(&b'0') {
        at + 1
    } else {
        digits(at)?
    };
    if bytes.get(at) == Some(&b'.') {
        at = digits(at + 1)?;
    }
    if matches!(bytes.get(at), Some(b'e' | b'E')) {
        at += 1;
        if matches!(bytes.get(at), Some(b'+' | b'-')) {
            at += 1;
        }
        at = digits(at)?;
    }
    Some(at)
}

/// A hex dump of `bytes`: rows of 16 with the offset, the bytes in hex and
/// their ASCII text, as `hexdump -C` prints them. No trailing line break.
///
/// ```text
/// 00000000  89 50 4e 47 0d 0a 1a 0a  00 00 00 0d 49 48 44 52  |.PNG........IHDR|
/// ```
///
/// The dump is about five times the size of `bytes`: give it the part of
/// a large value that is looked at, not the whole value.
pub fn hex_dump(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(16) * 79);
    for (row, chunk) in bytes.chunks(16).enumerate() {
        if row > 0 {
            out.push('\n');
        }
        let _ = write!(out, "{:08x} ", row * 16);
        for column in 0..16 {
            if column % 8 == 0 {
                out.push(' ');
            }
            match chunk.get(column) {
                Some(byte) => {
                    let _ = write!(out, "{byte:02x} ");
                }
                None => out.push_str("   "),
            }
        }
        out.push_str(" |");
        out.extend(chunk.iter().map(|&byte| {
            if byte.is_ascii_graphic() || byte == b' ' {
                byte as char
            } else {
                '.'
            }
        }));
        out.push('|');
    }
    out
}

/// What a part of [`hex_dump`]'s text is, for the viewers to draw it in a
/// text level of the theme (K81): the bytes themselves are left plain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HexPart {
    /// The row's offset.
    Offset,
    /// The ASCII gutter, its bars included.
    Gutter,
    /// A byte of the gutter that has no printable character (drawn `.`,
    /// as a real `.` byte is): dimmer than the gutter.
    Unprintable,
}

/// The parts of `hex_dump(bytes)` to draw apart, as byte ranges of its text,
/// in order and apart from each other: a row's gutter is cut around its
/// unprintable runs.
pub fn hex_parts(bytes: &[u8]) -> Vec<(std::ops::Range<usize>, HexPart)> {
    // A row: 8 offset digits and 2 spaces, 16 bytes of 3 characters and
    // 1 space between the halves, then a space, the bars and the gutter.
    const GUTTER: usize = 60;
    let mut parts: Vec<(std::ops::Range<usize>, HexPart)> =
        Vec::with_capacity(bytes.len().div_ceil(16) * 3);
    let mut start = 0;
    for chunk in bytes.chunks(16) {
        parts.push((start..start + 8, HexPart::Offset));
        let bar = std::iter::once(HexPart::Gutter);
        let kinds = bar
            .clone()
            .chain(chunk.iter().map(|&byte| {
                if byte.is_ascii_graphic() || byte == b' ' {
                    HexPart::Gutter
                } else {
                    HexPart::Unprintable
                }
            }))
            .chain(bar);
        let mut row_first = true;
        for (column, kind) in kinds.enumerate() {
            let at = start + GUTTER + column;
            match parts.last_mut() {
                Some((range, last)) if !row_first && *last == kind => range.end = at + 1,
                _ => parts.push((at..at + 1, kind)),
            }
            row_first = false;
        }
        start += GUTTER + chunk.len() + 3;
    }
    parts
}

/// A size in bytes for people: `512 B`, `1.2 kB`, `3.4 MB`. Units are
/// powers of 1024; one decimal below 10, none from 10 up.
pub fn byte_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["kB", "MB", "GB", "TB", "PB", "EB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    // 1023.5 kB and more would print as 1024 kB: that is 1.0 MB.
    while value >= 1023.5 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if value < 9.95 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// What an integer that looks like a Unix time is, for the value viewer
/// (Beekeeper #941): `Unix seconds → 2026-11-14 01:23:45 UTC`. Seconds
/// from 2000-01-01 to 2100-01-01 (946684800 to 4102444800), or the same
/// span in milliseconds or microseconds; `None` for anything else.
pub fn unix_time_hint(text: &str) -> Option<String> {
    const FIRST: i64 = 946_684_800;
    const LAST: i64 = 4_102_444_800;
    let text = text.trim();
    let value: i64 = text.strip_prefix('+').unwrap_or(text).parse().ok()?;
    let (unit, per_second, digits) = [
        ("seconds", 1, 0),
        ("milliseconds", 1_000, 3),
        ("microseconds", 1_000_000, 6),
    ]
    .into_iter()
    .find(|(_, per_second, _)| (FIRST * per_second..=LAST * per_second).contains(&value))?;
    let (seconds, fraction) = (value / per_second, value % per_second);
    let (days, time) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    let (year, month, day) = civil_from_days(days);
    let mut out = format!(
        "Unix {unit} \u{2192} {year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        time / 3600,
        time % 3600 / 60,
        time % 60
    );
    if digits > 0 {
        let _ = write!(out, ".{fraction:0digits$}");
    }
    out.push_str(" UTC");
    Some(out)
}

/// The proleptic Gregorian date `days` after 1970-01-01. Howard Hinnant's
/// `civil_from_days` (public domain, "chrono-Compatible Low-Level Date
/// Algorithms").
pub(crate) fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = if month_from_march < 10 {
        month_from_march + 3
    } else {
        month_from_march - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_that_look_like_unix_times_say_when() {
        let hint = |text: &str| unix_time_hint(text);
        assert_eq!(hint("946684799"), None);
        assert_eq!(
            hint("946684800").as_deref(),
            Some("Unix seconds \u{2192} 2000-01-01 00:00:00 UTC")
        );
        assert_eq!(
            hint("4102444800").as_deref(),
            Some("Unix seconds \u{2192} 2100-01-01 00:00:00 UTC")
        );
        assert_eq!(hint("4102444801"), None);
        assert_eq!(
            hint("1794619425").as_deref(),
            Some("Unix seconds \u{2192} 2026-11-14 01:23:45 UTC")
        );
        // Leap day, and the end of a day.
        assert_eq!(
            hint("951868799").as_deref(),
            Some("Unix seconds \u{2192} 2000-02-29 23:59:59 UTC")
        );
        // Milliseconds and microseconds, with their fraction.
        assert_eq!(
            hint("1794619425678").as_deref(),
            Some("Unix milliseconds \u{2192} 2026-11-14 01:23:45.678 UTC")
        );
        assert_eq!(
            hint("946684800000").as_deref(),
            Some("Unix milliseconds \u{2192} 2000-01-01 00:00:00.000 UTC")
        );
        assert_eq!(
            hint("4102444800000000").as_deref(),
            Some("Unix microseconds \u{2192} 2100-01-01 00:00:00.000000 UTC")
        );
        assert_eq!(
            hint("1794619425000042").as_deref(),
            Some("Unix microseconds \u{2192} 2026-11-14 01:23:45.000042 UTC")
        );
        // Between the spans, outside them, or not a whole number: nothing.
        for text in [
            "4102444800001",
            "946684799999",
            "0",
            "-1794619425",
            "1794619425.5",
            "",
            "abc",
            "99999999999999999999",
        ] {
            assert_eq!(hint(text), None, "{text}");
        }
        assert!(hint(" +1794619425 ").is_some());
    }

    fn show(cell: Option<&[u8]>, kind: TypeKind, max_chars: usize) -> (String, CellStyle, bool) {
        let column = Column::new("c", "t", kind);
        let shown = display(cell, &column, max_chars);
        (shown.text.into_owned(), shown.style, shown.truncated)
    }

    /// A text cell as shown: `(text, truncated)`.
    fn text(value: &str, max_chars: usize) -> (String, bool) {
        let (text, style, truncated) = show(Some(value.as_bytes()), TypeKind::Text, max_chars);
        assert_eq!(style, CellStyle::Text);
        (text, truncated)
    }

    fn is_borrowed(cell: &[u8], kind: TypeKind, max_chars: usize) -> bool {
        let column = Column::new("c", "t", kind);
        matches!(
            display(Some(cell), &column, max_chars).text,
            Cow::Borrowed(_)
        )
    }

    #[test]
    fn null_is_not_an_empty_string() {
        for kind in [TypeKind::Text, TypeKind::Integer, TypeKind::Binary] {
            assert_eq!(
                show(None, kind, 10),
                ("NULL".into(), CellStyle::Null, false)
            );
        }
        assert_eq!(
            show(Some(b""), TypeKind::Text, 10),
            (String::new(), CellStyle::Text, false)
        );
        // The four letters are text, not a NULL.
        assert_eq!(
            show(Some(b"NULL"), TypeKind::Text, 10),
            ("NULL".into(), CellStyle::Text, false)
        );
        // NULL is not cut.
        assert_eq!(
            show(None, TypeKind::Text, 0),
            ("NULL".into(), CellStyle::Null, false)
        );
    }

    #[test]
    fn style_follows_the_kind_of_the_column() {
        let cases = [
            (TypeKind::Integer, CellStyle::Number),
            (TypeKind::Float, CellStyle::Number),
            (TypeKind::Decimal, CellStyle::Number),
            (TypeKind::Boolean, CellStyle::Boolean),
            (TypeKind::Text, CellStyle::Text),
            (TypeKind::Enum, CellStyle::Text),
            (TypeKind::Date, CellStyle::Temporal),
            (TypeKind::Time, CellStyle::Temporal),
            (TypeKind::DateTime, CellStyle::Temporal),
            (TypeKind::DateTimeTz, CellStyle::Temporal),
            (TypeKind::Interval, CellStyle::Temporal),
            (TypeKind::Json, CellStyle::Json),
            (TypeKind::Binary, CellStyle::Binary),
            (TypeKind::Uuid, CellStyle::Other),
            (TypeKind::Array, CellStyle::Other),
            (TypeKind::Bit, CellStyle::Other),
            (TypeKind::Other, CellStyle::Other),
        ];
        for (kind, style) in cases {
            assert_eq!(show(Some(b"x"), kind, 10).1, style, "{kind:?}");
        }
    }

    #[test]
    fn plain_values_are_borrowed_not_copied() {
        assert!(is_borrowed(b"hello world", TypeKind::Text, 100));
        assert!(is_borrowed(b"-12345.6789", TypeKind::Decimal, 100));
        assert!(is_borrowed(
            b"2024-02-29 12:00:00+00",
            TypeKind::DateTimeTz,
            100
        ));
        assert!(is_borrowed(
            "h\u{e9}llo \u{1f600}".as_bytes(),
            TypeKind::Text,
            100
        ));
        assert!(is_borrowed(b"", TypeKind::Text, 100));
        assert!(is_borrowed(b"t", TypeKind::Boolean, 100));
        // A cut is a prefix of the cell: still borrowed.
        assert!(is_borrowed(b"hello world", TypeKind::Text, 5));
        // Only a rewritten value is copied.
        assert!(!is_borrowed(b"hello\nworld", TypeKind::Text, 100));
    }

    #[test]
    fn numbers_are_shown_as_the_server_printed_them() {
        for value in ["-0", "1e10", ".5", "00012", "NaN", "12.50"] {
            assert_eq!(
                show(Some(value.as_bytes()), TypeKind::Decimal, 100),
                (value.to_string(), CellStyle::Number, false)
            );
        }
        let digits = "1234567890123456789012345678901234567890";
        assert_eq!(
            show(Some(digits.as_bytes()), TypeKind::Integer, 100).0,
            digits
        );
    }

    #[test]
    fn booleans_are_spelled_out() {
        for (cell, shown) in [
            ("t", "true"),
            ("f", "false"),
            ("1", "true"),
            ("0", "false"),
            ("true", "true"),
            ("false", "false"),
            ("TRUE", "true"),
            ("F", "false"),
            // Anything else is shown as it is.
            ("yes", "yes"),
            ("", ""),
            ("2", "2"),
        ] {
            assert_eq!(
                show(Some(cell.as_bytes()), TypeKind::Boolean, 100),
                (shown.to_string(), CellStyle::Boolean, false),
                "{cell:?}"
            );
        }
        // Only in boolean columns.
        assert_eq!(show(Some(b"t"), TypeKind::Text, 100).0, "t");
        assert_eq!(show(Some(b"1"), TypeKind::Integer, 100).0, "1");
    }

    #[test]
    fn text_is_shown_on_one_line() {
        assert_eq!(
            text(
                "a\nb\r\nc\rd\te\u{0}f\u{7f}g\u{85}h\u{2028}i\u{2029}j",
                100
            ),
            (
                "a\u{21b5}b\u{21b5}c\u{21b5}d\u{2192}e\u{fffd}f\u{fffd}g\u{fffd}h\u{21b5}i\u{21b5}j"
                    .to_string(),
                false
            )
        );
        assert_eq!(text("\n", 100), ("\u{21b5}".to_string(), false));
        assert_eq!(
            text("\r\n\r\n", 100),
            ("\u{21b5}\u{21b5}".to_string(), false)
        );
    }

    #[test]
    fn text_is_cut_at_a_number_of_characters() {
        assert_eq!(text("hello", 5), ("hello".to_string(), false));
        assert_eq!(text("hello", 4), ("hell".to_string(), true));
        assert_eq!(text("hello", 0), (String::new(), true));
        assert_eq!(text("", 0), (String::new(), false));
        // Characters, not bytes.
        assert_eq!(
            text("h\u{e9}llo w\u{f6}rld", 4),
            ("h\u{e9}ll".to_string(), true)
        );
        assert_eq!(
            text("\u{e9}\u{e9}\u{e9}", 3),
            ("\u{e9}\u{e9}\u{e9}".to_string(), false)
        );
        // Four-byte characters at the boundary are kept whole or left out.
        let faces = "\u{1f600}\u{1f601}\u{1f602}";
        assert_eq!(text(faces, 2), ("\u{1f600}\u{1f601}".to_string(), true));
        assert_eq!(text(faces, 3), (faces.to_string(), false));
        assert_eq!(
            text("a\u{1f600}\u{1f601}\u{1f602}", 2),
            ("a\u{1f600}".to_string(), true)
        );
        // A combining mark is a character of its own: the cut may fall
        // between a letter and its accent, never inside either.
        assert_eq!(text("e\u{301}e\u{301}", 3), ("e\u{301}e".to_string(), true));
        assert_eq!(
            text("e\u{301}e\u{301}", 4),
            ("e\u{301}e\u{301}".to_string(), false)
        );
    }

    #[test]
    fn replaced_characters_count_once_when_cutting() {
        // CR LF is one line break, so one character.
        assert_eq!(text("ab\r\n", 3), ("ab\u{21b5}".to_string(), false));
        assert_eq!(text("ab\r\nc", 3), ("ab\u{21b5}".to_string(), true));
        assert_eq!(text("a\tb\tc", 4), ("a\u{2192}b\u{2192}".to_string(), true));
        assert_eq!(text("\n\n\n", 2), ("\u{21b5}\u{21b5}".to_string(), true));
    }

    #[test]
    fn a_huge_cell_is_cut_without_reading_all_of_it() {
        let big = "x".repeat(5 * 1024 * 1024);
        let column = Column::new("c", "text", TypeKind::Text);
        let shown = display(Some(big.as_bytes()), &column, 200);
        assert_eq!(shown.text, "x".repeat(200));
        assert!(shown.truncated);
        assert!(matches!(shown.text, Cow::Borrowed(_)));

        // Bytes past the part that is shown are not looked at: a broken
        // tail does not turn the cell into a lossy copy.
        let mut broken_tail = vec![b'a'; 1000];
        broken_tail.push(0xff);
        let shown = display(Some(&broken_tail), &column, 10);
        assert_eq!(shown.text, "aaaaaaaaaa");
        assert!(shown.truncated);
        assert!(matches!(shown.text, Cow::Borrowed(_)));

        // A line break inside the part that is shown.
        let lines = "line\n".repeat(1024 * 1024);
        let shown = display(Some(lines.as_bytes()), &column, 7);
        assert_eq!(shown.text, "line\u{21b5}li");
        assert!(shown.truncated);
    }

    #[test]
    fn invalid_utf8_is_shown_not_fatal() {
        let cases: [(&[u8], &str); 5] = [
            (b"ab\xffcd", "ab\u{fffd}cd"),
            // A character cut short at the end of the cell.
            (b"ab\xe2\x82", "ab\u{fffd}"),
            (b"\xff\xfe", "\u{fffd}\u{fffd}"),
            (b"\x80", "\u{fffd}"),
            (b"ok\xc3\xa9\xc3", "ok\u{e9}\u{fffd}"),
        ];
        for (cell, shown) in cases {
            assert_eq!(
                show(Some(cell), TypeKind::Text, 100),
                (shown.to_string(), CellStyle::Text, false),
                "{cell:?}"
            );
        }
        assert_eq!(
            show(Some(b"ab\xffcd"), TypeKind::Text, 3),
            ("ab\u{fffd}".to_string(), CellStyle::Text, true)
        );
        // Cut in the middle of a long run of broken bytes.
        assert_eq!(
            show(Some(&[0xff; 100]), TypeKind::Json, 2),
            ("\u{fffd}\u{fffd}".to_string(), CellStyle::Json, true)
        );
    }

    #[test]
    fn a_cut_never_splits_a_character_of_a_long_value() {
        // The part of the cell that is read ends inside a character.
        for max_chars in 0..12 {
            let value = format!("a{}", "\u{1f600}".repeat(20));
            let (shown, truncated) = text(&value, max_chars);
            assert_eq!(shown.chars().count(), max_chars);
            assert!(value.starts_with(&shown));
            assert!(truncated);
        }
        let value = "\u{e9}".repeat(100);
        assert_eq!(text(&value, 100), (value.clone(), false));
        assert_eq!(text(&value, 99), ("\u{e9}".repeat(99), true));
        assert_eq!(text(&value, usize::MAX), (value.clone(), false));
    }

    #[test]
    fn binary_is_a_hex_preview() {
        let binary = |cell: &[u8], max_chars| show(Some(cell), TypeKind::Binary, max_chars);
        assert_eq!(
            binary(&[0x89, 0x50, 0x4e, 0x47], 100),
            ("\\x89504e47".to_string(), CellStyle::Binary, false)
        );
        assert_eq!(
            binary(&[], 100),
            ("\\x".to_string(), CellStyle::Binary, false)
        );
        assert_eq!(
            binary(&[0, 0, 0xff], 100),
            ("\\x0000ff".to_string(), CellStyle::Binary, false)
        );
        // Sixteen bytes at most, then the size of the whole value.
        let mut png = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
        png.resize(1229, 0xab);
        assert_eq!(
            binary(&png, 100),
            (
                "\\x89504e470d0a1a0aabababababababab\u{2026} (1.2 kB)".to_string(),
                CellStyle::Binary,
                true
            )
        );
        assert_eq!(
            binary(&png[..16], 100),
            (
                "\\x89504e470d0a1a0aabababababababab".to_string(),
                CellStyle::Binary,
                false
            )
        );
        assert_eq!(
            binary(&png[..17], 100).0,
            "\\x89504e470d0a1a0aabababababababab\u{2026} (17 B)"
        );
        // Fewer bytes when the cell is narrow.
        assert_eq!(
            binary(&png, 10),
            (
                "\\x89504e47\u{2026} (1.2 kB)".to_string(),
                CellStyle::Binary,
                true
            )
        );
        assert_eq!(binary(&png, 0).0, "\\x\u{2026} (1.2 kB)");
        // Text that happens to be in a binary column is not decoded.
        assert_eq!(binary(b"hi\n", 100).0, "\\x68690a");
    }

    #[test]
    fn json_is_indented_by_two_spaces() {
        assert_eq!(
            pretty_json(r#"{"a":1,"b":[1,2,{"c":null}],"d":"x\"y","e":true}"#).unwrap(),
            r#"{
  "a": 1,
  "b": [
    1,
    2,
    {
      "c": null
    }
  ],
  "d": "x\"y",
  "e": true
}"#
        );
        // Whatever the spacing was.
        assert_eq!(
            pretty_json(" {\n\t\"a\" : [ 1 , 2 ] ,\r\n \"b\":{ }\n} ").unwrap(),
            "{\n  \"a\": [\n    1,\n    2\n  ],\n  \"b\": {}\n}"
        );
        assert_eq!(pretty_json("[ ]").unwrap(), "[]");
        assert_eq!(pretty_json("[[],{}]").unwrap(), "[\n  [],\n  {}\n]");
        // A scalar is a document too.
        for scalar in ["42", "-1.5e3", "\"x\"", "true", "false", "null"] {
            assert_eq!(pretty_json(scalar).as_deref(), Some(scalar));
            assert_eq!(
                pretty_json(&format!("  {scalar}\n")).as_deref(),
                Some(scalar)
            );
        }
    }

    #[test]
    fn json_numbers_and_strings_are_kept_as_written() {
        assert_eq!(
            pretty_json(
                "[12345678901234567890123456789012345678901234567890,1.10,-0,1E+2,0.1e-7,-12.5]"
            )
            .unwrap(),
            "[\n  12345678901234567890123456789012345678901234567890,\n  1.10,\n  -0,\n  1E+2,\n  \
             0.1e-7,\n  -12.5\n]"
        );
        let strings = r#"["{[,:]}","a\\\"b","\u00e9\ud83d\ude00\/","é😀"," spaced  out ",""]"#;
        assert_eq!(
            pretty_json(strings).unwrap(),
            r#"[
  "{[,:]}",
  "a\\\"b",
  "\u00e9\ud83d\ude00\/",
  "é😀",
  " spaced  out ",
  ""
]"#
        );
        // Keys too, duplicates included.
        assert_eq!(
            pretty_json(r#"{"a b":1,"a b":2,"":3}"#).unwrap(),
            "{\n  \"a b\": 1,\n  \"a b\": 2,\n  \"\": 3\n}"
        );
    }

    #[test]
    fn what_is_not_json_is_refused() {
        for text in [
            "",
            "  ",
            "{",
            "}",
            "[",
            "]",
            "{\"a\":}",
            "{\"a\":1,}",
            "{,}",
            "[1,]",
            "[,1]",
            "[1 2]",
            "{\"a\" 1}",
            "{\"a\":1 \"b\":2}",
            "{\"a\"}",
            "{1:2}",
            "{a:1}",
            "[1}",
            "{\"a\":1]",
            "01",
            "1.",
            ".5",
            "+1",
            "-",
            "1e",
            "1e+",
            "--1",
            "0x10",
            "NaN",
            "Infinity",
            "'a'",
            "tru",
            "truex",
            "nul",
            "True",
            "\"abc",
            "\"a\nb\"",
            "\"a\tb\"",
            "\"\\x\"",
            "\"\\u12\"",
            "\"\\u12g4\"",
            "\"a\" \"b\"",
            "1 2",
            "{\"a\":1}x",
            "[1],",
            "hello",
        ] {
            assert_eq!(pretty_json(text), None, "{text:?}");
            assert_eq!(compact_json(text), None, "{text:?}");
        }
    }

    #[test]
    fn json_is_compacted_without_touching_values() {
        assert_eq!(
            compact_json(" {\n\t\"a\" : [ 1.10 , \"x  y\" ] ,\r\n \"b\":{ }\n} ").unwrap(),
            r#"{"a":[1.10,"x  y"],"b":{}}"#
        );
        assert_eq!(compact_json(" 42 ").unwrap(), "42");
    }

    #[test]
    fn deeply_nested_json_does_not_overflow() {
        let nested = |depth: usize| format!("{}1{}", "[".repeat(depth), "]".repeat(depth));
        let deep = nested(100_000);
        assert_eq!(compact_json(&deep).as_deref(), Some(deep.as_str()));
        // Indenting it would take gigabytes of spaces.
        assert_eq!(pretty_json(&deep), None);
        assert!(pretty_json(&nested(MAX_PRETTY_DEPTH)).is_some());
        assert_eq!(pretty_json(&nested(MAX_PRETTY_DEPTH + 1)), None);
        assert_eq!(compact_json(&"[".repeat(100_000)), None);
    }

    #[test]
    fn json_numbers() {
        for number in [
            "0",
            "-0",
            "7",
            "12.5",
            "-12.50",
            "1e10",
            "1E+10",
            "1.5e-7",
            "1234567890123456789012345678901234567890",
        ] {
            assert!(is_json_number(number), "{number}");
        }
        for other in [
            "",
            "-",
            ".5",
            "5.",
            "00012",
            "01",
            "+1",
            "1e",
            "1e+",
            "NaN",
            "Infinity",
            "-Infinity",
            " 1",
            "1 ",
            "1,000",
            "$1.00",
            "0x10",
            "1.2.3",
            "--1",
        ] {
            assert!(!is_json_number(other), "{other:?}");
        }
    }

    #[test]
    fn hex_dump_has_rows_of_sixteen_bytes() {
        let png = [
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x01, 0x00,
        ];
        assert_eq!(
            hex_dump(&png),
            format!(
                "00000000  89 50 4e 47 0d 0a 1a 0a  00 00 00 0d 49 48 44 52  |.PNG........IHDR|\n\
                 00000010  00 00 01 00 {}|....|",
                " ".repeat(38)
            )
        );
        assert_eq!(
            hex_dump(&png[..16]),
            "00000000  89 50 4e 47 0d 0a 1a 0a  00 00 00 0d 49 48 44 52  |.PNG........IHDR|"
        );
        // The second half of a row keeps its place when the row is short.
        assert_eq!(
            hex_dump(b"Hello, ~wor"),
            format!(
                "00000000  48 65 6c 6c 6f 2c 20 7e  77 6f 72 {}|Hello, ~wor|",
                " ".repeat(16)
            )
        );
        assert_eq!(hex_dump(&[]), "");
        // Every row is as wide as the others, and offsets count in hex.
        let dump = hex_dump(&[0x7f; 300]);
        let rows: Vec<&str> = dump.lines().collect();
        assert_eq!(rows.len(), 19);
        assert!(rows.iter().all(|row| row.find('|') == Some(60)));
        assert!(rows[18].starts_with("00000120  7f 7f"));
        assert!(rows[18].ends_with("|............|"));
    }

    /// K81: the offsets, the gutter and its unprintable bytes, where the
    /// dump has them; a real `.` byte is printable.
    #[test]
    fn hex_parts_name_the_offsets_and_the_gutter_of_each_row() {
        let bytes: Vec<u8> = b"A.\x00\x01B\x7fCDEFGHIJKL\x02".to_vec();
        let dump = hex_dump(&bytes);
        let parts = hex_parts(&bytes);
        let text = |part: &(std::ops::Range<usize>, HexPart)| &dump[part.0.clone()];
        let named: Vec<(&str, HexPart)> = parts.iter().map(|p| (text(p), p.1)).collect();
        assert_eq!(
            named,
            [
                ("00000000", HexPart::Offset),
                ("|A.", HexPart::Gutter),
                ("..", HexPart::Unprintable),
                ("B", HexPart::Gutter),
                (".", HexPart::Unprintable),
                ("CDEFGHIJKL|", HexPart::Gutter),
                ("00000010", HexPart::Offset),
                ("|", HexPart::Gutter),
                (".", HexPart::Unprintable),
                ("|", HexPart::Gutter),
            ]
        );
        // The parts follow each other without overlapping.
        assert!(
            parts
                .windows(2)
                .all(|pair| pair[0].0.end <= pair[1].0.start)
        );
        assert!(hex_parts(&[]).is_empty());
        // Every range of a large dump falls inside its text.
        let large: Vec<u8> = (0..=255u8).cycle().take(4099).collect();
        let dump = hex_dump(&large);
        let parts = hex_parts(&large);
        assert!(
            parts
                .windows(2)
                .all(|pair| pair[0].0.end <= pair[1].0.start)
        );
        for (range, part) in parts {
            let shown = &dump[range.clone()];
            let line = dump[..range.start].rfind('\n').map_or(0, |at| at + 1);
            assert!(!shown.contains('\n'), "{shown:?}");
            match part {
                HexPart::Offset => assert_eq!((range.start - line, shown.len()), (0, 8)),
                HexPart::Gutter => assert!(range.start - line >= 60, "{shown:?}"),
                HexPart::Unprintable => {
                    assert!(range.start - line > 60 && shown.chars().all(|c| c == '.'))
                }
            }
        }
    }

    #[test]
    fn byte_sizes() {
        for (bytes, shown) in [
            (0, "0 B"),
            (1, "1 B"),
            (512, "512 B"),
            (1023, "1023 B"),
            (1024, "1.0 kB"),
            (1229, "1.2 kB"),
            (1536, "1.5 kB"),
            (10_137, "9.9 kB"),
            (10_240, "10 kB"),
            (10_239, "10 kB"),
            (15_800, "15 kB"),
            (1_047_552, "1023 kB"),
            (1_048_575, "1.0 MB"),
            (1_048_576, "1.0 MB"),
            (3_565_158, "3.4 MB"),
            (5 * 1024 * 1024, "5.0 MB"),
            (1_073_741_824, "1.0 GB"),
            (1_099_511_627_776, "1.0 TB"),
            (u64::MAX, "16 EB"),
        ] {
            assert_eq!(byte_size(bytes), shown, "{bytes}");
        }
    }
}
