//! The PDF document (K32): the HTML's content laid out on A4 or Letter
//! pages with `pdf-writer`, three embedded fonts subsetted with `subsetter`
//! and read by [`sfnt`]
//! (CID fonts, Identity-H, a ToUnicode map, so the text can be selected,
//! copied and searched), the diagram and the lockup as vectors, an outline,
//! links from the contents, a footer with the title and the page numbers.
//!
//! Text from the database reaches the file only as glyph ids in strings
//! written by `pdf-writer` (`Str`, which escapes them) and as text strings
//! of the outline (`TextStr`); nothing is pasted into a content stream or a
//! dictionary by hand. Every font name, resource name and colour is Kerno's.

use std::collections::HashMap;
use std::io::Write as _;

use kurbo::Shape as _;
use pdf_writer::types::{
    ActionType, AnnotationType, CidFontType, FontFlags, SystemInfo, UnicodeCmap,
};
use pdf_writer::{Content, Filter, Finish as _, Name, Pdf, Rect, Ref, Settings, Str, TextStr};
use subsetter::GlyphRemapper;

use super::{
    CODE_KINDS, Code, Document, Options, Style, Table, column_rows, constraint_label, kind_label,
    primary_key, referenced,
};
use crate::erd;
use crate::schema::ConstraintKind;
use crate::table_draft::present;

/// The bytes of the three fonts the PDF embeds: the app gives the ones it
/// already carries (Inter Regular and SemiBold, JetBrains Mono Regular).
#[derive(Debug, Clone, Copy)]
pub struct Fonts<'a> {
    pub regular: &'a [u8],
    pub semibold: &'a [u8],
    pub mono: &'a [u8],
}

/// The paper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PageSize {
    #[default]
    A4,
    Letter,
}

impl PageSize {
    pub const ALL: [PageSize; 2] = [PageSize::A4, PageSize::Letter];

    pub fn label(self) -> &'static str {
        match self {
            PageSize::A4 => "A4",
            PageSize::Letter => "US Letter",
        }
    }

    /// Width and height in points.
    pub fn points(self) -> (f32, f32) {
        match self {
            PageSize::A4 => (595.276, 841.89),
            PageSize::Letter => (612., 792.),
        }
    }
}

const LOCKUP: &str = include_str!("../../../../assets/brand/kerno-horizontal.svg");
const LOCKUP_REVERSED: &str =
    include_str!("../../../../assets/brand/kerno-horizontal-reversed.svg");

/// Margins: left and right, top, bottom (the footer sits in the bottom one).
const SIDE: f32 = 50.;
const TOP: f32 = 56.;
const BOTTOM: f32 = 62.;
const FOOTER: f32 = 30.;
/// Body text.
const BODY: f32 = 9.;
const SMALL: f32 = 7.5;
/// The lockup's height: 102 pt wide, above BRAND.md's 96 px minimum.
const LOCKUP_HEIGHT: f32 = 32.;
/// Cell padding in tables.
const PAD: f32 = 4.;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum F {
    Regular = 0,
    Semibold = 1,
    Mono = 2,
}

type Rgb = [f32; 3];

fn rgb(hex: &str) -> Rgb {
    let value = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or_default();
    [
        ((value >> 16) & 0xff) as f32 / 255.,
        ((value >> 8) & 0xff) as f32 / 255.,
        (value & 0xff) as f32 / 255.,
    ]
}

/// A font as the writer uses it: its glyphs and advances, and the
/// characters drawn with it. A CID is given to each character drawn (1, 2,
/// …; 0 is `.notdef`), mapped to its glyph by a CIDToGIDMap and back to the
/// character by the ToUnicode map.
struct Face<'f> {
    data: &'f [u8],
    face: sfnt::Sfnt<'f>,
    scale: f32,
    remapper: GlyphRemapper,
    cids: HashMap<char, u16>,
    /// Per CID (index): the character and its glyph in the original font.
    chars: Vec<(char, u16)>,
}

impl<'f> Face<'f> {
    fn new(data: &'f [u8]) -> Result<Self, String> {
        let face = sfnt::Sfnt::parse(data)?;
        Ok(Self {
            data,
            scale: 1. / f32::from(face.units_per_em),
            face,
            remapper: GlyphRemapper::new(),
            cids: HashMap::new(),
            chars: vec![('\u{fffd}', 0)],
        })
    }

    /// The glyph of `c` in the original font.
    // ponytail: no fallback font and no shaping (one glyph per character,
    // no kerning, no complex scripts): a character the font lacks is drawn
    // as its missing glyph and still maps back to itself in ToUnicode. To
    // do better: shape with a maintained shaper (harfrust, which RustSec
    // points to; rustybuzz is unmaintained) and add a fallback font.
    fn glyph(&self, c: char) -> u16 {
        self.face.glyph(c)
    }

    /// Advance of `c` at size 1.
    fn advance(&self, c: char) -> f32 {
        f32::from(self.face.advance(self.glyph(c))) * self.scale
    }

    fn width(&self, text: &str, size: f32) -> f32 {
        text.chars().map(|c| self.advance(c)).sum::<f32>() * size
    }

    /// The CIDs of `text`, as the big-endian bytes a string shows.
    fn encode(&mut self, text: &str) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(text.len() * 2);
        for c in text.chars() {
            let cid = match self.cids.get(&c) {
                Some(cid) => *cid,
                None => {
                    let glyph = self.glyph(c);
                    self.remapper.remap(glyph);
                    let cid = self.chars.len() as u16;
                    self.chars.push((c, glyph));
                    self.cids.insert(c, cid);
                    cid
                }
            };
            bytes.extend_from_slice(&cid.to_be_bytes());
        }
        bytes
    }
}

/// Where a link or a bookmark leads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Anchor {
    Diagram,
    Tables,
    Table(usize),
    Views,
    View(usize),
    Code,
    CodeItem(usize),
}

/// A place in the document: page index, and the top of the target in
/// points from the page's bottom.
type Place = (usize, f32);

/// A bookmark: its title and where it leads.
type Bookmark = (String, Anchor);

struct Page {
    content: Content,
    links: Vec<(Rect, Anchor)>,
}

/// A text drawn, kept for the tests: page, left, right, baseline.
#[cfg(test)]
#[derive(Debug, Clone)]
pub(super) struct Drawn {
    pub page: usize,
    pub x0: f32,
    pub x1: f32,
    pub y: f32,
    pub text: String,
}

/// Colours of a style, as fractions.
struct Palette {
    background: Rgb,
    surface: Rgb,
    border: Rgb,
    text: Rgb,
    muted: Rgb,
    accent: Rgb,
    header: Rgb,
    fill_page: bool,
}

impl Palette {
    fn of(style: Style) -> Self {
        let c = super::colours(style);
        Self {
            background: rgb(&c.background),
            surface: rgb(&c.surface),
            border: rgb(&c.border),
            text: rgb(&c.text),
            muted: rgb(&c.muted),
            accent: rgb(&c.accent),
            header: rgb(&c.header),
            // White is the paper's colour: only the dark style paints it.
            fill_page: c.background != "#ffffff",
        }
    }
}

/// Lays the document out, page by page.
pub(super) struct Writer<'f> {
    faces: [Face<'f>; 3],
    palette: Palette,
    style: Style,
    width: f32,
    height: f32,
    pages: Vec<Page>,
    /// The top of the next line, in points from the page's bottom.
    y: f32,
    anchors: HashMap<Anchor, Place>,
    /// Where each anchor was in the first pass: the contents' page numbers.
    known: Option<HashMap<Anchor, Place>>,
    #[cfg(test)]
    pub(super) drawn: Vec<Drawn>,
}

impl<'f> Writer<'f> {
    fn new(
        fonts: &Fonts<'f>,
        options: &Options,
        known: Option<HashMap<Anchor, Place>>,
    ) -> Result<Self, String> {
        let (width, height) = options.page.points();
        let mut writer = Self {
            faces: [
                Face::new(fonts.regular)?,
                Face::new(fonts.semibold)?,
                Face::new(fonts.mono)?,
            ],
            palette: Palette::of(options.style),
            style: options.style,
            width,
            height,
            pages: Vec::new(),
            y: 0.,
            anchors: HashMap::new(),
            known,
            #[cfg(test)]
            drawn: Vec::new(),
        };
        writer.new_page();
        Ok(writer)
    }

    fn left(&self) -> f32 {
        SIDE
    }

    fn right(&self) -> f32 {
        self.width - SIDE
    }

    fn measure(&self, font: F, size: f32, text: &str) -> f32 {
        self.faces[font as usize].width(text, size)
    }

    fn page(&mut self) -> &mut Page {
        self.pages.last_mut().expect("a page")
    }

    fn new_page(&mut self) {
        let mut content = Content::new();
        if self.palette.fill_page {
            let [r, g, b] = self.palette.background;
            content.set_fill_rgb(r, g, b);
            content.rect(0., 0., self.width, self.height);
            content.fill_nonzero();
        }
        self.pages.push(Page {
            content,
            links: Vec::new(),
        });
        self.y = self.height - TOP;
    }

    /// Starts a new page unless the current one has `height` left.
    fn ensure(&mut self, height: f32) {
        if self.y - height < BOTTOM {
            self.new_page();
        }
    }

    /// Starts a new page unless nothing is on this one yet.
    fn break_page(&mut self) {
        if self.y < self.height - TOP {
            self.new_page();
        }
    }

    fn mark(&mut self, anchor: Anchor) {
        let place = (self.pages.len() - 1, self.y);
        self.anchors.insert(anchor, place);
    }

    /// One line of text whose baseline is at `y`, on the current page.
    fn text(&mut self, font: F, size: f32, x: f32, y: f32, text: &str, colour: Rgb) {
        let page = self.pages.len() - 1;
        self.text_on(page, (font, size), (x, y), text, colour);
    }

    /// One line of text on page `page`.
    fn text_on(
        &mut self,
        page: usize,
        (font, size): (F, f32),
        (x, y): (f32, f32),
        text: &str,
        colour: Rgb,
    ) {
        if text.is_empty() {
            return;
        }
        let bytes = self.faces[font as usize].encode(text);
        #[cfg(test)]
        self.drawn.push(Drawn {
            page,
            x0: x,
            x1: x + self.measure(font, size, text),
            y,
            text: text.to_string(),
        });
        let names = [b"F0".as_slice(), b"F1", b"F2"];
        let content = &mut self.pages[page].content;
        let [r, g, b] = colour;
        content.begin_text();
        content.set_fill_rgb(r, g, b);
        content.set_font(Name(names[font as usize]), size);
        content.next_line(x, y);
        content.show(Str(&bytes));
        content.end_text();
    }

    fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32, colour: Rgb) {
        let content = &mut self.page().content;
        let [r, g, b] = colour;
        content.set_fill_rgb(r, g, b);
        content.rect(x, y, w, h);
        content.fill_nonzero();
    }

    fn hline(&mut self, x0: f32, x1: f32, y: f32, colour: Rgb, width: f32) {
        let content = &mut self.page().content;
        let [r, g, b] = colour;
        content.set_stroke_rgb(r, g, b);
        content.set_line_width(width);
        content.move_to(x0, y);
        content.line_to(x1, y);
        content.stroke();
    }

    /// `text` cut into lines no wider than `width`: at its line breaks,
    /// between words, and inside a word too long for a line.
    fn wrap(&self, font: F, size: f32, text: &str, width: f32) -> Vec<String> {
        let face = &self.faces[font as usize];
        let mut lines = Vec::new();
        for paragraph in super::lines(text) {
            let paragraph: String = paragraph
                .chars()
                .map(|c| if c == '\t' { ' ' } else { c })
                .filter(|c| !c.is_control())
                .collect();
            // The indentation stays (a DDL's), at most half the width.
            let indent: String = paragraph.chars().take_while(|c| *c == ' ').collect();
            let indent = if face.width(&indent, size) * 2. > width {
                String::new()
            } else {
                indent
            };
            let mut line_width = face.width(&indent, size);
            let mut line = indent;
            // Where the words start on this line.
            let mut start = line.len();
            for word in paragraph.trim_start_matches(' ').split(' ') {
                let word_width = face.width(word, size);
                let space = if line.len() == start {
                    0.
                } else {
                    face.width(" ", size)
                };
                if line_width + space + word_width <= width {
                    if line.len() > start {
                        line.push(' ');
                    }
                    line.push_str(word);
                    line_width += space + word_width;
                    continue;
                }
                if line.len() > start {
                    lines.push(std::mem::take(&mut line));
                    (line_width, start) = (0., 0);
                }
                // A word longer than a line goes character by character.
                for c in word.chars() {
                    let advance = face.advance(c) * size;
                    if line_width + advance > width && line.len() > start {
                        lines.push(std::mem::take(&mut line));
                        (line_width, start) = (0., 0);
                    }
                    line.push(c);
                    line_width += advance;
                }
            }
            lines.push(line);
        }
        lines
    }

    /// A paragraph that continues on the next page when it must.
    fn paragraph(&mut self, font: F, size: f32, text: &str, colour: Rgb) {
        let leading = size * 1.4;
        let width = self.right() - self.left();
        for line in self.wrap(font, size, text, width) {
            self.ensure(leading);
            let baseline = self.y - size;
            let x = self.left();
            self.text(font, size, x, baseline, &line, colour);
            self.y -= leading;
        }
    }

    fn heading(&mut self, level: u8, text: &str) -> f32 {
        let (font, size, before, after) = match level {
            1 => (F::Semibold, 22., 0., 10.),
            2 => (F::Semibold, 14., 10., 6.),
            3 => (F::Semibold, 11., 14., 4.),
            _ => (F::Semibold, 7., 10., 4.),
        };
        let colour = if level == 4 {
            self.palette.muted
        } else {
            self.palette.text
        };
        let text = if level == 4 {
            text.to_uppercase()
        } else {
            text.to_string()
        };
        // A heading keeps a few lines of what follows with it.
        self.ensure(before + size * 1.3 + 40.);
        self.y -= before;
        let width = self.right() - self.left();
        for line in self.wrap(font, size, &text, width) {
            let baseline = self.y - size;
            let x = self.left();
            self.text(font, size, x, baseline, &line, colour);
            self.y -= size * 1.3;
        }
        if level == 2 {
            self.y -= 2.;
            let (x0, x1, y, border) = (self.left(), self.right(), self.y, self.palette.border);
            self.hline(x0, x1, y, border, 0.6);
        }
        self.y -= after;
        size
    }

    /// A table whose columns take the `widths` fractions of the page; its
    /// header row comes again on every page it continues on, a row taller
    /// than what is left continues on the next page.
    fn table(&mut self, header: &[&str], widths: &[f32], rows: Vec<Vec<(F, String, Rgb)>>) {
        let total = self.right() - self.left();
        let columns: Vec<f32> = widths.iter().map(|w| w * total).collect();
        let size = SMALL;
        let leading = size * 1.35;
        let header_height = leading + 2. * PAD;
        let draw_header = |writer: &mut Self| {
            let (x, surface, muted) = (writer.left(), writer.palette.surface, writer.palette.muted);
            writer.fill_rect(x, writer.y - header_height, total, header_height, surface);
            let mut cell_x = x;
            for (label, width) in header.iter().zip(&columns) {
                let baseline = writer.y - PAD - size;
                writer.text(F::Semibold, size, cell_x + PAD, baseline, label, muted);
                cell_x += width;
            }
            writer.y -= header_height;
        };
        self.ensure(header_height + leading + 2. * PAD);
        draw_header(self);
        for row in rows {
            let cells: Vec<Vec<String>> = row
                .iter()
                .zip(&columns)
                .map(|((font, text, _), width)| {
                    self.wrap(*font, size, text, (width - 2. * PAD).max(size))
                })
                .collect();
            let count = cells.iter().map(Vec::len).max().unwrap_or(1).max(1);
            let mut done = 0;
            while done < count {
                if self.y - (leading + 2. * PAD) < BOTTOM {
                    self.new_page();
                    draw_header(self);
                }
                let room = ((self.y - BOTTOM - 2. * PAD) / leading).floor().max(1.) as usize;
                let take = room.min(count - done);
                let mut cell_x = self.left();
                for (((font, _, colour), lines), width) in row.iter().zip(&cells).zip(&columns) {
                    for (index, line) in lines.iter().skip(done).take(take).enumerate() {
                        let baseline = self.y - PAD - size - index as f32 * leading;
                        self.text(*font, size, cell_x + PAD, baseline, line, *colour);
                    }
                    cell_x += width;
                }
                done += take;
                self.y -= take as f32 * leading + 2. * PAD;
                let (x0, x1, y, border) = (self.left(), self.right(), self.y, self.palette.border);
                self.hline(x0, x1, y, border, 0.5);
            }
        }
        self.y -= 6.;
    }

    /// A DDL block in the monospace font on the surface colour; it
    /// continues on the next page when it must.
    fn code(&mut self, text: &str) {
        let size = 7.;
        let leading = size * 1.4;
        let width = self.right() - self.left() - 2. * PAD;
        let lines = self.wrap(F::Mono, size, text.trim_end(), width);
        let mut index = 0;
        while index < lines.len() {
            self.ensure(leading + 2. * PAD);
            let room = ((self.y - BOTTOM - 2. * PAD) / leading).floor().max(1.) as usize;
            let take = room.min(lines.len() - index);
            let height = take as f32 * leading + 2. * PAD;
            let (x, surface, text_colour) = (self.left(), self.palette.surface, self.palette.text);
            self.fill_rect(x, self.y - height, self.right() - x, height, surface);
            for (row, line) in lines[index..index + take].iter().enumerate() {
                let baseline = self.y - PAD - size - row as f32 * leading;
                self.text(F::Mono, size, x + PAD, baseline, line, text_colour);
            }
            self.y -= height + 6.;
            index += take;
        }
    }

    /// The lockup's shapes, from the brand file, `height` points high with
    /// its top-left corner at (`x`, `top`).
    fn lockup(&mut self, svg: &str, x: f32, top: f32, height: f32) {
        let scale = height / 256.;
        let content = &mut self.pages.last_mut().expect("a page").content;
        for shape in brand_shapes(svg) {
            let [r, g, b] = shape.fill;
            content.set_fill_rgb(r, g, b);
            let map = |p: kurbo::Point| {
                let (px, py) = (
                    shape.translate.0 + p.x * shape.scale,
                    shape.translate.1 + p.y * shape.scale,
                );
                (x + px as f32 * scale, top - py as f32 * scale)
            };
            let mut last = kurbo::Point::ZERO;
            for element in shape.path.elements() {
                match *element {
                    kurbo::PathEl::MoveTo(p) => {
                        let (px, py) = map(p);
                        content.move_to(px, py);
                        last = p;
                    }
                    kurbo::PathEl::LineTo(p) => {
                        let (px, py) = map(p);
                        content.line_to(px, py);
                        last = p;
                    }
                    kurbo::PathEl::QuadTo(c, p) => {
                        let c1 = last + (c - last) * (2. / 3.);
                        let c2 = p + (c - p) * (2. / 3.);
                        let ((x1, y1), (x2, y2), (x3, y3)) = (map(c1), map(c2), map(p));
                        content.cubic_to(x1, y1, x2, y2, x3, y3);
                        last = p;
                    }
                    kurbo::PathEl::CurveTo(c1, c2, p) => {
                        let ((x1, y1), (x2, y2), (x3, y3)) = (map(c1), map(c2), map(p));
                        content.cubic_to(x1, y1, x2, y2, x3, y3);
                        last = p;
                    }
                    kurbo::PathEl::ClosePath => {
                        content.close_path();
                    }
                }
            }
            content.fill_nonzero();
        }
    }

    /// A rounded rectangle's path, `(x, y)` its bottom-left corner.
    fn rounded(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32) {
        let k = 0.552_284_8 * r;
        let content = &mut self.page().content;
        content.move_to(x + r, y);
        content.line_to(x + w - r, y);
        content.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
        content.line_to(x + w, y + h - r);
        content.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
        content.line_to(x + r, y + h);
        content.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
        content.line_to(x, y + r);
        content.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
        content.close_path();
    }

    /// The diagram, scaled to fit what is left of the page (its own page):
    /// vectors, so a reader zooms into it.
    fn diagram(&mut self, doc: &Document) {
        let diagram = doc.diagram();
        let layout = erd::layout(&diagram);
        let routes: Vec<_> = (0..diagram.edges.len())
            .map(|edge| erd::route(&diagram, &layout, edge))
            .collect();
        let Some(area) = erd::extent(&layout, &routes) else {
            return;
        };
        let (avail_w, avail_h) = (self.right() - self.left(), self.y - BOTTOM);
        let s = (avail_w / area.w).min(avail_h / area.h).min(1.);
        let (ox, oy) = (self.left() - area.x * s, self.y + area.y * s);
        let at = |x: f32, y: f32| (ox + x * s, oy - y * s);
        let p = (
            self.palette.background,
            self.palette.header,
            self.palette.border,
            self.palette.text,
            self.palette.muted,
        );
        let (surface, header, border, text, muted) = p;
        // Edges first, under the tables.
        {
            let content = &mut self.page().content;
            content.set_stroke_rgb(muted[0], muted[1], muted[2]);
            content.set_line_width(1.25 * s);
            for points in &routes {
                for (index, (x, y)) in points.iter().enumerate() {
                    let (px, py) = at(*x, *y);
                    if index == 0 {
                        content.move_to(px, py);
                    } else {
                        content.line_to(px, py);
                    }
                }
                for [(x1, y1), (x2, y2)] in erd::ends(points) {
                    let ((a, b), (c, d)) = (at(x1, y1), at(x2, y2));
                    content.move_to(a, b);
                    content.line_to(c, d);
                }
            }
            content.stroke();
        }
        for (index, (node, rect)) in diagram.nodes.iter().zip(&layout).enumerate() {
            let (x, top) = at(rect.x, rect.y);
            let (w, h) = (rect.w * s, rect.h * s);
            let band = erd::HEADER_HEIGHT * s;
            let r = 6. * s;
            let content = &mut self.page().content;
            content.set_fill_rgb(surface[0], surface[1], surface[2]);
            self.rounded(x, top - h, w, h, r);
            self.page().content.fill_nonzero();
            // The header band: rounded at the top only.
            let k = 0.552_284_8 * r;
            let content = &mut self.page().content;
            content.set_fill_rgb(header[0], header[1], header[2]);
            content.move_to(x, top - band);
            content.line_to(x, top - r);
            content.cubic_to(x, top - r + k, x + r - k, top, x + r, top);
            content.line_to(x + w - r, top);
            content.cubic_to(x + w - r + k, top, x + w, top - r + k, x + w, top - r);
            content.line_to(x + w, top - band);
            content.close_path();
            content.fill_nonzero();
            content.set_stroke_rgb(border[0], border[1], border[2]);
            content.set_line_width(0.75 * s.max(0.5));
            content.move_to(x, top - band);
            content.line_to(x + w, top - band);
            content.stroke();
            self.rounded(x, top - h, w, h, r);
            self.page().content.stroke();
            let title = diagram.title(index);
            let baseline = top - band / 2. - 4.5 * s;
            self.text(
                F::Semibold,
                13. * s,
                x + erd::PADDING * s,
                baseline,
                &title,
                text,
            );
            for (row, line) in erd::rows(node).iter().enumerate() {
                let baseline = top - band - (row as f32 + 0.5) * erd::ROW_HEIGHT * s - 4. * s;
                if !line.mark.is_empty() {
                    self.text(
                        F::Semibold,
                        9. * s,
                        x + erd::PADDING * s,
                        baseline,
                        line.mark,
                        muted,
                    );
                }
                let name_x = x + (erd::PADDING + erd::MARK_WIDTH) * s;
                self.text(F::Regular, 12. * s, name_x, baseline, &line.name, text);
                let type_width = self.measure(F::Mono, 11. * s, &line.data_type);
                let type_x = x + w - erd::PADDING * s - type_width;
                self.text(F::Mono, 11. * s, type_x, baseline, &line.data_type, muted);
            }
        }
        self.y -= area.h * s + 8.;
    }

    /// The footer of every page: the title, "page n of N".
    fn footers(&mut self, title: &str) {
        let count = self.pages.len();
        let title = super::one_line(title);
        for index in 0..count {
            let page = format!("page {} of {count}", index + 1);
            let size = SMALL;
            let page_width = self.measure(F::Regular, size, &page);
            let right = self.right();
            let space = right - self.left() - page_width - 24.;
            let shown = shorten(&self.faces[F::Regular as usize], &title, size, space);
            let (muted, x) = (self.palette.muted, self.left());
            self.text_on(index, (F::Regular, size), (x, FOOTER), &shown, muted);
            self.text_on(
                index,
                (F::Regular, size),
                (right - page_width, FOOTER),
                &page,
                muted,
            );
        }
    }
}

/// `text` cut with an ellipsis to fit `width` at `size`.
fn shorten(face: &Face, text: &str, size: f32, width: f32) -> String {
    if face.width(text, size) <= width {
        return text.to_string();
    }
    let ellipsis = face.width("…", size);
    let mut out = String::new();
    let mut used = 0.;
    for c in text.chars() {
        let advance = face.advance(c) * size;
        if used + advance + ellipsis > width {
            break;
        }
        out.push(c);
        used += advance;
    }
    out.push('…');
    out
}

/// A filled shape of a brand file.
struct Shape {
    fill: Rgb,
    translate: (f64, f64),
    scale: f64,
    path: kurbo::BezPath,
}

/// The `<rect>` and `<path>` elements of a brand lockup, in order.
// ponytail: reads the brand files' own forms only (a full-size `rect`,
// `path`s with `translate(x y) scale(s)`); use an SVG parser if they change.
fn brand_shapes(svg: &str) -> Vec<Shape> {
    let attribute = |element: &str, name: &str| -> Option<String> {
        let start = element.find(&format!(" {name}=\""))? + name.len() + 3;
        let end = element[start..].find('"')? + start;
        Some(element[start..end].to_string())
    };
    let mut shapes = Vec::new();
    for element in svg.split('<').skip(1) {
        let fill = attribute(element, "fill").map(|fill| rgb(&fill));
        if let Some(rest) = element.strip_prefix("rect") {
            let size = |name| attribute(rest, name).and_then(|v| v.parse::<f64>().ok());
            if let (Some(w), Some(h), Some(fill)) = (size("width"), size("height"), fill) {
                shapes.push(Shape {
                    fill,
                    translate: (0., 0.),
                    scale: 1.,
                    path: kurbo::Rect::new(0., 0., w, h).to_path(0.1),
                });
            }
        } else if element.starts_with("path") {
            let (Some(fill), Some(d)) = (fill, attribute(element, "d")) else {
                continue;
            };
            let Ok(path) = kurbo::BezPath::from_svg(&d) else {
                continue;
            };
            let transform = attribute(element, "transform").unwrap_or_default();
            let numbers = |name: &str| -> Vec<f64> {
                transform
                    .split(&format!("{name}("))
                    .nth(1)
                    .and_then(|rest| rest.split(')').next())
                    .map(|inside| {
                        inside
                            .split([' ', ','])
                            .filter_map(|n| n.parse().ok())
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let translate = numbers("translate");
            shapes.push(Shape {
                fill,
                translate: (
                    translate.first().copied().unwrap_or(0.),
                    translate.get(1).copied().unwrap_or(0.),
                ),
                scale: numbers("scale").first().copied().unwrap_or(1.),
                path,
            });
        }
    }
    shapes
}

/// Lays `doc` out; `known` gives the contents their page numbers.
fn layout<'f>(
    doc: &Document,
    options: &Options,
    fonts: &Fonts<'f>,
    known: Option<HashMap<Anchor, Place>>,
) -> Result<Writer<'f>, String> {
    let mut w = Writer::new(fonts, options, known)?;
    let tables = doc.tables();
    let views = doc.views();
    let code = doc.code(&CODE_KINDS);
    let with_diagram = options.diagram && !tables.is_empty();
    let p = &w.palette;
    let (text, muted, accent) = (p.text, p.muted, p.accent);

    // The cover: lockup, title, facts.
    match w.style {
        Style::KernoLight | Style::KernoDark => {
            let svg = if w.style == Style::KernoDark {
                LOCKUP_REVERSED
            } else {
                LOCKUP
            };
            let (x, top) = (w.left(), w.y);
            w.lockup(svg, x, top, LOCKUP_HEIGHT);
            // Clear space of one disk height (44 of 256 units) under it.
            w.y -= LOCKUP_HEIGHT + 44. / 256. * LOCKUP_HEIGHT + 8.;
        }
        Style::Plain => {}
    }
    w.heading(1, &super::one_line(&doc.title));
    let facts = doc.facts();
    let column = (w.right() - w.left()) / 3.;
    for chunk in facts.chunks(3) {
        let mut tallest = 0;
        let wrapped: Vec<Vec<String>> = chunk
            .iter()
            .map(|(_, value)| w.wrap(F::Regular, BODY, value, column - 12.))
            .collect();
        for lines in &wrapped {
            tallest = tallest.max(lines.len());
        }
        let height = SMALL * 1.4 + tallest as f32 * BODY * 1.4 + 8.;
        w.ensure(height);
        for (index, ((label, _), lines)) in chunk.iter().zip(&wrapped).enumerate() {
            let x = w.left() + index as f32 * column;
            let label_y = w.y - SMALL;
            w.text(F::Regular, SMALL, x, label_y, label, muted);
            for (row, line) in lines.iter().enumerate() {
                let baseline = w.y - SMALL * 1.4 - BODY - row as f32 * BODY * 1.4;
                w.text(F::Regular, BODY, x, baseline, line, text);
            }
        }
        w.y -= height;
    }

    // Contents, with links and (second pass) page numbers.
    w.heading(2, "Contents");
    let mut entries: Vec<(u8, String, Anchor)> = Vec::new();
    if with_diagram {
        entries.push((0, "Diagram".into(), Anchor::Diagram));
    }
    if !tables.is_empty() {
        entries.push((0, "Tables".into(), Anchor::Tables));
        for (index, table) in tables.iter().enumerate() {
            entries.push((1, table.table.name.clone(), Anchor::Table(index)));
        }
    }
    if !views.is_empty() {
        entries.push((0, "Views".into(), Anchor::Views));
        for (index, view) in views.iter().enumerate() {
            entries.push((1, view.name.clone(), Anchor::View(index)));
        }
    }
    if !code.is_empty() {
        entries.push((
            0,
            "Routines, sequences, types and triggers".into(),
            Anchor::Code,
        ));
        for (index, item) in code.iter().enumerate() {
            entries.push((
                1,
                format!("{} ({})", item.name, item.kind.label()),
                Anchor::CodeItem(index),
            ));
        }
    }
    if entries.is_empty() {
        w.paragraph(
            F::Regular,
            BODY,
            "Nothing was picked: the document lists no object.",
            muted,
        );
    }
    let leading = BODY * 1.5;
    for (level, label, anchor) in entries {
        w.ensure(leading);
        let indent = f32::from(level) * 14.;
        let number = w
            .known
            .as_ref()
            .and_then(|known| known.get(&anchor))
            .map(|(page, _)| (page + 1).to_string())
            .unwrap_or_default();
        let number_width = w.measure(F::Regular, BODY, &number);
        let font = if level == 0 { F::Semibold } else { F::Regular };
        let space = w.right() - w.left() - indent - number_width - 16.;
        let shown = shorten(
            &w.faces[font as usize],
            &super::one_line(&label),
            BODY,
            space,
        );
        let baseline = w.y - BODY;
        let x = w.left() + indent;
        w.text(font, BODY, x, baseline, &shown, accent);
        let right = w.right();
        w.text(
            F::Regular,
            BODY,
            right - number_width,
            baseline,
            &number,
            muted,
        );
        let rect = Rect::new(x, w.y - leading, right, w.y);
        w.page().links.push((rect, anchor));
        w.y -= leading;
    }

    if with_diagram {
        w.new_page();
        w.mark(Anchor::Diagram);
        w.heading(2, "Diagram");
        w.diagram(doc);
    }
    if !tables.is_empty() {
        w.new_page();
        w.mark(Anchor::Tables);
        w.heading(2, "Tables");
        for (index, table) in tables.iter().enumerate() {
            if index > 0 {
                w.break_page();
            }
            w.mark(Anchor::Table(index));
            table_section(&mut w, doc, table);
        }
    }
    let sections = [
        (Anchor::Views, "Views", &views),
        (
            Anchor::Code,
            "Routines, sequences, types and triggers",
            &code,
        ),
    ];
    for (anchor, heading, objects) in sections {
        if objects.is_empty() {
            continue;
        }
        w.break_page();
        w.mark(anchor);
        w.heading(2, heading);
        for (index, object) in objects.iter().enumerate() {
            let item = match anchor {
                Anchor::Views => Anchor::View(index),
                _ => Anchor::CodeItem(index),
            };
            w.ensure(60.);
            w.mark(item);
            w.heading(3, &format!("{} ({})", object.name, object.kind.label()));
            w.code(object.ddl);
        }
    }
    if w.style != Style::Plain {
        w.ensure(30.);
        w.y -= 16.;
        w.paragraph(F::Regular, SMALL, "Generated by Kerno", muted);
    }
    let title = doc.title.clone();
    w.footers(&title);
    Ok(w)
}

fn table_section(w: &mut Writer, doc: &Document, entry: &Table) {
    let Table {
        table,
        kind,
        structure,
    } = entry;
    let (text, muted, accent) = (w.palette.text, w.palette.muted, w.palette.accent);
    let title = match kind_label(*kind) {
        Some(kind) => format!("{} ({kind})", table.name),
        None => table.name.clone(),
    };
    w.heading(3, &title);
    if let Some(comment) = present(&structure.comment) {
        w.paragraph(F::Regular, BODY, comment, text);
        w.y -= 4.;
    }
    let cell = |font: F, value: &str, colour: Rgb| (font, value.to_string(), colour);
    let rows: Vec<_> = column_rows(structure)
        .into_iter()
        .map(|row| {
            vec![
                cell(F::Mono, &row.column.name, text),
                cell(F::Mono, &row.column.data_type, text),
                cell(
                    F::Regular,
                    if row.column.nullable { "yes" } else { "no" },
                    text,
                ),
                cell(F::Mono, row.default.as_deref().unwrap_or_default(), text),
                cell(F::Semibold, &row.keys, accent),
                cell(
                    F::Regular,
                    present(&row.column.comment).unwrap_or_default(),
                    text,
                ),
            ]
        })
        .collect();
    if rows.is_empty() {
        w.paragraph(F::Regular, BODY, "No columns.", muted);
    } else {
        w.table(
            &["Column", "Type", "Nullable", "Default", "Key", "Comment"],
            &[0.2, 0.2, 0.1, 0.2, 0.08, 0.22],
            rows,
        );
    }
    if let Some((name, columns)) = primary_key(structure) {
        w.heading(4, "Primary key");
        let line = match name {
            Some(name) => format!("{name} ({})", columns.join(", ")),
            None => format!("({})", columns.join(", ")),
        };
        w.paragraph(F::Mono, SMALL, &line, text);
    }
    let constraints: Vec<_> = structure
        .constraints
        .iter()
        .filter(|constraint| constraint.kind != ConstraintKind::PrimaryKey)
        .map(|constraint| {
            vec![
                cell(F::Mono, &constraint.name, text),
                cell(F::Regular, constraint_label(constraint.kind), text),
                cell(F::Mono, &constraint.definition, text),
            ]
        })
        .collect();
    if !constraints.is_empty() {
        w.heading(4, "Constraints");
        w.table(
            &["Name", "Kind", "Definition"],
            &[0.3, 0.12, 0.58],
            constraints,
        );
    }
    let indexes: Vec<_> = structure
        .indexes
        .iter()
        .filter(|index| !index.primary)
        .map(|index| {
            let columns = if index.columns.is_empty() {
                index.definition.clone().unwrap_or_default()
            } else {
                index.columns.join(", ")
            };
            vec![
                cell(F::Mono, &index.name, text),
                cell(F::Mono, &columns, text),
                cell(F::Regular, if index.unique { "yes" } else { "no" }, text),
                cell(F::Mono, &index.method, text),
            ]
        })
        .collect();
    if !indexes.is_empty() {
        w.heading(4, "Indexes");
        w.table(
            &["Name", "Columns", "Unique", "Method"],
            &[0.34, 0.38, 0.1, 0.18],
            indexes,
        );
    }
    let keys: Vec<_> = structure
        .foreign_keys
        .iter()
        .map(|key| {
            vec![
                cell(F::Mono, &key.name, text),
                cell(F::Mono, &key.columns.join(", "), text),
                cell(
                    F::Mono,
                    &format!(
                        "{} ({})",
                        referenced(doc, &key.referenced_table),
                        key.referenced_columns.join(", ")
                    ),
                    text,
                ),
                cell(F::Regular, &key.on_delete, text),
                cell(F::Regular, &key.on_update, text),
            ]
        })
        .collect();
    if !keys.is_empty() {
        w.heading(4, "Foreign keys");
        w.table(
            &["Name", "Columns", "References", "On delete", "On update"],
            &[0.26, 0.18, 0.26, 0.15, 0.15],
            keys,
        );
    }
}

/// A stream's bytes compressed for `FlateDecode`.
fn deflate(data: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    // Writing to a `Vec` does not fail.
    let _ = encoder.write_all(data);
    encoder.finish().unwrap_or_default()
}

/// The document as a PDF. `compress` off leaves the streams readable, for
/// the tests.
pub(super) fn to_pdf_with(
    doc: &Document,
    options: &Options,
    fonts: &Fonts,
    compress: bool,
) -> Result<Vec<u8>, String> {
    // Two passes: the first finds the page of each anchor, the second
    // writes the contents' page numbers (the same lines, so the same pages).
    let first = layout(doc, options, fonts, None)?;
    let known = first.anchors.clone();
    let writer = layout(doc, options, fonts, Some(known))?;
    assemble(writer, doc, options, compress)
}

pub(super) fn to_pdf(doc: &Document, options: &Options, fonts: &Fonts) -> Result<Vec<u8>, String> {
    to_pdf_with(doc, options, fonts, true)
}

/// The writer's laid out document; for the tests.
#[cfg(test)]
pub(super) fn laid_out<'f>(doc: &Document, options: &Options, fonts: &Fonts<'f>) -> Writer<'f> {
    layout(doc, options, fonts, None).expect("the fonts parse")
}

#[cfg(test)]
impl Writer<'_> {
    pub(super) fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub(super) fn page_box(&self) -> (f32, f32) {
        (self.width, self.height)
    }
}

fn assemble(
    writer: Writer,
    doc: &Document,
    options: &Options,
    compress: bool,
) -> Result<Vec<u8>, String> {
    let Writer {
        faces,
        pages,
        anchors,
        width,
        height,
        ..
    } = writer;
    let mut pdf = Pdf::with_settings(Settings { pretty: false });
    let mut next = 1;
    let mut alloc = || {
        let id = Ref::new(next);
        next += 1;
        id
    };
    let catalog = alloc();
    let tree = alloc();
    let info = alloc();
    let page_ids: Vec<Ref> = pages.iter().map(|_| alloc()).collect();
    let content_ids: Vec<Ref> = pages.iter().map(|_| alloc()).collect();
    let font_ids: Vec<Ref> = faces.iter().map(|_| alloc()).collect();
    let stream = |pdf: &mut Pdf, id: Ref, data: &[u8]| {
        if compress {
            let packed = deflate(data);
            pdf.stream(id, &packed).filter(Filter::FlateDecode);
        } else {
            pdf.stream(id, data);
        }
    };

    // Fonts: a Type0 font over a CID font of the subset, with a CIDToGIDMap
    // and a ToUnicode map.
    let used: Vec<bool> = faces.iter().map(|face| face.chars.len() > 1).collect();
    for (index, face) in faces.iter().enumerate() {
        if !used[index] {
            continue;
        }
        let (cid_font, descriptor, file, to_unicode, cid_map) =
            (alloc(), alloc(), alloc(), alloc(), alloc());
        let subset = subsetter::subset(face.data, 0, &face.remapper)
            .map_err(|error| format!("the font could not be subsetted: {error}"))?;
        let postscript = face
            .face
            .postscript_name
            .clone()
            .unwrap_or_else(|| format!("Font{index}"))
            .replace(|c: char| !c.is_ascii_alphanumeric() && c != '-', "");
        // The subset tag: six capitals, different per font.
        let tag: String = (0..6)
            .map(|i| (b'A' + ((face.chars.len() * 7 + index * 5 + i * 3) % 26) as u8) as char)
            .collect();
        let base = format!("{tag}+{postscript}");
        pdf.type0_font(font_ids[index])
            .base_font(Name(base.as_bytes()))
            .encoding_predefined(Name(b"Identity-H"))
            .descendant_font(cid_font)
            .to_unicode(to_unicode);
        let unit = 1000. * face.scale;
        let widths: Vec<f32> = face
            .chars
            .iter()
            .map(|(_, glyph)| f32::from(face.face.advance(*glyph)) * unit)
            .collect();
        let mut cid = pdf.cid_font(cid_font);
        cid.subtype(CidFontType::Type2)
            .base_font(Name(base.as_bytes()))
            .system_info(SystemInfo {
                registry: Str(b"Adobe"),
                ordering: Str(b"Identity"),
                supplement: 0,
            })
            .font_descriptor(descriptor)
            .cid_to_gid_map_stream(cid_map)
            .default_width(0.);
        cid.widths().consecutive(0, widths);
        cid.finish();
        let [x_min, y_min, x_max, y_max] = face.face.bbox.map(|v| f32::from(v) * unit);
        let mut flags = FontFlags::NON_SYMBOLIC;
        if face.face.monospaced {
            flags |= FontFlags::FIXED_PITCH;
        }
        pdf.font_descriptor(descriptor)
            .name(Name(base.as_bytes()))
            .flags(flags)
            .bbox(Rect::new(x_min, y_min, x_max, y_max))
            .italic_angle(face.face.italic_angle)
            .ascent(f32::from(face.face.ascender) * unit)
            .descent(f32::from(face.face.descender) * unit)
            .cap_height(f32::from(face.face.cap_height.unwrap_or(700)) * unit)
            .stem_v(80.)
            .font_file2(file);
        stream(&mut pdf, file, &subset);
        let map: Vec<u8> = face
            .chars
            .iter()
            .flat_map(|(_, glyph)| face.remapper.get(*glyph).unwrap_or(0).to_be_bytes())
            .collect();
        stream(&mut pdf, cid_map, &map);
        let info = SystemInfo {
            registry: Str(b"Adobe"),
            ordering: Str(b"UCS"),
            supplement: 0,
        };
        let mut cmap = UnicodeCmap::new(Name(b"Custom"), info);
        for (index, (c, _)) in face.chars.iter().enumerate().skip(1) {
            cmap.pair(index as u16, *c);
        }
        let cmap = cmap.finish();
        stream(&mut pdf, to_unicode, &cmap);
    }

    // Pages, their contents and the contents' links.
    let place = |anchor: &Anchor| anchors.get(anchor).copied();
    let mut annotations: Vec<Vec<Ref>> = Vec::new();
    for page in &pages {
        let ids: Vec<Ref> = page.links.iter().map(|_| alloc()).collect();
        annotations.push(ids);
    }
    for (index, page) in pages.into_iter().enumerate() {
        let mut writer = pdf.page(page_ids[index]);
        writer
            .parent(tree)
            .media_box(Rect::new(0., 0., width, height))
            .contents(content_ids[index]);
        writer.annotations(annotations[index].iter().copied());
        let mut resources = writer.resources();
        let mut fonts = resources.fonts();
        let names = [b"F0".as_slice(), b"F1", b"F2"];
        for (font, id) in font_ids.iter().enumerate() {
            if used[font] {
                fonts.pair(Name(names[font]), *id);
            }
        }
        fonts.finish();
        resources.finish();
        writer.finish();
        for ((rect, anchor), id) in page.links.iter().zip(&annotations[index]) {
            let Some((target, top)) = place(anchor) else {
                continue;
            };
            let mut annotation = pdf.annotation(*id);
            annotation.subtype(AnnotationType::Link).rect(*rect);
            annotation.border(0., 0., 0., None);
            annotation
                .action()
                .action_type(ActionType::GoTo)
                .destination()
                .page(page_ids[target])
                .xyz(0., top + 20., None);
        }
        let data = page.content.finish();
        stream(&mut pdf, content_ids[index], &data);
    }
    pdf.pages(tree)
        .kids(page_ids.iter().copied())
        .count(page_ids.len() as i32);

    // The outline: Diagram, Tables and each table, Views and each view,
    // the rest and each of it.
    let tables = doc.tables();
    let views = doc.views();
    let code = doc.code(&CODE_KINDS);
    let mut groups: Vec<(String, Anchor, Vec<Bookmark>)> = Vec::new();
    if anchors.contains_key(&Anchor::Diagram) {
        groups.push(("Diagram".into(), Anchor::Diagram, Vec::new()));
    }
    let children = |items: Vec<String>, of: fn(usize) -> Anchor| -> Vec<Bookmark> {
        items
            .into_iter()
            .enumerate()
            .map(|(index, name)| (super::one_line(&name), of(index)))
            .collect()
    };
    if !tables.is_empty() {
        let names = tables.iter().map(|t| t.table.name.clone()).collect();
        groups.push((
            "Tables".into(),
            Anchor::Tables,
            children(names, Anchor::Table),
        ));
    }
    let names = |items: &[Code]| items.iter().map(|item| item.name.clone()).collect();
    if !views.is_empty() {
        groups.push((
            "Views".into(),
            Anchor::Views,
            children(names(&views), Anchor::View),
        ));
    }
    if !code.is_empty() {
        groups.push((
            "Routines, sequences, types and triggers".into(),
            Anchor::Code,
            children(names(&code), Anchor::CodeItem),
        ));
    }
    let outline = alloc();
    let group_ids: Vec<Ref> = groups.iter().map(|_| alloc()).collect();
    let child_ids: Vec<Vec<Ref>> = groups
        .iter()
        .map(|(_, _, items)| items.iter().map(|_| alloc()).collect())
        .collect();
    let total: usize = groups.iter().map(|(_, _, items)| 1 + items.len()).sum();
    if !groups.is_empty() {
        pdf.outline(outline)
            .first(group_ids[0])
            .last(*group_ids.last().expect("a group"))
            .count(total as i32);
    }
    let destination = |anchor: &Anchor| place(anchor).unwrap_or((0, height));
    for (index, (title, anchor, items)) in groups.iter().enumerate() {
        let mut item = pdf.outline_item(group_ids[index]);
        item.title(TextStr(title)).parent(outline);
        if index > 0 {
            item.prev(group_ids[index - 1]);
        }
        if index + 1 < groups.len() {
            item.next(group_ids[index + 1]);
        }
        if !items.is_empty() {
            item.first(child_ids[index][0])
                .last(*child_ids[index].last().expect("an item"))
                .count(items.len() as i32);
        }
        let (page, top) = destination(anchor);
        item.dest().page(page_ids[page]).xyz(0., top + 20., None);
        item.finish();
        for (position, (name, anchor)) in items.iter().enumerate() {
            let ids = &child_ids[index];
            let mut child = pdf.outline_item(ids[position]);
            child.title(TextStr(name)).parent(group_ids[index]);
            if position > 0 {
                child.prev(ids[position - 1]);
            }
            if position + 1 < ids.len() {
                child.next(ids[position + 1]);
            }
            let (page, top) = destination(anchor);
            child.dest().page(page_ids[page]).xyz(0., top + 20., None);
        }
    }

    let mut catalog_writer = pdf.catalog(catalog);
    catalog_writer.pages(tree);
    if !groups.is_empty() {
        catalog_writer.outlines(outline);
    }
    catalog_writer.finish();
    let mut document = pdf.document_info(info);
    document.title(TextStr(&super::one_line(&doc.title)));
    if options.style != Style::Plain {
        document.creator(TextStr("Kerno"));
    }
    document.finish();
    Ok(pdf.finish())
}

mod sfnt;
#[cfg(test)]
mod tests;
