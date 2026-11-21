//! K32: the PDF read back without a PDF reader: its objects are split, the
//! ToUnicode maps of its fonts parsed, and every string its pages show
//! decoded back to text, so the tests see what a reader would extract.

use std::collections::HashMap;

use super::super::tests::{fonts, hostile, shop, table as relation};
use super::*;
use crate::compare::Side;
use crate::schema::{ColumnInfo, TableStructure};

/// The objects of an uncompressed PDF: number → body.
fn objects(pdf: &[u8]) -> HashMap<u32, Vec<u8>> {
    let mut found = HashMap::new();
    let text = pdf;
    let mut at = 0;
    while let Some(start) = find(text, b" 0 obj", at) {
        // The object number is the digits before.
        let mut begin = start;
        while begin > 0 && text[begin - 1].is_ascii_digit() {
            begin -= 1;
        }
        let number: u32 = std::str::from_utf8(&text[begin..start])
            .unwrap()
            .parse()
            .unwrap();
        let body_start = start + 6;
        let end = find(text, b"endobj", body_start).expect("endobj");
        found.insert(number, text[body_start..end].to_vec());
        at = end;
    }
    found
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    haystack[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|at| at + from)
}

/// `/Key N 0 R` in a body.
fn reference(body: &[u8], key: &[u8]) -> Option<u32> {
    let at = find(body, key, 0)? + key.len();
    let rest = std::str::from_utf8(&body[at..]).ok()?;
    rest.split_whitespace().next()?.parse().ok()
}

/// The data of a stream object.
fn stream_data(body: &[u8]) -> &[u8] {
    let start = find(body, b"stream\n", 0).expect("a stream") + 7;
    let end = find(body, b"\nendstream", start).expect("endstream");
    &body[start..end]
}

/// CID → text, from a ToUnicode map.
fn to_unicode(data: &[u8]) -> HashMap<u16, String> {
    let text = String::from_utf8_lossy(data);
    let mut map = HashMap::new();
    for line in text.lines() {
        let parts: Vec<&str> = line
            .split(['<', '>'])
            .filter(|part| !part.trim().is_empty())
            .collect();
        if parts.len() == 2 && parts[0].len() == 4 {
            let cid = u16::from_str_radix(parts[0], 16).unwrap();
            let units: Vec<u16> = (0..parts[1].len() / 4)
                .map(|i| u16::from_str_radix(&parts[1][i * 4..i * 4 + 4], 16).unwrap())
                .collect();
            map.insert(cid, String::from_utf16(&units).unwrap());
        }
    }
    map
}

/// A literal string's bytes, from just after its `(`; and where it ends.
fn literal(data: &[u8], mut at: usize) -> (Vec<u8>, usize) {
    let mut out = Vec::new();
    let mut depth = 0;
    while at < data.len() {
        let c = data[at];
        at += 1;
        match c {
            b'\\' => {
                let next = data[at];
                at += 1;
                match next {
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'b' => out.push(8),
                    b'f' => out.push(12),
                    b'0'..=b'7' => {
                        let mut value = u32::from(next - b'0');
                        for _ in 0..2 {
                            if at < data.len() && (b'0'..=b'7').contains(&data[at]) {
                                value = value * 8 + u32::from(data[at] - b'0');
                                at += 1;
                            }
                        }
                        out.push(value as u8);
                    }
                    other => out.push(other),
                }
            }
            b'(' => {
                depth += 1;
                out.push(c);
            }
            b')' if depth == 0 => return (out, at),
            b')' => {
                depth -= 1;
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    (out, at)
}

/// The text each page shows, decoded through the fonts' ToUnicode maps.
fn page_texts(pdf: &[u8]) -> Vec<String> {
    let objects = objects(pdf);
    let mut pages: Vec<(u32, &Vec<u8>)> = objects
        .iter()
        .filter(|(_, body)| {
            find(body, b"/Type/Page/", 0).is_some() || body.ends_with(b"/Type/Page")
        })
        .filter(|(_, body)| find(body, b"/Contents", 0).is_some())
        .map(|(number, body)| (*number, body))
        .collect();
    pages.sort_by_key(|(number, _)| *number);
    pages
        .iter()
        .map(|(_, page)| {
            let mut maps: HashMap<Vec<u8>, HashMap<u16, String>> = HashMap::new();
            for name in [b"/F0".as_slice(), b"/F1", b"/F2"] {
                if let Some(font) = reference(page, name) {
                    let type0 = &objects[&font];
                    let cmap = reference(type0, b"/ToUnicode").expect("a ToUnicode map");
                    maps.insert(name[1..].to_vec(), to_unicode(stream_data(&objects[&cmap])));
                }
            }
            let content = reference(page, b"/Contents").unwrap();
            let data = stream_data(&objects[&content]);
            let mut text = String::new();
            let mut font: Vec<u8> = Vec::new();
            let mut at = 0;
            while at < data.len() {
                if data[at] == b'/' {
                    let end = data[at + 1..]
                        .iter()
                        .position(|c| !c.is_ascii_alphanumeric())
                        .map_or(data.len(), |p| p + at + 1);
                    font = data[at + 1..end].to_vec();
                    at = end;
                } else if data[at] == b'(' {
                    let (bytes, end) = literal(data, at + 1);
                    let map = &maps[&font];
                    for pair in bytes.chunks(2) {
                        let cid = u16::from_be_bytes([pair[0], pair[1]]);
                        text.push_str(map.get(&cid).map_or("\u{fffd}", String::as_str));
                    }
                    text.push('\n');
                    at = end;
                } else {
                    at += 1;
                }
            }
            text
        })
        .collect()
}

fn pdf(doc: &Document, options: &Options) -> Vec<u8> {
    to_pdf_with(doc, options, &fonts(), false).expect("written")
}

#[test]
fn a_pdf_is_a_pdf_with_its_pages_outline_and_links() {
    let doc = shop();
    let bytes = pdf(&doc, &Options::default());
    assert!(bytes.starts_with(b"%PDF-"));
    assert!(bytes.trim_ascii_end().ends_with(b"%%EOF"));
    // Cover and contents, the diagram, a page per table, the views, the
    // function.
    let writer = laid_out(&doc, &Options::default(), &fonts());
    assert_eq!(writer.page_count(), 6);
    assert!(find(&bytes, b"/Count 6", 0).is_some());
    assert_eq!(page_texts(&bytes).len(), 6);
    // The outline: the groups and one entry per table, view and function.
    for title in [
        "(Diagram)",
        "(Tables)",
        "(customers)",
        "(orders)",
        "(Views)",
        "(big_orders)",
        "(order_count(bigint))",
    ] {
        assert!(
            find(&bytes, format!("/Title{title}").as_bytes(), 0).is_some(),
            "{title}"
        );
    }
    // A link per entry of the contents: four groups, two tables, a view, a function.
    let links = bytes
        .windows(b"/Subtype/Link".len())
        .filter(|w| *w == b"/Subtype/Link")
        .count();
    assert_eq!(links, 8);
    assert!(find(&bytes, b"/Title(kerno.shop)", 0).is_some());
    assert!(find(&bytes, b"/Creator(Kerno)", 0).is_some());
    // Compressed, it is smaller and still a PDF.
    let packed = to_pdf(&doc, &Options::default(), &fonts()).unwrap();
    assert!(
        packed.len() < bytes.len() / 2,
        "{} {}",
        packed.len(),
        bytes.len()
    );
    assert!(find(&packed, b"/Filter/FlateDecode", 0).is_some());
}

#[test]
fn the_text_maps_back_to_its_characters() {
    let doc = shop();
    let pages = page_texts(&pdf(&doc, &Options::default()));
    let all = pages.join("\n");
    for expected in [
        "kerno.shop",
        "customers",
        "As printed on invoices",
        "Who buys",
        "orders_customer_fk",
        "timestamp with time",
        "CHECK ((total >= (0)::numeric))",
        "CREATE OR REPLACE VIEW big_orders AS",
        "page 1 of 6",
    ] {
        assert!(all.contains(expected), "{expected}\n{all}");
    }
    assert!(!all.contains('\u{fffd}'), "an unmapped glyph:\n{all}");
    // The contents name each table with its page.
    assert!(pages[0].contains("customers\n3"), "{}", pages[0]);
    // A character the fonts lack is drawn as the missing glyph and still
    // maps back to itself.
    let mut odd = shop();
    odd.title = "Schéma 数据".into();
    let text = page_texts(&pdf(&odd, &Options::default())).join("\n");
    assert!(text.contains("Schéma 数据"), "{text}");
}

#[test]
fn plain_names_no_kerno() {
    let options = Options {
        style: Style::Plain,
        ..Options::default()
    };
    let bytes = pdf(&shop(), &options);
    assert!(find(&bytes, b"Kerno", 0).is_none());
    assert!(!page_texts(&bytes).join("\n").contains("Kerno"));
    let compressed = to_pdf(&shop(), &options, &fonts()).unwrap();
    assert!(find(&compressed, b"Kerno", 0).is_none());
    // The Kerno styles say who made it, and draw the lockup's shapes.
    let light = pdf(&shop(), &Options::default());
    assert!(page_texts(&light)[5].contains("Generated by Kerno"));
    assert_eq!(brand_shapes(LOCKUP).len(), 2);
    assert_eq!(brand_shapes(LOCKUP_REVERSED).len(), 3);
}

#[test]
fn dark_pages_are_painted_and_light_ones_are_not() {
    let dark = Options {
        style: Style::KernoDark,
        ..Options::default()
    };
    let paint = |options: &Options| {
        let writer = laid_out(&shop(), options, &fonts());
        writer.palette.fill_page
    };
    assert!(paint(&dark));
    assert!(!paint(&Options::default()));
    // The page's background is the dark theme's.
    let bytes = pdf(&shop(), &dark);
    let [r, g, b] = rgb(&super::super::colours(Style::KernoDark).background);
    let painted = format!("{r} {g} {b} rg\n0 0 595.276 841.89 re\nf");
    assert!(find(&bytes, painted.as_bytes(), 0).is_some(), "{painted}");
}

#[test]
fn hostile_names_stay_strings() {
    let mut doc = hostile();
    doc.side.objects.push(relation(
        "a)b\\c(d endstream endobj %x\nq",
        TableStructure {
            columns: vec![ColumnInfo {
                comment: Some(") Tj ET 0 0 1000 1000 re f (".into()),
                ..super::super::tests::column("x", "int", true)
            }],
            ..Default::default()
        },
    ));
    let bytes = pdf(&doc, &Options::default());
    // Every object closes, and the page texts decode: nothing broke out.
    let objects = objects(&bytes);
    assert!(objects.len() > 20);
    let text = page_texts(&bytes).join("\n");
    assert!(text.contains("a)b\\c(d endstream endobj %x q"), "{text}");
    assert!(text.contains(") Tj ET 0 0 1000 1000 re f ("), "{text}");
    assert!(text.contains("x\"><script>alert(1)</script>"));
    // In the outline the name is a string, its parentheses and backslash
    // escaped, the line break a space.
    assert!(find(&bytes, b"/Title(a\\)b\\\\c\\(d endstream endobj %x q)", 0).is_some());
    // Compressed, the same.
    assert!(to_pdf(&doc, &Options::default(), &fonts()).is_ok());
}

/// Sixty columns of 300-character types and defaults, a 300-character
/// table name and comment.
fn wide() -> Document {
    let long = "x".repeat(300);
    let structure = TableStructure {
        columns: (0..60)
            .map(|index| ColumnInfo {
                default: Some(long.clone()),
                comment: Some(format!("{index} {long}")),
                ..super::super::tests::column(&format!("c{index}"), &long, true)
            })
            .collect(),
        comment: Some(long.clone()),
        ..Default::default()
    };
    Document {
        title: "y".repeat(300),
        side: Side {
            schema: "shop".into(),
            objects: vec![relation(&"n".repeat(300), structure)],
        },
        ..shop()
    }
}

#[test]
fn nothing_runs_off_the_page() {
    for page in PageSize::ALL {
        let options = Options {
            page,
            ..Options::default()
        };
        let writer = laid_out(&wide(), &options, &fonts());
        let (width, height) = writer.page_box();
        assert!(writer.page_count() > 10, "{}", writer.page_count());
        for drawn in &writer.drawn {
            assert!(drawn.x0 >= SIDE - 0.01, "{drawn:?}");
            assert!(drawn.x1 <= width - SIDE + 0.01, "{page:?}: {drawn:?}");
            assert!(
                drawn.y >= FOOTER - 0.01 && drawn.y <= height - TOP,
                "{drawn:?}"
            );
            // Only the footer goes into the bottom margin.
            let footer = drawn.y == FOOTER;
            assert!(footer || drawn.y >= BOTTOM - 0.01, "{drawn:?}");
        }
        // The header row comes again on every page the table continues on.
        let headers: Vec<usize> = writer
            .drawn
            .iter()
            .filter(|drawn| drawn.text == "Nullable")
            .map(|drawn| drawn.page)
            .collect();
        // The table's rows (the diagram's page names the columns too).
        let diagram = writer.anchors[&Anchor::Diagram].0;
        let rows: Vec<usize> = writer
            .drawn
            .iter()
            .filter(|drawn| drawn.page != diagram)
            .filter(|drawn| drawn.text.starts_with('c') && drawn.text[1..].parse::<u32>().is_ok())
            .map(|drawn| drawn.page)
            .collect();
        for page in &rows {
            assert!(headers.contains(page), "page {page} has rows and no header");
        }
        // Every column's row is there.
        assert_eq!(rows.len(), 60);
    }
}

#[test]
fn lines_wrap_between_words_keep_their_indentation_and_break_long_words() {
    let writer = laid_out(&shop(), &Options::default(), &fonts());
    let width = writer.measure(F::Mono, 7., "x") * 20.;
    let lines = writer.wrap(
        F::Mono,
        7.,
        "BEGIN\n    RETURN  NEW;\nalpha beta gamma delta epsilon\n"
            .to_string()
            .as_str(),
        width,
    );
    assert_eq!(lines[0], "BEGIN");
    assert_eq!(lines[1], "    RETURN  NEW;");
    assert_eq!(lines[2], "alpha beta gamma");
    assert_eq!(lines[3], "delta epsilon");
    let long = writer.wrap(F::Mono, 7., &"y".repeat(50), width);
    assert_eq!(long.len(), 3);
    for line in long.iter().chain(&lines) {
        assert!(writer.measure(F::Mono, 7., line) <= width + 0.01, "{line}");
    }
}

/// The font reader gives the glyphs, advances and metrics `ttf-parser`
/// 0.25.1 gave for the three fonts (taken from it before it was replaced).
#[test]
fn the_font_reader_reads_the_glyphs_advances_and_metrics() {
    let f = fonts();
    let faces = [
        (
            f.regular,
            2048,
            1984,
            -494,
            Some(1490),
            false,
            "Inter-Regular",
            [-1513, -660, 5290, 2269],
            vec![
                ('A', 2, 1413),
                ('M', 292, 1850),
                ('Z', 496, 1288),
                ('a', 507, 1150),
                ('g', 658, 1256),
                ('z', 1022, 1131),
                ('0', 1341, 1292),
                (' ', 1777, 576),
                ('_', 1543, 934),
                ('(', 1437, 747),
                ('|', 1456, 681),
                ('é', 618, 1194),
                ('…', 1503, 1770),
                ('—', 1462, 2048),
                ('→', 1805, 1954),
                ('€', 1316, 1365),
                ('ß', 886, 1262),
                ('数', 0, 1344),
                ('\u{1F600}', 0, 1344),
            ],
        ),
        (
            f.semibold,
            2048,
            1984,
            -494,
            Some(1490),
            false,
            "Inter-SemiBold",
            [-1580, -677, 5290, 2275],
            vec![
                ('A', 2, 1490),
                ('M', 292, 1889),
                ('z', 1022, 1159),
                (' ', 1777, 516),
                ('é', 618, 1211),
                ('…', 1503, 1958),
                ('€', 1316, 1390),
                ('数', 0, 1344),
            ],
        ),
        (
            f.mono,
            1000,
            1020,
            -300,
            Some(730),
            true,
            "JetBrainsMono-Regular",
            [-1735, -400, 765, 1120],
            vec![
                ('A', 1, 600),
                ('M', 89, 600),
                ('Z', 177, 600),
                ('g', 245, 600),
                ('9', 733, 600),
                (' ', 958, 600),
                ('|', 1470, 600),
                ('é', 226, 600),
                ('—', 858, 600),
                ('→', 1139, 600),
                ('ß', 330, 600),
                ('数', 0, 600),
                ('\u{1F600}', 0, 600),
            ],
        ),
    ];
    for (data, upem, ascender, descender, cap, mono, name, bbox, glyphs) in faces {
        let face = sfnt::Sfnt::parse(data).unwrap();
        assert_eq!(face.units_per_em, upem, "{name}");
        assert_eq!(
            (face.ascender, face.descender),
            (ascender, descender),
            "{name}"
        );
        assert_eq!(face.cap_height, cap, "{name}");
        assert_eq!(face.monospaced, mono, "{name}");
        assert_eq!(face.italic_angle, 0., "{name}");
        assert_eq!(face.postscript_name.as_deref(), Some(name));
        assert_eq!(face.bbox, bbox, "{name}");
        for (c, glyph, advance) in glyphs {
            assert_eq!(face.glyph(c), glyph, "{name} {c:?}");
            assert_eq!(face.advance(face.glyph(c)), advance, "{name} {c:?}");
        }
    }
    // A monospace font: every printable ASCII character has one advance.
    let mono = sfnt::Sfnt::parse(f.mono).unwrap();
    for c in ' '..='~' {
        assert_ne!(mono.glyph(c), 0, "{c:?}");
        assert_eq!(mono.advance(mono.glyph(c)), 600, "{c:?}");
    }
    // Not a font: an error, not a panic.
    assert!(sfnt::Sfnt::parse(b"not a font at all").is_err());
    assert!(sfnt::Sfnt::parse(&f.regular[..200]).is_err());
}

#[test]
fn a4_and_letter_media_boxes() {
    let a4 = pdf(&shop(), &Options::default());
    assert!(find(&a4, b"/MediaBox[0 0 595.276 841.89]", 0).is_some());
    let letter = pdf(
        &shop(),
        &Options {
            page: PageSize::Letter,
            ..Options::default()
        },
    );
    assert!(find(&letter, b"/MediaBox[0 0 612 792]", 0).is_some());
    assert!(find(&letter, b"/MediaBox[0 0 595.276", 0).is_none());
}

#[test]
fn an_empty_pick_and_no_diagram() {
    let empty = Document {
        side: Side {
            schema: "shop".into(),
            objects: Vec::new(),
        },
        ..shop()
    };
    let bytes = pdf(&empty, &Options::default());
    let pages = page_texts(&bytes);
    assert_eq!(pages.len(), 1);
    assert!(pages[0].contains("Nothing was picked"));
    assert!(find(&bytes, b"/Outlines", 0).is_none());
    let without = laid_out(
        &shop(),
        &Options {
            diagram: false,
            ..Options::default()
        },
        &fonts(),
    );
    assert_eq!(without.page_count(), 5);
    assert!(!without.anchors.contains_key(&Anchor::Diagram));
}
