//! The few tables of an OpenType (TrueType) font the PDF writer reads: the
//! character map, the advances and the metrics of the font descriptor.
//! Written here rather than taken from `ttf-parser` or `rustybuzz`, both
//! marked unmaintained by RustSec (D-105). It reads the fonts Kerno embeds;
//! a table that is missing or too short is an error, never a panic.

/// Big-endian reads that fail instead of panicking.
fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

fn i16_at(data: &[u8], at: usize) -> Option<i16> {
    u16_at(data, at).map(|value| value as i16)
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

/// Where characters map to glyphs.
enum Cmap<'a> {
    /// Segments of the Basic Multilingual Plane.
    Format4(&'a [u8]),
    /// Groups of any plane.
    Format12(&'a [u8]),
}

/// A parsed font.
pub(super) struct Sfnt<'a> {
    pub units_per_em: u16,
    /// xMin, yMin, xMax, yMax.
    pub bbox: [i16; 4],
    pub ascender: i16,
    pub descender: i16,
    pub cap_height: Option<i16>,
    pub italic_angle: f32,
    pub monospaced: bool,
    pub postscript_name: Option<String>,
    hmtx: &'a [u8],
    metrics: u16,
    cmap: Cmap<'a>,
}

/// The table `tag` of the font.
fn table<'a>(data: &'a [u8], tag: &[u8; 4]) -> Option<&'a [u8]> {
    let count = usize::from(u16_at(data, 4)?);
    (0..count).find_map(|index| {
        let record = 12 + index * 16;
        if data.get(record..record + 4)? != tag {
            return None;
        }
        let offset = u32_at(data, record + 8)? as usize;
        let length = u32_at(data, record + 12)? as usize;
        data.get(offset..offset.checked_add(length)?)
    })
}

/// The character map to read: format 12 first (every plane), else format 4.
fn cmap(table: &[u8]) -> Option<Cmap<'_>> {
    let count = usize::from(u16_at(table, 2)?);
    let subtables: Vec<(u16, u16, &[u8])> = (0..count)
        .filter_map(|index| {
            let record = 4 + index * 8;
            let platform = u16_at(table, record)?;
            let encoding = u16_at(table, record + 2)?;
            let offset = u32_at(table, record + 4)? as usize;
            Some((platform, encoding, table.get(offset..)?))
        })
        .collect();
    let with_format = |format: u16, wanted: &[(u16, u16)]| {
        subtables
            .iter()
            .find(|(platform, encoding, data)| {
                wanted.contains(&(*platform, *encoding)) && u16_at(data, 0) == Some(format)
            })
            .map(|(_, _, data)| *data)
    };
    if let Some(data) = with_format(12, &[(3, 10), (0, 4), (0, 6)]) {
        return Some(Cmap::Format12(data));
    }
    with_format(4, &[(3, 1), (0, 3), (0, 4)]).map(Cmap::Format4)
}

/// The PostScript name (name 6): UTF-16BE on Windows, single bytes on Mac.
fn postscript_name(table: &[u8]) -> Option<String> {
    let count = usize::from(u16_at(table, 2)?);
    let strings = usize::from(u16_at(table, 4)?);
    (0..count).find_map(|index| {
        let record = 6 + index * 12;
        let platform = u16_at(table, record)?;
        if u16_at(table, record + 6)? != 6 {
            return None;
        }
        let length = usize::from(u16_at(table, record + 8)?);
        let offset = strings + usize::from(u16_at(table, record + 10)?);
        let bytes = table.get(offset..offset + length)?;
        match platform {
            0 | 3 => {
                let units: Vec<u16> = bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| u16::from_be_bytes(*pair))
                    .collect();
                String::from_utf16(&units).ok()
            }
            1 => Some(bytes.iter().map(|byte| char::from(*byte)).collect()),
            _ => None,
        }
    })
}

impl<'a> Sfnt<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self, String> {
        let missing = |name: &str| format!("the font has no readable `{name}` table");
        let head = table(data, b"head").ok_or_else(|| missing("head"))?;
        let hhea = table(data, b"hhea").ok_or_else(|| missing("hhea"))?;
        let hmtx = table(data, b"hmtx").ok_or_else(|| missing("hmtx"))?;
        let cmap = table(data, b"cmap")
            .and_then(cmap)
            .ok_or_else(|| missing("cmap"))?;
        let post = table(data, b"post");
        let os2 = table(data, b"OS/2");
        let field =
            |table: &[u8], at: usize, name: &str| i16_at(table, at).ok_or_else(|| missing(name));
        let units_per_em = u16_at(head, 18).ok_or_else(|| missing("head"))?;
        if units_per_em == 0 {
            return Err(missing("head"));
        }
        let metrics = u16_at(hhea, 34).ok_or_else(|| missing("hhea"))?;
        if metrics == 0 || hmtx.len() < usize::from(metrics) * 4 {
            return Err(missing("hmtx"));
        }
        Ok(Self {
            units_per_em,
            bbox: [
                field(head, 36, "head")?,
                field(head, 38, "head")?,
                field(head, 40, "head")?,
                field(head, 42, "head")?,
            ],
            ascender: field(hhea, 4, "hhea")?,
            descender: field(hhea, 6, "hhea")?,
            cap_height: os2
                .filter(|os2| u16_at(os2, 0).is_some_and(|version| version >= 2))
                .and_then(|os2| i16_at(os2, 88)),
            italic_angle: post
                .and_then(|post| u32_at(post, 4))
                .map_or(0., |fixed| fixed as i32 as f32 / 65536.),
            monospaced: post
                .and_then(|post| u32_at(post, 12))
                .is_some_and(|fixed| fixed != 0),
            postscript_name: table(data, b"name").and_then(postscript_name),
            hmtx,
            metrics,
            cmap,
        })
    }

    /// The glyph of `c`, 0 (`.notdef`) when the font has none.
    pub fn glyph(&self, c: char) -> u16 {
        let code = c as u32;
        match self.cmap {
            Cmap::Format12(data) => format12(data, code).unwrap_or(0),
            Cmap::Format4(data) => format4(data, code).unwrap_or(0),
        }
    }

    /// The advance of glyph `glyph`, in font units: past the last long
    /// metric, the last advance.
    pub fn advance(&self, glyph: u16) -> u16 {
        let index = usize::from(glyph.min(self.metrics - 1));
        u16_at(self.hmtx, index * 4).unwrap_or(0)
    }
}

/// The first of `count` sorted entries for which `past` holds (a binary
/// search: the groups and segments of a character map are sorted).
fn first(count: usize, past: impl Fn(usize) -> bool) -> usize {
    let (mut low, mut high) = (0, count);
    while low < high {
        let middle = (low + high) / 2;
        if past(middle) {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    low
}

/// A format 12 lookup: the group whose end is at or past `code`.
fn format12(data: &[u8], code: u32) -> Option<u16> {
    let groups = u32_at(data, 12)? as usize;
    let end = |index: usize| u32_at(data, 16 + index * 12 + 4);
    let group = first(groups, |index| end(index).is_none_or(|end| end >= code));
    let at = 16 + group * 12;
    let start = u32_at(data, at)?;
    if group >= groups || start > code {
        return None;
    }
    u16::try_from(u32_at(data, at + 8)? + (code - start)).ok()
}

/// A format 4 lookup: the segment whose end is at or past `code`, then the
/// glyph by delta or through the glyph id array.
fn format4(data: &[u8], code: u32) -> Option<u16> {
    let code = u16::try_from(code).ok()?;
    let segments = usize::from(u16_at(data, 6)? / 2);
    let ends = 14;
    let starts = ends + segments * 2 + 2;
    let deltas = starts + segments * 2;
    let offsets = deltas + segments * 2;
    let segment = first(segments, |index| {
        u16_at(data, ends + index * 2).is_none_or(|end| end >= code)
    });
    if segment >= segments {
        return None;
    }
    let start = u16_at(data, starts + segment * 2)?;
    if start > code {
        return None;
    }
    let delta = u16_at(data, deltas + segment * 2)?;
    let range = usize::from(u16_at(data, offsets + segment * 2)?);
    if range == 0 {
        return Some(code.wrapping_add(delta));
    }
    let at = offsets + segment * 2 + range + usize::from(code - start) * 2;
    let glyph = u16_at(data, at)?;
    (glyph != 0).then(|| glyph.wrapping_add(delta))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(words: &[u16]) -> Vec<u8> {
        words.iter().flat_map(|word| word.to_be_bytes()).collect()
    }

    /// A format 4 map: `A`–`C` by delta (to 10–12), `a` through the glyph id
    /// array (to 20, with the segment's delta), `b` mapped to nothing.
    #[test]
    fn format4_by_delta_and_through_the_glyph_array() {
        let delta = 10u16.wrapping_sub(0x41);
        let data = bytes(&[
            4, 48, 0, 6, 4, 1, 2, // header, segCountX2 = 6
            0x43, 0x62, 0xFFFF, // ends
            0,      // reserved
            0x41, 0x61, 0xFFFF, // starts
            delta, 5, 1, // deltas
            0, 4, 0, // range offsets: the second points at the glyph array
            15, 0, // glyph id array: a → 15 + 5, b → none
        ]);
        let glyph = |c: char| format4(&data, c as u32);
        assert_eq!(glyph('A'), Some(10));
        assert_eq!(glyph('C'), Some(12));
        assert_eq!(glyph('D'), None);
        assert_eq!(glyph('a'), Some(20));
        assert_eq!(glyph('b'), None);
        assert_eq!(glyph('\u{1F600}'), None);
    }

    #[test]
    fn format12_by_groups() {
        let mut data = bytes(&[12, 0, 0, 0, 0, 0]);
        data.extend_from_slice(&2u32.to_be_bytes());
        for (start, end, glyph) in [(0x41u32, 0x5A, 3u32), (0x1F600, 0x1F601, 50)] {
            for value in [start, end, glyph] {
                data.extend_from_slice(&value.to_be_bytes());
            }
        }
        assert_eq!(format12(&data, 'A' as u32), Some(3));
        assert_eq!(format12(&data, 'Z' as u32), Some(28));
        assert_eq!(format12(&data, '[' as u32), None);
        assert_eq!(format12(&data, 0x1F601), Some(51));
        assert_eq!(format12(&data, 0x1F602), None);
    }
}
