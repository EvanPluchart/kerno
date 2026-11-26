//! Times that carry their zone, read in the computer's own (K82, D-148).
//!
//! A value is never changed: [`parse`] reads a time written with an offset
//! (`2026-06-08 16:17:04.768895+00`, `2026-06-08T16:17:04Z`, `16:17:04+02`),
//! and [`Zone::show`] says what it is in a zone, with the offset that zone's
//! rules give for that instant (a June date and a December date differ in
//! Europe), never today's. A date and time without an offset has no zone:
//! [`parse`] refuses it.

use std::fmt::Write as _;
use std::sync::Arc;

use chrono::{
    DateTime, Datelike as _, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, TimeZone as _,
    Timelike as _,
};

/// A time with its offset, as a value writes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Zoned {
    /// `None` for a time of day without a date (PostgreSQL's `timetz`).
    date: Option<NaiveDate>,
    time: NaiveTime,
    offset: FixedOffset,
    /// The digits of its fraction of a second, as written (at most 9).
    digits: usize,
}

impl Zoned {
    /// Whether it has a date: a time of day alone takes today's offset.
    pub fn has_date(&self) -> bool {
        self.date.is_some()
    }

    /// The instant in UTC, `today` giving the date of a time of day alone.
    fn instant(&self, today: NaiveDate) -> Option<DateTime<FixedOffset>> {
        let local = NaiveDateTime::new(self.date.unwrap_or(today), self.time);
        self.offset.from_local_datetime(&local).single()
    }
}

/// Reads `text` as a time with a zone: a date (`YYYY-MM-DD`), a space or a
/// `T`, then a time (`HH:MM`, `HH:MM:SS`, a fraction of any length), or a time
/// alone; then `Z` or an offset (`+02`, `+02:00`, `+0200`, PostgreSQL's
/// historical `+00:09:21`). `None` for anything else: no offset, ` BC`,
/// `infinity`, `24:00`, an hour, a minute or an offset out of range. A
/// second of 60 is a leap second.
pub fn parse(text: &str) -> Option<Zoned> {
    let mut cursor = Cursor(text.as_bytes());
    let date = match text.as_bytes().get(4) {
        Some(b'-') => {
            let year = cursor.number(4)?;
            cursor.byte(b'-')?;
            let month = cursor.number(2)?;
            cursor.byte(b'-')?;
            let day = cursor.number(2)?;
            if !(cursor.byte(b' ').is_some() || cursor.byte(b'T').is_some()) {
                return None;
            }
            Some(NaiveDate::from_ymd_opt(year as i32, month, day)?)
        }
        _ => None,
    };
    let hour = cursor.number(2)?;
    cursor.byte(b':')?;
    let minute = cursor.number(2)?;
    let mut second = 0;
    let (mut nanos, mut digits) = (0, 0);
    let seconds = cursor.byte(b':').is_some();
    if seconds {
        second = cursor.number(2)?;
        if cursor.byte(b'.').is_some() {
            let fraction = cursor.digits();
            if fraction.is_empty() {
                return None;
            }
            digits = fraction.len().min(9);
            nanos = fraction[..digits]
                .iter()
                .fold(0u32, |nanos, digit| nanos * 10 + u32::from(digit - b'0'))
                * 10u32.pow((9 - digits) as u32);
        }
    }
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    // A leap second: chrono writes it as the 59th with a second's more nanos.
    let time = match second {
        60 => NaiveTime::from_hms_nano_opt(hour, minute, 59, 1_000_000_000 + nanos)?,
        _ => NaiveTime::from_hms_nano_opt(hour, minute, second, nanos)?,
    };
    let offset = cursor.offset()?;
    if !cursor.0.is_empty() {
        return None;
    }
    // A time without a date must name its seconds: `12:30-14:00` is opening
    // hours, not 12:30 at UTC−14.
    if date.is_none() && !seconds {
        return None;
    }
    Some(Zoned {
        date,
        time,
        offset,
        digits,
    })
}

/// What reading a value has left to read.
struct Cursor<'a>(&'a [u8]);

impl Cursor<'_> {
    fn byte(&mut self, wanted: u8) -> Option<()> {
        let (&first, rest) = self.0.split_first()?;
        (first == wanted).then(|| self.0 = rest)
    }

    /// Exactly `count` digits.
    fn number(&mut self, count: usize) -> Option<u32> {
        let digits = self.0.get(..count)?;
        if !digits.iter().all(u8::is_ascii_digit) {
            return None;
        }
        self.0 = &self.0[count..];
        Some(
            digits
                .iter()
                .fold(0, |value, digit| value * 10 + u32::from(digit - b'0')),
        )
    }

    /// The digits that follow, however many.
    fn digits(&mut self) -> &[u8] {
        let end = self
            .0
            .iter()
            .position(|byte| !byte.is_ascii_digit())
            .unwrap_or(self.0.len());
        let (digits, rest) = self.0.split_at(end);
        self.0 = rest;
        digits
    }

    /// `Z`, or a sign and hours, then minutes and seconds with or without
    /// colons.
    fn offset(&mut self) -> Option<FixedOffset> {
        if self.byte(b'Z').is_some() || self.byte(b'z').is_some() {
            return FixedOffset::east_opt(0);
        }
        let sign = match self.0.first()? {
            b'+' => 1,
            b'-' => -1,
            _ => return None,
        };
        self.0 = &self.0[1..];
        let hours = self.number(2)?;
        let colon = self.byte(b':').is_some();
        let minutes = match colon || !self.0.is_empty() {
            true => self.number(2)?,
            false => 0,
        };
        let seconds = match colon && self.byte(b':').is_some() {
            true => self.number(2)?,
            false => 0,
        };
        if hours > 23 || minutes > 59 || seconds > 59 {
            return None;
        }
        FixedOffset::east_opt(sign * (hours * 3600 + minutes * 60 + seconds) as i32)
    }
}

/// The offset a zone's rules give at an instant, given in UTC.
pub type OffsetAt = dyn Fn(&NaiveDateTime) -> FixedOffset + Send + Sync;

/// A time zone: its name, and its offset at any instant.
#[derive(Clone)]
pub struct Zone {
    name: String,
    offset_at: Arc<OffsetAt>,
}

impl std::fmt::Debug for Zone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Zone").field(&self.name).finish()
    }
}

impl Zone {
    pub fn new(
        name: impl Into<String>,
        offset_at: impl Fn(&NaiveDateTime) -> FixedOffset + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            offset_at: Arc::new(offset_at),
        }
    }

    /// The computer's zone: its offsets from the system's zone rules
    /// (chrono's `Local`: on macOS and Linux the zone file `TZ` names, else
    /// `/etc/localtime`; on Windows the system's rules for each year), and
    /// its name: `TZ` when it is set (Unix), as `Local` reads it, else the
    /// system's (`iana-time-zone`: macOS' system zone, Linux's
    /// `/etc/localtime` link, Windows' zone mapped to its IANA name). An error
    /// says, in plain words, why it cannot be read.
    pub fn system() -> Result<Zone, String> {
        let tz = std::env::var("TZ")
            .ok()
            .map(|tz| tz.trim_start_matches(':').to_string())
            .filter(|tz| cfg!(unix) && !tz.is_empty());
        let name = match tz {
            Some(tz) => tz,
            None => iana_time_zone::get_timezone().map_err(|error| {
                format!("this computer's time zone could not be read ({error})")
            })?,
        };
        Ok(Zone::new(name, |utc| {
            chrono::Local.offset_from_utc_datetime(utc)
        }))
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// What `zoned` is in this zone: `2026-06-08 18:17:04.768895 +02:00`,
    /// its fraction as written; a time of day alone as `18:17:04 +02:00`, on
    /// `today`. `None` only past chrono's calendar (no 4-digit year is).
    pub fn show(&self, zoned: &Zoned, today: NaiveDate) -> Option<String> {
        let instant = zoned.instant(today)?;
        let offset = (self.offset_at)(&instant.naive_utc());
        let here = instant.with_timezone(&offset);
        let mut out = String::new();
        if zoned.has_date() {
            let _ = write!(out, "{} ", here.format("%Y-%m-%d"));
        }
        let time = here.time();
        // A leap second keeps its 60.
        let (second, nanos) = match time.nanosecond() {
            nanos @ 1_000_000_000.. => (60, nanos - 1_000_000_000),
            nanos => (time.second(), nanos),
        };
        let _ = write!(out, "{:02}:{:02}:{second:02}", time.hour(), time.minute());
        if zoned.digits > 0 {
            let fraction = format!("{nanos:09}");
            let _ = write!(out, ".{}", &fraction[..zoned.digits]);
        }
        let _ = write!(out, " {}", offset_text(offset));
        Some(out)
    }
}

impl Zone {
    /// [`Self::show`] with today's date (UTC) for a time of day alone.
    pub fn show_today(&self, zoned: &Zoned) -> Option<String> {
        self.show(zoned, chrono::Utc::now().date_naive())
    }
}

/// `+02:00`, `-05:00`, `+05:30`; seconds only when there are some.
fn offset_text(offset: FixedOffset) -> String {
    let seconds = offset.local_minus_utc();
    let sign = if seconds < 0 { '-' } else { '+' };
    let seconds = seconds.unsigned_abs();
    let mut text = format!("{sign}{:02}:{:02}", seconds / 3600, seconds % 3600 / 60);
    if !seconds.is_multiple_of(60) {
        let _ = write!(text, ":{:02}", seconds % 60);
    }
    text
}

/// A time with a zone found in a JSON document: where, and what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// `ordered_at`, `items[0].shipped_at`; empty for the document itself.
    pub path: String,
    pub zoned: Zoned,
}

/// The strings of the JSON document `text` that are times with a zone, in
/// the document's order: at most `limit`, and how many there are in all.
/// Nothing when `text` is not JSON.
pub fn times_in_json(text: &str, limit: usize) -> (Vec<Found>, usize) {
    use serde::de::DeserializeSeed as _;
    let mut found = Vec::new();
    let mut count = 0;
    let mut each = |path: &str, zoned| {
        count += 1;
        if found.len() < limit {
            found.push(Found {
                path: path.to_string(),
                zoned,
            });
        }
    };
    let mut path = String::new();
    let mut reader = serde_json::Deserializer::from_str(text);
    let walked = Walk {
        path: &mut path,
        found: &mut each,
    }
    .deserialize(&mut reader)
    .and_then(|()| reader.end());
    match walked {
        Ok(()) => (found, count),
        Err(_) => (Vec::new(), 0),
    }
}

/// Reads a JSON document in its own order (a map of serde_json's would sort
/// the keys) and calls `found` with the path of each string that is a time
/// with a zone. serde_json refuses documents nested deeper than 128: the
/// recursion is bounded.
struct Walk<'a> {
    path: &'a mut String,
    found: &'a mut dyn FnMut(&str, Zoned),
}

impl<'de> serde::de::DeserializeSeed<'de> for Walk<'_> {
    type Value = ();

    fn deserialize<D: serde::Deserializer<'de>>(self, reader: D) -> Result<(), D::Error> {
        reader.deserialize_any(self)
    }
}

impl<'de> serde::de::Visitor<'de> for Walk<'_> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_str<E>(self, text: &str) -> Result<(), E> {
        if let Some(zoned) = parse(text) {
            (self.found)(self.path, zoned);
        }
        Ok(())
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut items: A) -> Result<(), A::Error> {
        let at = self.path.len();
        for index in 0.. {
            let _ = write!(self.path, "[{index}]");
            let item = Walk {
                path: &mut *self.path,
                found: &mut *self.found,
            };
            let more = items.next_element_seed(item)?;
            self.path.truncate(at);
            if more.is_none() {
                break;
            }
        }
        Ok(())
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut fields: A) -> Result<(), A::Error> {
        let at = self.path.len();
        while let Some(key) = fields.next_key::<String>()? {
            if at > 0 {
                self.path.push('.');
            }
            self.path.push_str(&key);
            fields.next_value_seed(Walk {
                path: &mut *self.path,
                found: &mut *self.found,
            })?;
            self.path.truncate(at);
        }
        Ok(())
    }

    fn visit_bool<E>(self, _: bool) -> Result<(), E> {
        Ok(())
    }

    fn visit_i64<E>(self, _: i64) -> Result<(), E> {
        Ok(())
    }

    fn visit_u64<E>(self, _: u64) -> Result<(), E> {
        Ok(())
    }

    fn visit_f64<E>(self, _: f64) -> Result<(), E> {
        Ok(())
    }

    fn visit_unit<E>(self) -> Result<(), E> {
        Ok(())
    }
}

/// An integer that looks like a Unix time (`format::unix_time_hint`'s
/// span), as a time in UTC.
pub fn unix_time(text: &str) -> Option<Zoned> {
    let (seconds, nanos, digits) = crate::format::unix_parts(text)?;
    let instant = DateTime::from_timestamp(seconds, nanos)?.naive_utc();
    Some(Zoned {
        date: Some(instant.date()),
        time: instant.time(),
        offset: FixedOffset::east_opt(0)?,
        digits,
    })
}

/// Europe/Paris' rules since 1996 (CET, CEST from the last Sunday of March
/// to the last Sunday of October, at 01:00 UTC): for tests that need a zone
/// with daylight saving whatever the computer's.
#[cfg(any(test, feature = "test-support"))]
pub fn europe_paris() -> Zone {
    Zone::new("Europe/Paris", |utc| {
        let year = utc.year();
        let last_sunday = |month: u32| {
            let last = NaiveDate::from_ymd_opt(year, month, 31).unwrap();
            let back = last.weekday().num_days_from_sunday();
            last - chrono::Days::new(u64::from(back))
        };
        let start = last_sunday(3).and_hms_opt(1, 0, 0).unwrap();
        let end = last_sunday(10).and_hms_opt(1, 0, 0).unwrap();
        let hours = if (start..end).contains(utc) { 2 } else { 1 };
        FixedOffset::east_opt(hours * 3600).unwrap()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 26).unwrap()
    }

    fn fixed(name: &str, seconds: i32) -> Zone {
        Zone::new(name, move |_| FixedOffset::east_opt(seconds).unwrap())
    }

    /// America/New_York's rules since 2007: EDT from the second Sunday of
    /// March at 07:00 UTC to the first Sunday of November at 06:00 UTC.
    fn new_york() -> Zone {
        Zone::new("America/New_York", |utc| {
            let year = utc.year();
            let nth_sunday = |month: u32, nth: u64| {
                let first = NaiveDate::from_ymd_opt(year, month, 1).unwrap();
                let ahead = (7 - first.weekday().num_days_from_sunday()) % 7;
                first + chrono::Days::new(u64::from(ahead) + 7 * (nth - 1))
            };
            let start = nth_sunday(3, 2).and_hms_opt(7, 0, 0).unwrap();
            let end = nth_sunday(11, 1).and_hms_opt(6, 0, 0).unwrap();
            let hours = if (start..end).contains(utc) { -4 } else { -5 };
            FixedOffset::east_opt(hours * 3600).unwrap()
        })
    }

    fn shown(zone: &Zone, text: &str) -> Option<String> {
        zone.show(&parse(text)?, today())
    }

    /// The spellings read, and those refused; nothing panics.
    #[test]
    fn times_with_a_zone_are_read_and_others_refused() {
        for accepted in [
            "2026-06-08 16:17:04.768895+00",
            "2026-06-08T16:17:04Z",
            "2026-06-08T16:17:04z",
            "2026-06-08 16:17:04+02",
            "2026-06-08 16:17:04+02:00",
            "2026-06-08 16:17:04+0200",
            "2026-06-08 16:17:04-05:30",
            "2026-06-08T16:17Z",
            "1850-01-01 00:00:00+00:09:21",
            "2026-06-08 16:17:04.123456789123+00",
            "0000-01-01 00:00:00+00",
            "9999-12-31 23:59:59-12",
            "2016-12-31 23:59:60Z",
            "16:17:04+02",
            "16:17:04.5-03:30",
        ] {
            assert!(parse(accepted).is_some(), "{accepted:?} refused");
        }
        for refused in [
            "2026-06-08 16:17:04",
            "2026-06-08T16:17:04",
            "2026-06-08",
            "16:17:04",
            "12:30+01",
            "12:30-14:00",
            "16:17+02:00",
            "2026-06-08 16:17:04+00 BC",
            "infinity",
            "-infinity",
            "2026-06-08 24:00:00+00",
            "2026-06-08 16:60:00+00",
            "2026-06-08 16:17:61+00",
            "2026-02-30 16:17:04+00",
            "2026-06-08 16:17:04+24",
            "2026-06-08 16:17:04+02:60",
            "2026-06-08 16:17:04.+00",
            "2026-06-08 16:17:04 +00",
            "2026-06-08 16:17:04+2",
            "2026-06-08  16:17:04+00",
            "26-06-08 16:17:04+00",
            "2026-06-08 16:17:04+00 ",
            " 2026-06-08 16:17:04+00",
            "2026-06-08 16:17:04 UTC",
            "1794619425",
            "",
            "é",
            "2026-06-08 16:17:04+00:00:00:00",
        ] {
            assert_eq!(parse(refused), None, "{refused:?} read");
        }
    }

    /// The offset is the zone's at that instant: Paris in June and in
    /// December, half an hour, west of UTC, across midnight, the fraction as
    /// written.
    #[test]
    fn a_time_is_shown_with_the_offset_of_its_own_date() {
        let paris = europe_paris();
        assert_eq!(
            shown(&paris, "2026-06-08 16:17:04.768895+00").as_deref(),
            Some("2026-06-08 18:17:04.768895 +02:00")
        );
        assert_eq!(
            shown(&paris, "2026-12-08T09:30:00Z").as_deref(),
            Some("2026-12-08 10:30:00 +01:00")
        );
        // Either side of the change of 2026-03-29 at 01:00 UTC.
        assert_eq!(
            shown(&paris, "2026-03-29 00:59:59Z").as_deref(),
            Some("2026-03-29 01:59:59 +01:00")
        );
        assert_eq!(
            shown(&paris, "2026-03-29 01:00:00Z").as_deref(),
            Some("2026-03-29 03:00:00 +02:00")
        );
        let kolkata = fixed("Asia/Kolkata", 5 * 3600 + 1800);
        assert_eq!(
            shown(&kolkata, "2026-06-08 16:17:04+00").as_deref(),
            Some("2026-06-08 21:47:04 +05:30")
        );
        let york = new_york();
        assert_eq!(
            shown(&york, "2026-06-08 16:17:04+02:00").as_deref(),
            Some("2026-06-08 10:17:04 -04:00")
        );
        assert_eq!(
            shown(&york, "2026-01-02 03:00:00Z").as_deref(),
            Some("2026-01-01 22:00:00 -05:00"),
            "across midnight, back a day"
        );
        assert_eq!(
            shown(&paris, "2026-12-31 23:30:00Z").as_deref(),
            Some("2027-01-01 00:30:00 +01:00"),
            "across midnight and the year"
        );
        // From another offset than UTC, and PostgreSQL's historical one.
        assert_eq!(
            shown(&paris, "2026-06-08 12:00:00-05:30").as_deref(),
            Some("2026-06-08 19:30:00 +02:00")
        );
        assert_eq!(
            shown(&fixed("UTC", 0), "1850-01-01 00:09:21+00:09:21").as_deref(),
            Some("1850-01-01 00:00:00 +00:00")
        );
        assert_eq!(
            shown(&fixed("LMT", 561), "1850-01-01 00:00:00Z").as_deref(),
            Some("1850-01-01 00:09:21 +00:09:21")
        );
        // Nine digits at most, kept as written.
        assert_eq!(
            shown(&paris, "2026-06-08 16:17:04.123456789123+00").as_deref(),
            Some("2026-06-08 18:17:04.123456789 +02:00")
        );
        assert_eq!(
            shown(&paris, "2026-06-08 16:17:04.50+00").as_deref(),
            Some("2026-06-08 18:17:04.50 +02:00")
        );
        // A time of day alone: today's offset.
        assert_eq!(
            shown(&paris, "16:17:04+00").as_deref(),
            Some("17:17:04 +01:00")
        );
        assert!(!parse("16:17:04+00").unwrap().has_date());
        // The edges of the calendar and a leap second do not panic.
        assert_eq!(
            shown(&york, "0000-01-01 00:00:00+00").as_deref(),
            Some("-0001-12-31 19:00:00 -05:00")
        );
        assert_eq!(
            shown(&paris, "9999-12-31 23:59:59-12").as_deref(),
            Some("+10000-01-01 12:59:59 +01:00")
        );
        assert_eq!(
            shown(&fixed("UTC", 0), "2016-12-31 23:59:60.5Z").as_deref(),
            Some("2016-12-31 23:59:60.5 +00:00")
        );
    }

    /// Every string of a document that is a time with a zone, with its path,
    /// nested ones included; the count goes on past the limit.
    #[test]
    fn times_are_found_in_a_json_document() {
        let doc = r#"{"price": 600, "ordered_at": "2026-06-08 16:17:04.768895+00", "customer": "Ada",
            "items": [{"sku": "A-1", "shipped_at": "2026-12-08T09:30:00Z"}, {"due": "2026-12-09"}],
            "log": [["2026-01-01T00:00:00+01:00"]]}"#;
        let (found, count) = times_in_json(doc, 10);
        let paths: Vec<&str> = found.iter().map(|found| found.path.as_str()).collect();
        assert_eq!(paths, ["ordered_at", "items[0].shipped_at", "log[0][0]"]);
        assert_eq!(count, 3);
        let paris = europe_paris();
        assert_eq!(
            paris.show(&found[1].zoned, today()).as_deref(),
            Some("2026-12-08 10:30:00 +01:00")
        );
        assert_eq!(times_in_json(doc, 1).0.len(), 1);
        assert_eq!(times_in_json(doc, 1).1, 3);
        // The document itself a string, a document without times, not JSON.
        let (root, _) = times_in_json(r#""2026-06-08T16:17:04Z""#, 10);
        assert_eq!(root[0].path, "");
        assert_eq!(
            times_in_json(r#"{"a": "2026-06-08 16:17:04"}"#, 10),
            (vec![], 0)
        );
        assert_eq!(times_in_json("{not json", 10), (vec![], 0));
    }

    /// A Unix time is a UTC time; shown here as any other.
    #[test]
    fn a_unix_time_is_shown_in_the_zone() {
        let paris = europe_paris();
        let seconds = unix_time("1794619425").unwrap();
        assert_eq!(
            paris.show(&seconds, today()).as_deref(),
            Some("2026-11-14 02:23:45 +01:00")
        );
        let millis = unix_time("1780935424768").unwrap();
        assert_eq!(
            paris.show(&millis, today()).as_deref(),
            Some("2026-06-08 18:17:04.768 +02:00")
        );
        assert_eq!(unix_time("946684799"), None);
    }
}
