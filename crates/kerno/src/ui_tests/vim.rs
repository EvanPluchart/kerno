//! Vim mode in the SQL editor (CLAUDE.md §4 J19; DBeaver #37897, #8219,
//! Beekeeper #317): off, nothing changes; on, the keys are vim's in Normal
//! mode while the arrows, the mouse and ⌘ shortcuts keep working
//! (Beekeeper's vim mode broke the arrows: #1621, #1636).

use super::*;
use kerno_core::vim::Mode;

fn set_vim(cx: &mut TestAppContext, on: bool) {
    cx.update(|cx| AppState::update_settings(cx, |settings| settings.vim_mode = on));
    cx.run_until_parked();
}

/// Replaces the editor's text and puts the cursor at `at`.
fn set_text(app: &Fixture, cx: &mut TestAppContext, text: &str, at: usize) {
    let tab = app.tab(cx);
    app.step(cx, |window, cx| {
        tab.update(cx, |tab, cx| {
            tab.editor().update(cx, |editor, cx| {
                editor.set_value(text, window, cx);
                editor.set_selected_range(at..at, cx);
            })
        })
    });
}

fn cursor(app: &Fixture, cx: &mut TestAppContext) -> usize {
    let tab = app.tab(cx);
    cx.update(|cx| tab.read(cx).editor().read(cx).cursor())
}

fn mode(app: &Fixture, cx: &mut TestAppContext) -> Option<Mode> {
    let tab = app.tab(cx);
    cx.update(|cx| tab.read(cx).vim_mode())
}

/// Types keys one by one, as the keyboard sends them.
fn keys(app: &Fixture, cx: &mut TestAppContext, keys: &str) {
    for key in keys.chars() {
        app.type_text(cx, &key.to_string());
    }
}

/// A query tab with vim mode on, holding `text` with the cursor at `at`.
fn vim_tab(cx: &mut TestAppContext, text: &str, at: usize) -> Fixture {
    let app = open(cx, memory_storage());
    set_vim(cx, true);
    app.press(cx, &["secondary-t"]);
    set_text(&app, cx, text, at);
    app
}

/// Off (the default), nothing of vim is there: `dd` types "dd", and
/// neither its handlers, its status item nor its command field exist.
#[gpui_kit::test]
fn with_vim_mode_off_the_same_keys_type_text(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    assert!(!settings_now(cx).vim_mode);
    app.press(cx, &["secondary-t"]);
    keys(&app, cx, "dd");
    assert_eq!(app.active_query(cx).1, "dd");
    assert_eq!(mode(&app, cx), None);
    assert!(!app.has(cx, "vim-editor"));
    assert!(!app.has(cx, "status-vim-mode"));
    // On then off again: removed as it came.
    set_vim(cx, true);
    assert!(app.has(cx, "vim-editor"));
    set_vim(cx, false);
    assert!(!app.has(cx, "vim-editor"));
    keys(&app, cx, "x");
    assert_eq!(app.active_query(cx).1, "ddx");
}

/// The palette's (and Edit menu's) command turns it on and off.
#[gpui_kit::test]
fn toggle_vim_mode_turns_it_on_and_off(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    dispatch(&app, cx, actions::ToggleVimMode);
    assert!(settings_now(cx).vim_mode);
    assert_eq!(mode(&app, cx), Some(Mode::Normal));
    assert!(app.has(cx, "status-vim-mode"));
    dispatch(&app, cx, actions::ToggleVimMode);
    assert!(!settings_now(cx).vim_mode);
    assert!(!app.has(cx, "status-vim-mode"));
}

#[gpui_kit::test]
fn ddp_swaps_two_lines_through_the_clipboard(cx: &mut TestAppContext) {
    let app = vim_tab(cx, "one\ntwo\nthree", 1);
    assert_eq!(mode(&app, cx), Some(Mode::Normal));
    keys(&app, cx, "ddp");
    assert_eq!(app.active_query(cx).1, "two\none\nthree");
    // The unnamed register is the system clipboard.
    assert_eq!(app.clipboard(cx).as_deref(), Some("one\n"));
    // `u` takes the paste back, then the deletion: one step each.
    keys(&app, cx, "u");
    assert_eq!(app.active_query(cx).1, "two\nthree");
    keys(&app, cx, "u");
    assert_eq!(app.active_query(cx).1, "one\ntwo\nthree");
    // Text copied elsewhere (⌘C) pastes as characters.
    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string("X".into()));
    set_text(&app, cx, "ab", 0);
    keys(&app, cx, "p");
    assert_eq!(app.active_query(cx).1, "aXb");
}

#[gpui_kit::test]
fn ciw_then_text_then_escape_changes_a_word(cx: &mut TestAppContext) {
    let app = vim_tab(cx, "select name from t", 9);
    keys(&app, cx, "ciw");
    assert_eq!(mode(&app, cx), Some(Mode::Insert));
    assert!(app.has(cx, "status-vim-mode"));
    keys(&app, cx, "title");
    app.press(cx, &["escape"]);
    assert_eq!(app.active_query(cx).1, "select title from t");
    assert_eq!(mode(&app, cx), Some(Mode::Normal));
    assert_eq!(cursor(&app, cx), "select titl".len());
    // `.` changes the next word the same way.
    keys(&app, cx, "w.");
    assert_eq!(app.active_query(cx).1, "select title title t");
}

#[gpui_kit::test]
fn a_count_moves_down_three_lines(cx: &mut TestAppContext) {
    let app = vim_tab(cx, "1\n2\n3\n4\n5", 0);
    keys(&app, cx, "3j");
    assert_eq!(cursor(&app, cx), "1\n2\n3\n".len());
    // Normal mode typed nothing.
    assert_eq!(app.active_query(cx).1, "1\n2\n3\n4\n5");
}

#[gpui_kit::test]
fn slash_searches_and_n_goes_to_the_next_match(cx: &mut TestAppContext) {
    let app = vim_tab(cx, "select id from t where id = 1", 0);
    keys(&app, cx, "/");
    assert!(app.has(cx, "vim-command-line"));
    keys(&app, cx, "id");
    app.press(cx, &["enter"]);
    assert!(!app.has(cx, "vim-command-line"));
    assert_eq!(cursor(&app, cx), 7);
    // The keyboard is back in the editor.
    keys(&app, cx, "n");
    assert_eq!(cursor(&app, cx), "select id from t where ".len());
    assert_eq!(app.active_query(cx).1, "select id from t where id = 1");
    // Escape closes the field without searching.
    keys(&app, cx, "/x");
    app.press(cx, &["escape"]);
    assert!(!app.has(cx, "vim-command-line"));
    assert_eq!(cursor(&app, cx), "select id from t where ".len());
}

#[gpui_kit::test]
fn colon_w_saves_the_file(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vim.sql");
    std::fs::write(&path, "select 1;").unwrap();
    let app = open(cx, memory_storage());
    set_vim(cx, true);
    let opening = path.clone();
    app.step(cx, |window, cx| {
        app.workspace.update(cx, |workspace, cx| {
            workspace.open_sql_file_from(opening, window, cx)
        })
    });
    app.wait_until(cx, "the file", |app, cx| app.titles(cx) == ["vim.sql"]);
    keys(&app, cx, "$x");
    assert_eq!(app.active_query(cx).1, "select 1");
    keys(&app, cx, ":w");
    app.press(cx, &["enter"]);
    app.wait_until(cx, "the file written", |_, _| {
        std::fs::read_to_string(&path).unwrap() == "select 1"
    });
    let tab = app.tab(cx);
    app.wait_until(cx, "the tab saved", |_, cx| {
        !cx.update(|cx| tab.read(cx).unsaved())
    });
}

#[gpui_kit::test]
fn colon_q_closes_the_tab(cx: &mut TestAppContext) {
    let app = vim_tab(cx, "select 1", 0);
    app.press(cx, &["secondary-t"]);
    assert_eq!(app.titles(cx).len(), 2);
    keys(&app, cx, ":q");
    app.press(cx, &["enter"]);
    assert_eq!(app.titles(cx).len(), 1);
    // An unknown command says so and closes nothing.
    keys(&app, cx, ":nope");
    app.press(cx, &["enter"]);
    assert_eq!(app.titles(cx).len(), 1);
}

/// Beekeeper #1621, #1636: the arrows move the cursor in Normal mode and in
/// Insert mode, and vim's next motion starts from where they left it.
#[gpui_kit::test]
fn arrows_move_the_cursor_in_normal_and_insert_mode(cx: &mut TestAppContext) {
    let app = vim_tab(cx, "abc\ndef\nghi", 0);
    app.press(cx, &["right"]);
    assert_eq!(cursor(&app, cx), 1);
    app.press(cx, &["down"]);
    assert_eq!(cursor(&app, cx), 5);
    keys(&app, cx, "l");
    assert_eq!(cursor(&app, cx), 6);
    app.press(cx, &["home"]);
    assert_eq!(cursor(&app, cx), 4);
    keys(&app, cx, "i");
    assert_eq!(mode(&app, cx), Some(Mode::Insert));
    app.press(cx, &["up"]);
    app.press(cx, &["right"]);
    assert_eq!(cursor(&app, cx), 1);
    keys(&app, cx, "X");
    assert_eq!(app.active_query(cx).1, "aXbc\ndef\nghi");
    app.press(cx, &["escape"]);
    assert_eq!(mode(&app, cx), Some(Mode::Normal));
    // Backspace and Enter do not edit in Normal mode.
    app.press(cx, &["backspace", "enter"]);
    assert_eq!(app.active_query(cx).1, "aXbc\ndef\nghi");
}

/// ⌘↵ still runs the statement at the cursor in Normal mode.
#[gpui_kit::test]
fn secondary_enter_still_runs_in_normal_mode(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    set_vim(cx, true);
    app.press(cx, &["secondary-t"]);
    set_text(&app, cx, "select 41 + 1 as kerno_ui_vim", 0);
    assert_eq!(mode(&app, cx), Some(Mode::Normal));
    app.press(cx, &["secondary-enter"]);
    app.wait_until(cx, "the result", |app, cx| !app.outcomes(cx).is_empty());
    let (columns, rows) = app.result(cx, 0, 1);
    assert_eq!(columns, ["kerno_ui_vim"]);
    assert_eq!(rows, [[Some("42".to_string())]]);
    assert_eq!(app.active_query(cx).1, "select 41 + 1 as kerno_ui_vim");
}

/// DBeaver #2316 (J1) in Insert mode: Tab still accepts the highlighted
/// completion; Normal mode's `i` got there.
#[gpui_kit::test]
fn tab_still_accepts_a_completion_in_insert_mode(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    set_vim(cx, true);
    new_tab_with_catalog(&app, cx);
    keys(&app, cx, "i");
    assert_eq!(mode(&app, cx), Some(Mode::Insert));
    app.type_text(cx, "sel");
    let (open, items) = completion_menu(&app, cx);
    assert!(open, "{items:?}");
    app.press(cx, &["tab"]);
    assert_eq!(app.active_query(cx).1, "SELECT");
    assert_eq!(mode(&app, cx), Some(Mode::Insert));
}

fn block(app: &Fixture, cx: &mut TestAppContext) -> Option<std::ops::Range<usize>> {
    let tab = app.tab(cx);
    cx.update(|cx| tab.read(cx).vim_block())
}

/// `V` selects lines, `d` deletes them; the block follows Normal mode's
/// cursor and goes away in Insert mode.
#[gpui_kit::test]
fn visual_line_deletes_lines_and_the_block_marks_the_cursor(cx: &mut TestAppContext) {
    let app = vim_tab(cx, "a\nbb\nc\nd", 2);
    keys(&app, cx, "l");
    assert_eq!(block(&app, cx), Some(3..4));
    keys(&app, cx, "Vj");
    assert_eq!(mode(&app, cx), Some(Mode::VisualLine));
    let tab = app.tab(cx);
    let selected = cx.update(|cx| tab.read(cx).editor().read(cx).selected_range());
    assert_eq!(selected, 2..7);
    assert_eq!(block(&app, cx), None);
    keys(&app, cx, "d");
    assert_eq!(app.active_query(cx).1, "a\nd");
    assert_eq!(app.clipboard(cx).as_deref(), Some("bb\nc\n"));
    assert_eq!(block(&app, cx), Some(2..3));
    keys(&app, cx, "a");
    assert_eq!(block(&app, cx), None);
}

/// `:wq` closes the tab once the file is written; `:q!` closes a tab whose
/// text was not saved without asking.
#[gpui_kit::test]
fn colon_wq_writes_then_closes_and_colon_q_bang_forgets_the_text(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let app = open(cx, memory_storage());
    set_vim(cx, true);
    let open_file = |app: &Fixture, cx: &mut TestAppContext, name: &str| {
        let path = dir.path().join(name);
        std::fs::write(&path, "select 1;").unwrap();
        let opening = path.clone();
        app.step(cx, |window, cx| {
            app.workspace.update(cx, |workspace, cx| {
                workspace.open_sql_file_from(opening, window, cx)
            })
        });
        let name = name.to_string();
        app.wait_until(cx, "the file", |app, cx| app.titles(cx).contains(&name));
        path
    };
    let written = open_file(&app, cx, "wq.sql");
    keys(&app, cx, "$x:wq");
    app.press(cx, &["enter"]);
    app.wait_until(cx, "the tab closed", |app, cx| app.titles(cx).is_empty());
    assert_eq!(std::fs::read_to_string(&written).unwrap(), "select 1");

    let kept = open_file(&app, cx, "kept.sql");
    keys(&app, cx, "x");
    let tab = app.tab(cx);
    assert!(cx.update(|cx| tab.read(cx).unsaved()));
    keys(&app, cx, ":q!");
    app.press(cx, &["enter"]);
    app.wait_until(cx, "the tab closed", |app, cx| app.titles(cx).is_empty());
    assert_eq!(std::fs::read_to_string(&kept).unwrap(), "select 1;");
}
