//! The value viewer: one cell in full, as text, as indented JSON or as a
//! hex dump; and the value editor dialog, which edits a cell that takes
//! typed values (DBeaver's `row.edit`, Shift+Enter). The docked value panel
//! (`super::value_panel`, K17) draws and saves values with the same
//! functions.

use std::rc::Rc;

use gpui_kit::base::TestTextExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, ClipboardItem, Context, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    TestSupportExt as _, Window, div, px,
};
use gpui_kit::{Entity, FocusHandle, Focusable as _};
use kerno_core::format::{byte_size, compact_json, hex_dump, pretty_json, unix_time_hint};
use kerno_core::sql::CellValue;
use kerno_core::{Column, TypeKind};

/// The most a view draws: beyond it the text is cut and says so (the
/// clipboard gets everything).
pub(super) const MAX_SHOWN_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum View {
    Text,
    Json,
    Hex,
}

struct ValueViewer {
    /// `None` for NULL.
    value: Option<Vec<u8>>,
    /// The value indented, when it is JSON.
    json: Option<String>,
    view: View,
    views: Vec<View>,
    /// When the value is an integer that looks like a Unix time.
    hint: Option<String>,
}

/// Makes a value a pending change of the cell.
pub type Apply = dyn Fn(CellValue, &mut Window, &mut App);

/// What saving the value panel does.
pub struct Save {
    /// The column takes NULL.
    pub nullable: bool,
    pub apply: Rc<Apply>,
}

/// Opens the value panel on a cell of `column`: editable with `save`
/// (binary values are only shown), else the viewer.
/// `back` takes the keyboard once the panel closes.
pub fn edit(
    column: &Column,
    value: Option<&[u8]>,
    save: Option<Save>,
    back: FocusHandle,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(save) = save.filter(|_| column.kind != TypeKind::Binary) else {
        return show(column, value, Some(back), window, cx);
    };
    let title = SharedString::from(format!("{} · {}", column.name, column.type_name));
    let fetched = value.map_or(String::new(), |bytes| {
        String::from_utf8_lossy(bytes).into_owned()
    });
    let json = column.kind == TypeKind::Json;
    // JSON is edited laid out; left as it was, it is no change.
    let shown = if json {
        pretty_json(&fetched).unwrap_or_else(|| fetched.clone())
    } else {
        fetched.clone()
    };
    let kind = column.kind;
    let editor = cx.new(|cx| ValueEditor {
        text: cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(14)
                .default_value(shown.clone())
        }),
        shown,
        fetched,
        kind,
        problem: None,
        hint: time_hint(column, value),
        save,
    });
    let text = editor.read(cx).text.clone();
    let back = Some(back);
    window.open_dialog(cx, move |dialog, _, _| {
        let (editor, back) = (editor.clone(), back.clone());
        dialog
            .title(title.clone())
            .w(px(720.))
            // Enter is a new line, never the dialog's OK.
            .on_ok(|_, _, _| false)
            .on_close(move |_, window, cx| give_back(back.clone(), window, cx))
            .content(move |content, _, _| content.child(editor.clone()))
    });
    window.defer(cx, move |window, cx| {
        text.read(cx).focus_handle(cx).focus(window, cx)
    });
}

/// What an integer cell that looks like a Unix time is (Beekeeper #941).
pub(super) fn time_hint(column: &Column, value: Option<&[u8]>) -> Option<String> {
    (column.kind == TypeKind::Integer)
        .then(|| unix_time_hint(std::str::from_utf8(value?).ok()?))
        .flatten()
}

/// The line that says what time an integer is.
pub(super) fn hint_line(hint: Option<String>, cx: &App) -> Option<impl IntoElement> {
    hint.map(|hint| {
        div()
            .id("value-unix-time")
            .test_support()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(SharedString::from(hint.clone()))
            .test_text(hint)
    })
}

/// Once a dialog has closed, the keyboard goes back where it was.
pub fn give_back(focus: Option<FocusHandle>, window: &mut Window, cx: &mut App) {
    if let Some(focus) = focus {
        window.defer(cx, move |window, cx| focus.focus(window, cx));
    }
}

/// The value panel in edit mode.
struct ValueEditor {
    text: Entity<TextareaState>,
    /// What the panel showed first.
    shown: String,
    /// What the cell holds.
    fetched: String,
    kind: TypeKind,
    /// Why the text cannot be saved.
    problem: Option<String>,
    /// When the value is an integer that looks like a Unix time.
    hint: Option<String>,
    save: Save,
}

impl ValueEditor {
    /// Saves the text as the cell's value, unless it is JSON the parser
    /// refuses. Text left as shown is no change; emptied text is NULL
    /// except in a text column, as when typing in the cell.
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.text.read(cx).value().to_string();
        match saved_value(&typed, &self.shown, &self.fetched, self.kind) {
            Err(problem) => {
                self.problem = Some(problem);
                cx.notify();
                return;
            }
            Ok(Some(value)) => (self.save.apply)(value, window, cx),
            Ok(None) => {}
        }
        window.close_dialog(cx);
    }

    /// Lays the JSON out on lines (`pretty`) or on one line.
    fn reformat(&mut self, pretty: bool, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.text.read(cx).value().to_string();
        let rewritten = if pretty {
            pretty_json(&typed)
        } else {
            compact_json(&typed)
        };
        match rewritten {
            Some(text) => {
                self.problem = None;
                self.text
                    .update(cx, |state, cx| state.set_value(text, window, cx));
            }
            None => self.problem = Some(json_problem(&typed)),
        }
        cx.notify();
    }
}

/// What saving `typed` gives, in an editor that showed `shown` of a cell
/// holding `fetched`: no change (`None`) when the text is as shown; for a
/// JSON column, why the text is not JSON (with where the parser stopped),
/// and nothing is written; else as typing in the cell ([`super::grid::typed_value`]:
/// emptied text is NULL except in a text column).
pub(super) fn saved_value(
    typed: &str,
    shown: &str,
    fetched: &str,
    kind: TypeKind,
) -> Result<Option<CellValue>, String> {
    if typed == shown {
        return Ok(None);
    }
    if kind == TypeKind::Json
        && !typed.is_empty()
        && serde_json::from_str::<serde_json::Value>(typed).is_err()
    {
        return Err(json_problem(typed));
    }
    Ok(super::grid::typed_value(typed, fetched, kind))
}

/// Why `text` is not JSON, with the line and column where the parser
/// stopped.
pub(super) fn json_problem(text: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(_) => String::new(),
        Err(error) => format!("This is not valid JSON: {error}."),
    }
}

impl Render for ValueEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let json = self.kind == TypeKind::Json;
        v_flex()
            .id("value-editor")
            .test_support()
            .gap_3()
            .pt_1()
            .child(
                div()
                    .font_family(theme.mono_font_family.clone())
                    .text_sm()
                    .child(Textarea::new(&self.text)),
            )
            .children(hint_line(self.hint.clone(), cx))
            .children(self.problem.clone().map(|problem| {
                div()
                    .id("value-problem")
                    .test_support()
                    .text_sm()
                    .text_color(theme.danger)
                    .child(SharedString::from(problem))
            }))
            .child(
                h_flex()
                    .gap_2()
                    .when(json, |bar| {
                        bar.child(
                            Button::new("value-pretty")
                                .ghost()
                                .label("Format JSON")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.reformat(true, window, cx)
                                })),
                        )
                        .child(
                            Button::new("value-compact")
                                .ghost()
                                .label("Compact JSON")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.reformat(false, window, cx)
                                })),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        Button::new("value-null")
                            .outline()
                            .label("Set NULL")
                            .disabled(!self.save.nullable)
                            .on_click(cx.listener(|this, _, window, cx| {
                                (this.save.apply)(CellValue::Null, window, cx);
                                window.close_dialog(cx);
                            })),
                    )
                    .child(
                        Button::new("value-cancel")
                            .outline()
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("value-save")
                            .primary()
                            .label("Save")
                            .tooltip("Keep the value as a pending change")
                            .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                    ),
            )
    }
}

/// Opens the viewer on a cell of `column`; the keyboard comes back where
/// it is.
pub fn open(column: &Column, value: Option<&[u8]>, window: &mut Window, cx: &mut App) {
    let back = window.focused(cx);
    show(column, value, back, window, cx);
}

/// Opens the viewer; `back` takes the keyboard once it closes.
fn show(
    column: &Column,
    value: Option<&[u8]>,
    back: Option<FocusHandle>,
    window: &mut Window,
    cx: &mut App,
) {
    let title = SharedString::from(format!("{} · {}", column.name, column.type_name));
    let (views, json) = views_for(column, value);
    let viewer = cx.new(|_| ValueViewer {
        value: value.map(<[u8]>::to_vec),
        json,
        view: views.first().copied().unwrap_or(View::Text),
        views,
        hint: time_hint(column, value),
    });
    window.open_dialog(cx, move |dialog, _, _| {
        let (viewer, back) = (viewer.clone(), back.clone());
        dialog
            .title(title.clone())
            .w(px(720.))
            .on_close(move |_, window, cx| give_back(back.clone(), window, cx))
            .content(move |content, _, _| content.child(viewer.clone()))
    });
}

/// The views of a value, the first shown first, and the value indented
/// when JSON is one of them: only for a JSON column, or a text that holds a
/// JSON object or array (a number, a string or a boolean is no document).
/// Indenting a large document takes a while: the value panel calls this off
/// the UI thread.
pub(super) fn views_for(column: &Column, value: Option<&[u8]>) -> (Vec<View>, Option<String>) {
    let text = value.and_then(|bytes| std::str::from_utf8(bytes).ok());
    let document = |text: &str| matches!(text.trim_start().as_bytes().first(), Some(b'{' | b'['));
    let json = text
        .filter(|text| column.kind == TypeKind::Json || document(text))
        .and_then(pretty_json);
    let binary = column.kind == TypeKind::Binary || (value.is_some() && text.is_none());
    let mut views = Vec::new();
    if json.is_some() {
        views.push(View::Json);
    }
    if !binary {
        views.push(View::Text);
    }
    if value.is_some() {
        views.push(View::Hex);
    }
    (views, json)
}

/// What a view of a value shows; the hex dump of its first bytes only.
pub(super) fn view_text(bytes: &[u8], view: View, json: Option<&str>) -> String {
    match view {
        View::Json => json.unwrap_or_default().to_string(),
        View::Hex => hex_dump(&bytes[..bytes.len().min(MAX_SHOWN_BYTES / 4)]),
        View::Text => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// Where a text too long to draw is cut, on a character boundary.
pub(super) fn cut_at(text: &str) -> Option<usize> {
    (text.len() > MAX_SHOWN_BYTES).then(|| {
        let mut end = MAX_SHOWN_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        end
    })
}

/// The size of a value, and when only its start is shown.
pub(super) fn size_line(size: Option<u64>, cut: bool) -> String {
    match (size, cut) {
        (None, _) => "No value".to_string(),
        (Some(size), false) => byte_size(size),
        (Some(size), true) => format!(
            "{}; the first {} are shown, Copy takes all of it",
            byte_size(size),
            byte_size(MAX_SHOWN_BYTES as u64)
        ),
    }
}

impl ValueViewer {
    /// What the current view shows, and what "Copy" copies.
    fn shown(&self) -> String {
        match &self.value {
            None => String::new(),
            Some(bytes) => view_text(bytes, self.view, self.json.as_deref()),
        }
    }
}

impl Render for ValueViewer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let selected = self
            .views
            .iter()
            .position(|view| *view == self.view)
            .unwrap_or(0);
        let text = self.shown();
        let cut = cut_at(&text);
        let body = match (&self.value, cut) {
            (None, _) => div()
                .italic()
                .text_color(theme.muted_foreground)
                .child("NULL")
                .into_any_element(),
            (Some(_), Some(end)) => div()
                .child(SharedString::from(text[..end].to_string()))
                .into_any_element(),
            (Some(_), None) => div()
                .child(SharedString::from(text.clone()))
                .into_any_element(),
        };
        let size = self.value.as_ref().map(|bytes| bytes.len() as u64);
        v_flex()
            .gap_3()
            .pt_1()
            .when(self.views.len() > 1, |this| {
                this.child(
                    TabBar::new("value-views")
                        .underline()
                        .selected_index(selected)
                        .on_click(cx.listener(|this, index: &usize, _, cx| {
                            if let Some(view) = this.views.get(*index) {
                                this.view = *view;
                                cx.notify();
                            }
                        }))
                        .children(self.views.iter().map(|view| {
                            Tab::new().label(match view {
                                View::Text => "Text",
                                View::Json => "JSON",
                                View::Hex => "Hex",
                            })
                        })),
                )
            })
            // What an integer means in time sits right under it.
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .id("value-text")
                            .test_support()
                            .h(px(360.))
                            .p_2()
                            .overflow_y_scroll()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.muted)
                            .font_family(theme.mono_font_family.clone())
                            .text_sm()
                            .child(body),
                    )
                    .children(hint_line(self.hint.clone(), cx)),
            )
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(size_line(size, cut.is_some()))),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(Button::new("value-copy").outline().label("Copy").on_click(
                                cx.listener(|this, _, _, cx| {
                                    let all = match (&this.value, this.view) {
                                        (Some(bytes), View::Hex) => hex_dump(bytes),
                                        _ => this.shown(),
                                    };
                                    cx.write_to_clipboard(ClipboardItem::new_string(all));
                                }),
                            ))
                            .child(
                                Button::new("value-close")
                                    .primary()
                                    .label("Close")
                                    .on_click(|_, window, cx| window.close_dialog(cx)),
                            ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn views(kind: TypeKind, value: &str) -> Vec<View> {
        views_for(&Column::new("c", "t", kind), Some(value.as_bytes())).0
    }

    /// JSON is offered first for a JSON column and for a text holding an
    /// object or an array; an integer, a string or a boolean opens as text
    /// (an integer is valid JSON, and was shown as such).
    #[test]
    fn the_json_view_is_for_documents_only() {
        use View::{Hex, Json, Text};
        assert_eq!(views(TypeKind::Integer, "1794619425"), [Text, Hex]);
        assert_eq!(views(TypeKind::Text, "\"quoted\""), [Text, Hex]);
        assert_eq!(views(TypeKind::Text, "true"), [Text, Hex]);
        assert_eq!(views(TypeKind::Text, " {\"a\": 1}"), [Json, Text, Hex]);
        assert_eq!(views(TypeKind::Text, "[1, 2]"), [Json, Text, Hex]);
        assert_eq!(views(TypeKind::Json, "42"), [Json, Text, Hex]);
        assert_eq!(views(TypeKind::Text, "{not json"), [Text, Hex]);
        // NULL: the text view says so.
        let null = views_for(&Column::new("c", "t", TypeKind::Integer), None).0;
        assert_eq!(null, [Text]);
    }

    /// K17: what Set (the panel) and Save (the dialog) make of the text.
    #[test]
    fn saving_refuses_broken_json_and_says_where() {
        let shown = "{\n  \"a\": 1\n}";
        // As shown: no change, even though it differs from what is stored.
        assert_eq!(
            saved_value(shown, shown, "{\"a\": 1}", TypeKind::Json),
            Ok(None)
        );
        // Broken JSON is refused with the line and column it breaks at.
        let problem = saved_value("{\n  \"a\": 1,\n}", shown, "", TypeKind::Json).unwrap_err();
        assert!(problem.starts_with("This is not valid JSON: "), "{problem}");
        assert!(problem.contains("line 3 column 1"), "{problem}");
        // Valid JSON is written as typed.
        assert_eq!(
            saved_value("[1, 2]", shown, "", TypeKind::Json),
            Ok(Some(CellValue::Text("[1, 2]".into())))
        );
        // Emptied: NULL, except in a text column, where it is ''.
        assert_eq!(
            saved_value("", shown, "x", TypeKind::Json),
            Ok(Some(CellValue::Null))
        );
        assert_eq!(
            saved_value("", "x", "x", TypeKind::Text),
            Ok(Some(CellValue::Text(String::new())))
        );
        // Text is not checked as JSON.
        assert_eq!(
            saved_value("{", "", "", TypeKind::Text),
            Ok(Some(CellValue::Text("{".into())))
        );
    }

    #[test]
    fn long_text_is_cut_on_a_character_boundary() {
        let text = "é".repeat(MAX_SHOWN_BYTES);
        let end = cut_at(&text).unwrap();
        assert!(end <= MAX_SHOWN_BYTES && text.is_char_boundary(end));
        assert_eq!(cut_at("short"), None);
        assert_eq!(size_line(None, false), "No value");
        assert_eq!(
            size_line(Some(300 * 1024), true),
            "300 kB; the first 256 kB are shown, Copy takes all of it"
        );
    }
}
