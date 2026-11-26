//! K81 (docs/plans/M12.md, D-147): the value viewers are drawn with the
//! theme's colours. The viewer dialog and the value editor highlight JSON as
//! the SQL editor does, the hex view marks its offsets and its gutter, Copy
//! still copies the value, Escape still closes, and a text too long to wrap
//! says so.

use std::rc::Rc;

use kerno_core::format::{hex_dump, pretty_json};
use kerno_core::{Column, TypeKind};

use super::tool_wizards::paints;
use super::*;
use crate::query::viewer::{self, opened};

/// The neutral document of the task (never a user's data).
const DOC: &str = r#"{"price": 600, "ordered_at": "2026-06-08 16:17:04.768895+00", "customer": "Ada", "coupon": null}"#;

fn view(app: &Fixture, cx: &mut TestAppContext, column: Column, value: &[u8]) {
    app.step(cx, |window, cx| {
        viewer::open(&column, Some(value), window, cx)
    });
}

fn shown(cx: &mut TestAppContext) -> opened::Shown {
    cx.update(|cx| opened::viewer(cx)).expect("a viewer")
}

/// The colours the hex view's parts take in the theme on screen: the
/// muted text, and the editor's line numbers for the unprintable bytes.
fn hex_colors(cx: &mut TestAppContext) -> (gpui_kit::Hsla, gpui_kit::Hsla) {
    cx.update(|cx| {
        let theme = cx.theme();
        let dim = theme.highlight_theme.style.editor_line_number.unwrap();
        (theme.muted_foreground, dim)
    })
}

/// The JSON view is the kit's editor highlighting JSON; the Text view the
/// value as stored; the Hex view the dump with its parts marked. Copy copies
/// each view's whole text, and Escape closes the dialog.
#[gpui_kit::test]
fn the_viewer_highlights_json_and_marks_the_hex_dump(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    view(
        &app,
        cx,
        Column::new("doc", "jsonb", TypeKind::Json),
        DOC.as_bytes(),
    );
    assert!(app.has(cx, "value-text"));
    let json = shown(cx);
    assert_eq!(json.language, "json");
    assert_eq!(Some(json.text), pretty_json(DOC));
    assert_eq!(json.marked, 0);
    app.click(cx, "value-copy");
    assert_eq!(app.clipboard(cx), pretty_json(DOC));

    app.click_label(cx, "Text");
    let text = shown(cx);
    assert_eq!((text.language.as_str(), text.text.as_str()), ("text", DOC));
    app.click(cx, "value-copy");
    assert_eq!(app.clipboard(cx).as_deref(), Some(DOC));

    app.click_label(cx, "Hex");
    let hex = shown(cx);
    assert_eq!(hex.language, "text");
    assert_eq!(hex.text, hex_dump(DOC.as_bytes()));
    // An offset and a gutter a row: nothing of DOC is unprintable.
    assert_eq!(hex.marked, DOC.len().div_ceil(16) * 2);
    assert_eq!(hex.painted, Some(hex_colors(cx)));
    app.click(cx, "value-copy");
    assert_eq!(app.clipboard(cx), Some(hex_dump(DOC.as_bytes())));

    app.press(cx, &["escape"]);
    assert!(!app.has(cx, "value-text"));
}

/// The hex view's marks take the new theme's colours while the dialog is
/// open (the appearance can follow the system's at any time).
#[gpui_kit::test]
fn the_hex_marks_follow_a_change_of_theme(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    let bytes: Vec<u8> = (0..64u8).collect();
    view(
        &app,
        cx,
        Column::new("raw", "bytea", TypeKind::Binary),
        &bytes,
    );
    let light = hex_colors(cx);
    let marked = shown(cx);
    assert_eq!(marked.painted, Some(light));
    // Control characters: unprintable runs beside the offsets and gutters.
    assert!(marked.marked > 8, "{}", marked.marked);
    app.step(cx, |window, cx| {
        theme::apply(Appearance::Dark, Some(window), cx)
    });
    let dark = hex_colors(cx);
    assert_ne!(light, dark);
    assert_eq!(shown(cx).painted, Some(dark));
}

/// A text too long to wrap says so; only its start is in the editor, and
/// Copy still takes all of it.
#[gpui_kit::test]
fn a_long_text_is_shown_unwrapped_and_copied_whole(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    let long = "word ".repeat(60_000);
    view(
        &app,
        cx,
        Column::new("body", "text", TypeKind::Text),
        long.as_bytes(),
    );
    assert_eq!(shown(cx).text.len(), 256 * 1024);
    assert!(paints(
        &app,
        cx,
        "the first 256 kB are shown, Copy takes all of it; long lines are not wrapped at this size"
    ));
    app.click(cx, "value-copy");
    assert_eq!(
        app.clipboard(cx).map(|copied| copied.len()),
        Some(long.len())
    );
    app.click(cx, "value-close");
    // A short one wraps, and says nothing of it.
    view(
        &app,
        cx,
        Column::new("body", "text", TypeKind::Text),
        b"short",
    );
    assert!(!paints(&app, cx, "not wrapped"));
}

/// The value editor (Shift+Enter) highlights a JSON column's value as the
/// viewer does, and a text column's as text; typing and Save work as
/// before, and Escape closes it.
#[gpui_kit::test]
fn the_value_editor_highlights_json(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    let saved = Rc::new(std::cell::RefCell::new(Vec::new()));
    let edit = |app: &Fixture, cx: &mut TestAppContext, column: Column, value: &[u8]| {
        let saved = saved.clone();
        let workspace = app.workspace.clone();
        app.step(cx, |window, cx| {
            let back = workspace.focus_handle(cx);
            let save = viewer::Save {
                nullable: true,
                apply: Rc::new(move |value, _, _| saved.borrow_mut().push(value)),
            };
            viewer::edit(&column, Some(value), Some(save), back, window, cx)
        });
    };
    edit(
        &app,
        cx,
        Column::new("doc", "jsonb", TypeKind::Json),
        DOC.as_bytes(),
    );
    assert!(app.has(cx, "value-editor"));
    let (language, text) = cx.update(|cx| opened::editor(cx)).unwrap();
    assert_eq!(language, "json");
    assert_eq!(Some(text), pretty_json(DOC));
    app.press(cx, &["secondary-a", "backspace"]);
    app.type_text(cx, "[1, 2]");
    app.click(cx, "value-save");
    assert!(!app.has(cx, "value-editor"));
    assert_eq!(
        *saved.borrow(),
        [kerno_core::sql::CellValue::Text("[1, 2]".into())]
    );

    edit(
        &app,
        cx,
        Column::new("name", "text", TypeKind::Text),
        b"Ada",
    );
    let (language, text) = cx.update(|cx| opened::editor(cx)).unwrap();
    assert_eq!((language.as_str(), text.as_str()), ("text", "Ada"));
    app.press(cx, &["escape"]);
    assert!(!app.has(cx, "value-editor"));
}
