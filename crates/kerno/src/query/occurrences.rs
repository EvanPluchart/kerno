//! The other occurrences of the word or the one-line selection at the
//! cursor, highlighted in the SQL editor and selected all at once (CLAUDE.md
//! §4 J7, DBeaver's ⌥J). What is an occurrence is
//! [`kerno_core::sql::occurrences`].

use std::ops::Range;

use gpui_kit::base::input::{self, RangeDecorationStyle};
use gpui_kit::component::input::EditorState;
use gpui_kit::{App, Entity};
use kerno_core::sql::occurrences;

/// The highlights of one editor: a tint, and a frame around it (the kit
/// draws one style per decoration).
pub struct Occurrences {
    marks: input::RangeDecorationCollection,
    frames: input::RangeDecorationCollection,
    /// The selection and text length the marks were computed for; the text
    /// changed since when `stale`.
    seen: Option<(Range<usize>, usize)>,
    stale: bool,
}

impl Occurrences {
    pub fn new(editor: &Entity<EditorState>, cx: &mut App) -> Self {
        let (marks, frames) = editor.update(cx, |editor, cx| {
            (
                editor.create_range_decorations_collection(Vec::new(), cx),
                editor.create_range_decorations_collection(Vec::new(), cx),
            )
        });
        Self {
            marks,
            frames,
            seen: None,
            stale: true,
        }
    }

    /// The text changed: the next [`Self::refresh`] computes again.
    pub fn text_changed(&mut self) {
        self.stale = true;
    }

    /// Marks the occurrences for the editor's selection, when it or the
    /// text changed since the last time (the editor notifies at every blink
    /// of its cursor: nothing is computed then).
    pub fn refresh(&mut self, editor: &Entity<EditorState>, cx: &mut App) {
        let state = editor.read(cx);
        let selection = state.selected_range();
        let seen = Some((selection.clone(), state.text().len()));
        if !self.stale && self.seen == seen {
            return;
        }
        self.stale = false;
        self.seen = seen;
        let found = occurrences(&state.text().to_string(), selection.clone());
        // One occurrence is the word itself: nothing to point at.
        let marks: Vec<Range<usize>> = if found.len() < 2 {
            Vec::new()
        } else {
            found
                .into_iter()
                .filter(|at| !(at.start <= selection.start && selection.end <= at.end))
                .collect()
        };
        // Setting them notifies the editor, which calls this again.
        if self.marks.get_ranges(cx) != marks {
            let (fill, edge) = (
                crate::theme::pick(crate::theme::OCCURRENCE_FILL, cx),
                crate::theme::pick(crate::theme::MARK_EDGE, cx),
            );
            let decorations = |style, color| {
                marks
                    .iter()
                    .map(|range| {
                        input::RangeDecoration::new(range.clone())
                            .with_style(style)
                            .with_color(color)
                    })
                    .collect()
            };
            let (fills, frames) = (
                decorations(RangeDecorationStyle::Fill, fill),
                decorations(RangeDecorationStyle::Frame, edge),
            );
            self.marks.set(fills, cx);
            self.frames.set(frames, cx);
        }
    }

    /// The highlighted ranges; for tests.
    #[cfg(test)]
    pub fn marked(&self, cx: &App) -> Vec<Range<usize>> {
        self.marks.get_ranges(cx)
    }

    /// Selects every occurrence of the word or selection at the cursor, the
    /// one at the cursor first: typing then changes them all. Returns how
    /// many were selected.
    pub fn select_all(editor: &Entity<EditorState>, cx: &mut App) -> usize {
        editor.update(cx, |editor, cx| {
            let selection = editor.selected_range();
            let found = occurrences(&editor.text().to_string(), selection.clone());
            let Some(current) = found
                .iter()
                .position(|at| at.start <= selection.start && selection.end <= at.end)
            else {
                return 0;
            };
            // `add_selection` scrolls to each: the one at the cursor comes
            // last, so the view stays on it.
            let mut order: Vec<Range<usize>> = found
                .iter()
                .enumerate()
                .filter(|&(index, _)| index != current)
                .map(|(_, range)| range.clone())
                .collect();
            order.push(found[current].clone());
            editor.set_selected_range(order[0].clone(), cx);
            for range in &order[1..] {
                editor.add_selection(range.clone(), cx);
            }
            found.len()
        })
    }
}
