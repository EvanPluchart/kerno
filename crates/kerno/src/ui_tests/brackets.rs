//! The bracket beside the cursor and its match are marked in the SQL
//! editor (CLAUDE.md §4 C2 "bracket matching").

use super::*;

/// The ranges marked in the active query tab's editor.
fn marked(app: &Fixture, cx: &mut TestAppContext) -> Vec<std::ops::Range<usize>> {
    let tab = app.tab(cx);
    cx.update(|cx| tab.read(cx).marked_brackets(cx))
}

/// Replaces the editor's text and puts the cursor at `column` of its
/// first line.
fn set_text(app: &Fixture, cx: &mut TestAppContext, text: &str, column: u32) {
    let tab = app.tab(cx);
    app.step(cx, |window, cx| {
        tab.update(cx, |tab, cx| {
            tab.editor().update(cx, |editor, cx| {
                editor.set_value(text, window, cx);
                editor.set_cursor_position(
                    gpui_kit::component::input::Position::new(0, column),
                    window,
                    cx,
                );
            })
        })
    });
}

#[gpui_kit::test]
fn the_bracket_at_the_cursor_and_its_match_are_marked(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    for character in "select count(".chars() {
        app.type_text(cx, &character.to_string());
    }
    assert_eq!(app.active_query(cx).1, "select count()");
    // Between the pair, the bracket before the cursor and its match.
    assert_eq!(marked(&app, cx), vec![12..13, 13..14]);
    // Just before the closer, then just after it.
    app.type_text(cx, "*");
    assert_eq!(app.active_query(cx).1, "select count(*)");
    assert_eq!(marked(&app, cx), vec![14..15, 12..13]);
    app.press(cx, &["right"]);
    assert_eq!(marked(&app, cx), vec![14..15, 12..13]);
    // Elsewhere, nothing.
    app.type_text(cx, " from t");
    assert_eq!(marked(&app, cx), Vec::<std::ops::Range<usize>>::new());
    // The bracket in a string does not count.
    set_text(&app, cx, "select (')' || x), (1", 8);
    assert_eq!(marked(&app, cx), vec![7..8, 16..17]);
    set_text(&app, cx, "select (')' || x), (1", 10);
    assert_eq!(marked(&app, cx), Vec::<std::ops::Range<usize>>::new());
    // An unmatched bracket is not marked.
    set_text(&app, cx, "select (')' || x), (1", 20);
    assert_eq!(marked(&app, cx), Vec::<std::ops::Range<usize>>::new());
}
