//! Where a run stands, in the SQL editor's left margin (K37, D-110).
//!
//! While statements run, the margin beside each statement the run reached
//! says how it stands: running (a play sign in a breathing halo), done (a
//! tick), failed (a cross), cancelled (a square), or refused on a read-only
//! connection (a barred circle). Statements not reached yet have nothing.
//! After the run the marks stay as its trace until the next run from the
//! editor or the first edit of the text. DBeaver annotates its ruler the
//! same way while a script runs (its SQL editor's query job; behaviour
//! only, no code taken).
//!
//! The kit reserves the strip (a gutter decoration collection kept empty)
//! and says where each line is drawn; the marks are painted here, over the
//! editor, after its layout of the frame. The kit's own gutter marks are
//! flat bars of one colour each: no shape, which colour alone cannot carry.

use std::ops::Range;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::input::{EditorState, Rope, RopeExt as _};
use gpui_kit::{
    App, Bounds, ContentMask, Hsla, IntoElement, PathBuilder, Pixels, Point, Styled as _, Window,
    canvas, fill, point, px, size,
};

/// The width of the strip the marks are drawn in, left of the line numbers.
pub const STRIP: Pixels = px(12.);
/// The largest a mark is drawn.
const GLYPH: Pixels = px(10.);
/// One breath of the running mark's halo.
const BREATH_MS: u128 = 1200;

/// How a statement of the run stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Running,
    Done,
    Failed,
    Cancelled,
    /// A write not sent: the connection is read-only (J14).
    Refused,
}

impl Mark {
    /// The mark in a word.
    pub fn word(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Refused => "refused",
        }
    }

    /// The theme file's key of the mark's colour (checked at 3:1 on the
    /// editor in `theme.rs`).
    #[cfg(test)]
    pub fn theme_key(self) -> &'static str {
        match self {
            Self::Running => "info.background",
            Self::Done => "success.background",
            Self::Failed => "danger.background",
            Self::Cancelled => "muted.foreground",
            Self::Refused => "warning.background",
        }
    }

    #[cfg(test)]
    pub const ALL: [Mark; 5] = [
        Self::Running,
        Self::Done,
        Self::Failed,
        Self::Cancelled,
        Self::Refused,
    ];

    fn color(self, cx: &App) -> Hsla {
        let theme = cx.theme();
        match self {
            Self::Running => theme.info,
            Self::Done => theme.success,
            Self::Failed => theme.danger,
            Self::Cancelled => theme.muted_foreground,
            Self::Refused => theme.warning,
        }
    }

    /// Painted over the marks of lower layers on a shared row (two
    /// statements on one line, or inside one fold): a failure or the
    /// running statement is never hidden by a statement that went well.
    fn layer(self) -> u8 {
        match self {
            Self::Done => 0,
            Self::Refused => 1,
            Self::Cancelled => 2,
            Self::Failed => 3,
            Self::Running => 4,
        }
    }
}

/// A statement of the run, by its lines in the editor.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Placed {
    /// Its index in the run.
    statement: usize,
    /// The buffer lines it covers.
    lines: Range<usize>,
    mark: Option<Mark>,
}

/// The marks of the last run from the editor.
#[derive(Debug, Default)]
pub struct Trace {
    /// In the order of the text, which is the order of the run.
    placed: Vec<Placed>,
    /// The length of the text the places were read in: a text of another
    /// length is not the one the marks describe.
    text_len: usize,
}

impl Trace {
    /// A run of the statements at `places` (index in the run, bytes in
    /// `text`), in the order of the text; none reached yet.
    pub fn new(text: &Rope, places: impl IntoIterator<Item = (usize, Range<usize>)>) -> Self {
        let placed = places
            .into_iter()
            .map(|(statement, range)| {
                let first = text.offset_to_point(range.start).row;
                let mut last = text.offset_to_point(range.end).row;
                // A range ending at a line's start does not touch that line.
                if last > first && text.line_start_offset(last) == range.end {
                    last -= 1;
                }
                Placed {
                    statement,
                    lines: first..last + 1,
                    mark: None,
                }
            })
            .collect();
        Self {
            placed,
            text_len: text.len(),
        }
    }

    /// Statement `statement` of the run now stands as `mark`. Nothing for a
    /// statement not read from the editor, or after the trace was cleared.
    pub fn set(&mut self, statement: usize, mark: Mark) {
        if let Ok(at) = self
            .placed
            .binary_search_by_key(&statement, |placed| placed.statement)
        {
            self.placed[at].mark = Some(mark);
        }
    }

    /// The run was stopped: the statement it waited on ends as `now`.
    pub fn stopped(&mut self, now: Mark) {
        for placed in &mut self.placed {
            if placed.mark == Some(Mark::Running) {
                placed.mark = Some(now);
            }
        }
    }

    /// The text was edited: the places no longer hold.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Whether a run from the editor has a trace.
    pub fn is_empty(&self) -> bool {
        self.placed.is_empty()
    }

    /// The mark of each statement of the run read from the editor.
    pub fn marks(&self) -> Vec<Option<Mark>> {
        self.placed.iter().map(|placed| placed.mark).collect()
    }

    /// Where the run ended, in words, for a run of several statements:
    /// the first that failed, was cancelled or refused, else how many ran.
    /// What the marks say without their shapes (K37). Nothing when the
    /// text, `text_len` long, is not the one the marks describe.
    pub fn summary(&self, text_len: usize) -> Option<String> {
        let total = self.placed.len();
        let marks = self.marks();
        if total < 2 || text_len != self.text_len || marks.contains(&Some(Mark::Running)) {
            return None;
        }
        let stopped = [Mark::Failed, Mark::Cancelled, Mark::Refused]
            .into_iter()
            .find_map(|mark| Some((marks.iter().position(|m| *m == Some(mark))?, mark)));
        let done = marks.iter().filter(|m| **m == Some(Mark::Done)).count();
        Some(match stopped {
            Some((at, mark)) => format!("statement {} of {total} {}", at + 1, mark.word()),
            None if done == 0 => return None,
            None => format!("{done} of {total} statements done"),
        })
    }

    /// The statements that cover some of `lines`.
    fn meeting(&self, lines: Range<usize>) -> &[Placed] {
        // Statements do not overlap: both ends grow along the list.
        let from = self
            .placed
            .partition_point(|placed| placed.lines.end <= lines.start);
        let to = self
            .placed
            .partition_point(|placed| placed.lines.start < lines.end);
        &self.placed[from..to.max(from)]
    }
}

/// A mark as drawn: the top of its statement's first row and the bottom of
/// its last, in window coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Drawn {
    pub mark: Mark,
    pub top: Pixels,
    pub bottom: Pixels,
}

/// The marks of the rows on screen, as the editor last laid them out.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    /// The editor's viewport: nothing is drawn outside it.
    pub clip: Bounds<Pixels>,
    pub line_height: Pixels,
    /// In paint order.
    pub marks: Vec<Drawn>,
}

/// Where the marks of `trace` go in `editor`, from its latest layout: rows
/// of a soft-wrapped line count, a statement that starts in a fold is marked
/// on the fold's row, and only statements on rows on screen are looked at.
/// `None` before the first layout, without marks, or when the text is not
/// the one the trace was made in.
pub fn layout(editor: &EditorState, trace: &Trace) -> Option<Layout> {
    if trace.is_empty() || editor.text().len() != trace.text_len {
        return None;
    }
    let lines = editor.visible_row_range()?;
    let line_height = editor.line_height()?;
    // The text's origin, scrolled: the kit's own gutter marks start there.
    let origin = editor.text_bounds()?.origin;
    let map = editor.display_map();
    let mut marks: Vec<Drawn> = trace
        .meeting(lines)
        .iter()
        .filter_map(|placed| {
            let mark = placed.mark?;
            let first = map.buffer_line_to_display_row(placed.lines.start);
            let last = placed.lines.end - 1;
            let end = map
                .buffer_line_to_display_row_range(last)
                .map_or_else(|| map.buffer_line_to_display_row(last) + 1, |rows| rows.end)
                .max(first + 1);
            Some(Drawn {
                mark,
                top: origin.y + line_height * first as f32,
                bottom: origin.y + line_height * end as f32,
            })
        })
        .collect();
    if marks.is_empty() {
        return None;
    }
    marks.sort_by_key(|drawn| drawn.mark.layer());
    Some(Layout {
        clip: editor.input_bounds(),
        line_height,
        marks,
    })
}

/// The marks, drawn over `editor` once it is laid out: placed after it, so
/// that its layout of this frame is the one read.
pub fn overlay(
    editor: gpui_kit::Entity<EditorState>,
    trace: std::rc::Rc<std::cell::RefCell<Trace>>,
) -> impl IntoElement {
    canvas(
        move |_, _, cx| layout(editor.read(cx), &trace.borrow()),
        |_, layout, window, cx| {
            if let Some(layout) = layout {
                paint(&layout, window, cx);
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

fn paint(layout: &Layout, window: &mut Window, cx: &mut App) {
    let center_x = layout.clip.origin.x + STRIP / 2.;
    let glyph = (layout.line_height * 0.6).min(GLYPH);
    // How far into its breath the running mark's halo is, 0 to 1.
    let breath = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0., |now| {
            (now.as_millis() % BREATH_MS) as f32 / BREATH_MS as f32
        });
    window.with_content_mask(
        Some(ContentMask {
            bounds: layout.clip,
        }),
        |window| {
            for drawn in &layout.marks {
                let color = drawn.mark.color(cx);
                let middle = drawn.top + layout.line_height / 2.;
                // A statement of several rows: a thin line down its rows.
                if drawn.bottom - drawn.top > layout.line_height * 1.5 {
                    window.paint_quad(fill(
                        Bounds::from_corners(
                            point(center_x - px(1.), middle + glyph / 2. + px(2.)),
                            point(center_x + px(1.), drawn.bottom - px(2.)),
                        ),
                        color.opacity(0.55),
                    ));
                }
                paint_glyph(
                    drawn.mark,
                    point(center_x, middle),
                    glyph,
                    color,
                    breath,
                    window,
                );
            }
        },
    );
}

/// The shape of `mark`, `side` wide, centred on `center`.
fn paint_glyph(
    mark: Mark,
    center: Point<Pixels>,
    side: Pixels,
    color: Hsla,
    breath: f32,
    window: &mut Window,
) {
    let at = |x: f32, y: f32| point(center.x + side * x, center.y + side * y);
    let stroke = (side * 0.17).max(px(1.5));
    let line = |window: &mut Window, points: &[Point<Pixels>]| {
        let mut path = PathBuilder::stroke(stroke);
        path.move_to(points[0]);
        for point in &points[1..] {
            path.line_to(*point);
        }
        if let Ok(path) = path.build() {
            window.paint_path(path, color);
        }
    };
    match mark {
        Mark::Running => {
            // The halo breathes in and out: the server is at work.
            let swell = 0.5 - 0.5 * (breath * std::f32::consts::TAU).cos();
            let halo = side * 1.7;
            window.paint_quad(
                fill(
                    Bounds::centered_at(center, size(halo, halo)),
                    color.opacity(0.1 + 0.25 * swell),
                )
                .corner_radii(halo / 2.),
            );
            let mut path = PathBuilder::fill();
            path.move_to(at(-0.32, -0.5));
            path.line_to(at(0.5, 0.));
            path.line_to(at(-0.32, 0.5));
            path.close();
            if let Ok(path) = path.build() {
                window.paint_path(path, color);
            }
        }
        Mark::Done => line(window, &[at(-0.45, 0.), at(-0.12, 0.35), at(0.45, -0.38)]),
        Mark::Failed => {
            line(window, &[at(-0.4, -0.4), at(0.4, 0.4)]);
            line(window, &[at(0.4, -0.4), at(-0.4, 0.4)]);
        }
        Mark::Cancelled => window.paint_quad(fill(
            Bounds::centered_at(center, size(side * 0.75, side * 0.75)),
            color,
        )),
        Mark::Refused => {
            window.paint_quad(
                fill(
                    Bounds::centered_at(center, size(side, side)),
                    gpui_kit::transparent_black(),
                )
                .corner_radii(side / 2.)
                .border_widths(stroke)
                .border_color(color),
            );
            line(window, &[at(-0.33, 0.33), at(0.33, -0.33)]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace(text: &str, places: &[Range<usize>]) -> Trace {
        Trace::new(&Rope::from(text), places.iter().cloned().enumerate())
    }

    #[test]
    fn a_statement_covers_the_lines_of_its_text() {
        let text = "select 1;\nselect\n  2;\n\nselect 3; select 4;\n";
        let trace = trace(text, &[0..8, 10..20, 23..31, 33..41]);
        let lines: Vec<Range<usize>> = trace.placed.iter().map(|p| p.lines.clone()).collect();
        // A range ending at the start of a line does not cover it; two
        // statements share a line.
        assert_eq!(lines, vec![0..1, 1..3, 4..5, 4..5]);
        let ends_at_a_line = Trace::new(&Rope::from(text), [(0, 0..10)]);
        assert_eq!(ends_at_a_line.placed[0].lines, 0..1);
    }

    #[test]
    fn marks_follow_the_run_and_a_stop_cancels_the_running_one() {
        let mut trace = trace("a;\nb;\nc;", &[0..1, 3..4, 6..7]);
        assert_eq!(trace.marks(), vec![None, None, None]);
        trace.set(0, Mark::Done);
        trace.set(1, Mark::Running);
        assert_eq!(
            trace.marks(),
            vec![Some(Mark::Done), Some(Mark::Running), None]
        );
        trace.stopped(Mark::Cancelled);
        assert_eq!(
            trace.marks(),
            vec![Some(Mark::Done), Some(Mark::Cancelled), None]
        );
        // A statement not read from the editor has no place.
        trace.set(7, Mark::Failed);
        trace.clear();
        trace.set(0, Mark::Failed);
        assert!(trace.is_empty());
    }

    #[test]
    fn the_marks_are_said_in_words_once_the_run_ended() {
        let text = "a;\nb;\nc;";
        let mut trace = trace(text, &[0..1, 3..4, 6..7]);
        assert_eq!(trace.summary(text.len()), None);
        trace.set(0, Mark::Done);
        trace.set(1, Mark::Running);
        assert_eq!(trace.summary(text.len()), None);
        trace.set(1, Mark::Done);
        assert_eq!(
            trace.summary(text.len()),
            Some("2 of 3 statements done".into())
        );
        trace.set(2, Mark::Failed);
        assert_eq!(
            trace.summary(text.len()),
            Some("statement 3 of 3 failed".into())
        );
        // Another text: the marks no longer say anything.
        assert_eq!(trace.summary(text.len() + 1), None);
        // One statement alone says nothing more than its timing.
        let mut one = Trace::new(&Rope::from("a;"), [(0, 0..1)]);
        one.set(0, Mark::Failed);
        assert_eq!(one.summary(2), None);
    }

    #[test]
    fn only_the_statements_of_the_lines_asked_are_met() {
        let text: String = (0..5000).map(|n| format!("insert {n};\n")).collect();
        let mut at = 0;
        let places: Vec<Range<usize>> = (0..5000)
            .map(|n| {
                let len = format!("insert {n};").len();
                let range = at..at + len;
                at += len + 1;
                range
            })
            .collect();
        let trace = trace(&text, &places);
        let met: Vec<usize> = trace
            .meeting(2000..2040)
            .iter()
            .map(|p| p.statement)
            .collect();
        assert_eq!(met, (2000..2040).collect::<Vec<_>>());
        assert!(trace.meeting(6000..6010).is_empty());
        // A statement of many lines is met by any of them.
        let long = Trace::new(&Rope::from("a\nb\nc\nd;\ne;"), [(0, 0..8), (1, 9..10)]);
        let met: Vec<usize> = long.meeting(2..3).iter().map(|p| p.statement).collect();
        assert_eq!(met, vec![0]);
    }

    #[test]
    fn a_failure_or_the_running_mark_is_painted_last() {
        let mut layers: Vec<Mark> = Mark::ALL.to_vec();
        layers.sort_by_key(|mark| mark.layer());
        assert_eq!(layers.last(), Some(&Mark::Running));
        assert!(Mark::Failed.layer() > Mark::Done.layer());
    }
}
