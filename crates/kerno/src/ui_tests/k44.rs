//! K44 (docs/plans/M12.md): ⌘← (Ctrl+Left elsewhere) collapses the schema
//! tree while the tree has the keyboard, as VS Code's lists do; the text
//! fields keep it for the start of the line.

use super::shortcuts::{filter, keymap_file, open_shortcuts, text_of, winner};
use super::*;

fn expanded(app: &Fixture, cx: &mut TestAppContext) -> bool {
    app.tree(cx).iter().any(|row| row.starts_with(' '))
}

/// In the tree, ⌘← collapses it all; in the tree's filter and in a query
/// editor it moves the caret to the start of the line and collapses nothing.
#[gpui_kit::test]
fn cmd_left_collapses_the_tree_only_from_the_tree(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.open_connection_row(cx, 0);
    app.wait_until(cx, "the expanded tree", expanded);
    let tree = cx.update(|cx| app.workspace.read(cx).navigator(cx));

    // The tree's filter.
    app.dispatch(cx, actions::FocusNavigatorFilter);
    assert!(app.step(cx, |window, cx| tree.read(cx).filter_focused(window, cx)));
    app.type_text(cx, "zz");
    app.press(cx, &["secondary-left"]);
    app.type_text(cx, "y");
    assert_eq!(cx.update(|cx| tree.read(cx).filter_text(cx)), "yzz");
    app.press(cx, &["secondary-a", "backspace"]);
    app.wait_until(cx, "the tree unfiltered", expanded);

    // A query editor.
    app.press(cx, &["secondary-t"]);
    app.type_text(cx, "select 1");
    app.press(cx, &["secondary-left"]);
    app.type_text(cx, "x");
    assert_eq!(app.active_query(cx).1, "xselect 1");
    assert!(expanded(&app, cx), "{:#?}", app.tree(cx));

    // The tree itself.
    app.click(cx, ("connection", 0usize));
    assert_eq!(
        winner(&app, cx, "secondary-left").as_deref(),
        Some("kerno::CollapseSchemaTree")
    );
    app.press(cx, &["secondary-left"]);
    let rows = app.tree(cx);
    assert!(rows.iter().all(|row| !row.starts_with(' ')), "{rows:?}");
}

/// Settings ▸ Shortcuts shows ⌘← on the command; a key of the text fields
/// is taken for a command of the tree and still refused for one that works
/// everywhere; the keymap file keeps the user's other overrides.
#[gpui_kit::test]
fn a_key_of_the_text_fields_is_taken_for_a_command_of_the_tree(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    open_shortcuts(&app, cx);
    filter(&app, cx, "Collapse Schema Tree");
    // Its default, ⌘← (Ctrl+Left elsewhere), with nothing changed.
    assert!(app.has(cx, "shortcut-collapse_schema_tree-key-0"));
    assert!(!app.has(cx, "shortcut-collapse_schema_tree-changed"));
    // A command that works everywhere: refused, with the same sentence.
    filter(&app, cx, "About Kerno");
    app.click(cx, "shortcut-about-add");
    app.press(cx, &["secondary-right"]);
    let refused = text_of(&app, cx, "shortcut-recording");
    assert!(
        refused.contains("text fields' MoveEnd: a command that works everywhere would take it"),
        "{refused}"
    );
    app.press(cx, &["f6"]);
    assert!(app.has(cx, "shortcut-about-changed"));
    // The tree's command takes it.
    filter(&app, cx, "Collapse Schema Tree");
    app.click(cx, "shortcut-collapse_schema_tree-add");
    app.press(cx, &["secondary-right"]);
    assert!(!app.has(cx, "shortcut-recording"));
    assert!(app.has(cx, "shortcut-collapse_schema_tree-changed"));
    let file = keymap_file(cx);
    app.wait_until(cx, "the keymap file", |_, _| {
        std::fs::read_to_string(&file).is_ok_and(|text| {
            text.contains(r#""about": "f6""#) && text.contains(r#""secondary-right""#)
        })
    });
    // Given back its default.
    app.click(cx, "shortcut-collapse_schema_tree-reset");
    assert!(!app.has(cx, "shortcut-collapse_schema_tree-changed"));
}
