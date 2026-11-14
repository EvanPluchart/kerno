//! The SQL editor of v1.1 (CLAUDE.md §4 J1 and J7): Tab accepts a
//! completion (DBeaver #2316), occurrences highlighted and selected
//! together, folding (#17240), zoom (#17817), formatter options and format
//! on save (#267, #22488).

use super::*;
use gpui_kit::{InputEvent as _, ScrollDelta};
use kerno_core::sql::{Comma, FormatOptions, Indent, KeywordCase, LexRules, format_with};

/// Replaces the editor's text and selects `range` of it (a cursor when
/// empty).
fn set_text(app: &Fixture, cx: &mut TestAppContext, text: &str, range: std::ops::Range<usize>) {
    let tab = app.tab(cx);
    app.step(cx, |window, cx| {
        tab.update(cx, |tab, cx| {
            tab.editor().update(cx, |editor, cx| {
                editor.set_value(text, window, cx);
                editor.set_selected_range(range, cx);
            })
        })
    });
}

/// Turns the wheel over the centre of the element `id`, with Cmd (Ctrl
/// elsewhere) held when `secondary`.
fn wheel(
    app: &Fixture,
    cx: &mut TestAppContext,
    id: &'static str,
    delta: ScrollDelta,
    secondary: bool,
) {
    app.step(cx, |window, cx| {
        let position = window.find(id).bounds().center();
        let modifiers = if secondary {
            gpui_kit::Modifiers::secondary_key(cx)
        } else {
            gpui_kit::Modifiers::default()
        };
        window.dispatch_event(
            gpui_kit::MouseMoveEvent {
                position,
                pressed_button: None,
                modifiers,
            }
            .to_platform_input(),
            cx,
        );
        window.dispatch_event(
            gpui_kit::ScrollEvent {
                position,
                delta,
                modifiers,
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
    })
}

fn occurrences(app: &Fixture, cx: &mut TestAppContext) -> Vec<std::ops::Range<usize>> {
    let tab = app.tab(cx);
    cx.update(|cx| tab.read(cx).marked_occurrences(cx))
}

/// DBeaver #2316: with the list open, Tab accepts the highlighted item as
/// Enter does; with the setting off, Tab indents.
#[gpui_kit::test]
fn tab_accepts_the_highlighted_completion_unless_the_setting_is_off(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    new_tab_with_catalog(&app, cx);
    assert!(settings_now(cx).tab_accepts_completion);
    // A keyword.
    app.type_text(cx, "sel");
    let (open, items) = completion_menu(&app, cx);
    assert!(open, "{items:?}");
    app.press(cx, &["tab"]);
    assert_eq!(app.active_query(cx).1, "SELECT");
    assert!(!completion_menu(&app, cx).0);
    // A column after its table's name.
    app.type_text(cx, " * from shop.orders where orders.custo");
    let (open, items) = completion_menu(&app, cx);
    assert!(open, "{items:?}");
    assert_eq!(items[0], "customer_id");
    app.press(cx, &["tab"]);
    assert_eq!(
        app.active_query(cx).1,
        "SELECT * from shop.orders where orders.customer_id"
    );
    // Enter still accepts.
    app.type_text(cx, " = 1 and orders.stat");
    assert!(completion_menu(&app, cx).0);
    app.press(cx, &["enter"]);
    assert!(app.active_query(cx).1.ends_with("and orders.status"));
    // Off: Tab indents and accepts nothing.
    cx.update(|cx| {
        AppState::update_settings(cx, |settings| settings.tab_accepts_completion = false)
    });
    app.type_text(cx, " or orders.stat");
    assert!(completion_menu(&app, cx).0);
    app.press(cx, &["tab"]);
    let text = app.active_query(cx).1;
    assert!(
        text.ends_with(' ') && text.trim_end().ends_with("or orders.stat"),
        "{text:?}"
    );
}

/// Every other occurrence of the word at the cursor, or of a one-line
/// selection, is highlighted; ⌘⇧L selects them all and typing changes
/// them together.
#[gpui_kit::test]
fn occurrences_are_highlighted_then_selected_and_edited_together(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    let text = "select id, o.id from orders o where ID > 1 and idx = 'id'";
    let at = |needle: &str, nth: usize| text.match_indices(needle).nth(nth).unwrap().0;
    // The cursor in the second `id`: the others, whole words, any case.
    set_text(&app, cx, text, at("id", 1) + 1..at("id", 1) + 1);
    let first = at("id", 0);
    let upper = text.find("ID").unwrap();
    let quoted = text.rfind("id").unwrap();
    assert_eq!(
        occurrences(&app, cx),
        vec![first..first + 2, upper..upper + 2, quoted..quoted + 2]
    );
    // A word found once: nothing.
    set_text(&app, cx, text, at("orders", 0)..at("orders", 0));
    assert_eq!(occurrences(&app, cx), Vec::<std::ops::Range<usize>>::new());
    // A selection on one line: anywhere, inside other words too.
    set_text(&app, cx, text, first..first + 2);
    let idx = text.find("idx").unwrap();
    assert_eq!(
        occurrences(&app, cx),
        vec![
            at("id", 1)..at("id", 1) + 2,
            upper..upper + 2,
            idx..idx + 2,
            quoted..quoted + 2
        ]
    );

    // Select all occurrences of the word at the cursor, then type.
    set_text(&app, cx, text, first..first);
    app.press(cx, &["secondary-shift-l"]);
    app.type_text(cx, "key");
    assert_eq!(
        app.active_query(cx).1,
        "select key, o.key from orders o where key > 1 and idx = 'key'"
    );
    // The same from the editor's menu entry.
    set_text(&app, cx, "a + a + b", 0..0);
    app.dispatch(cx, actions::SelectAllOccurrences);
    app.type_text(cx, "c");
    assert_eq!(app.active_query(cx).1, "c + c + b");
}

/// The kit folds what tree-sitter sees as a block of three lines or more,
/// from the gutter. A folded region still runs and saves in full, and
/// "Unfold All" opens it (#17240).
#[gpui_kit::test]
fn a_folded_region_still_runs_and_saves_in_full(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.press(cx, &["secondary-t"]);
    let tab = app.tab(cx);
    let script = "select g,\n  g * 2 as twice\nfrom (\n  select g\n  from generate_series(1, 3) g\n) s\norder by g;\n\ndo $$\nbegin\n  perform 1;\nend\n$$;";
    set_text(&app, cx, script, 0..0);
    let candidates = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let editor = tab.read(cx).editor().read(cx);
            let map = editor.display_map();
            (0..script.lines().count())
                .filter(|line| map.is_fold_candidate(*line))
                .collect::<Vec<_>>()
        })
    };
    // The statement (line 0), its subquery (line 2) and the `BEGIN … END`
    // of the `do` body (line 9) fold.
    assert_eq!(candidates(cx), [0, 2, 9]);
    let folded = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let editor = tab.read(cx).editor().read(cx);
            editor
                .display_map()
                .folded_ranges()
                .iter()
                .map(|fold| fold.start_line)
                .collect::<Vec<_>>()
        })
    };
    // A click on the gutter's fold mark of `line` (the cursor's line, where
    // the mark is drawn), left of its text.
    let fold = |cx: &mut TestAppContext, line: usize| {
        let start: usize = script.lines().take(line).map(|text| text.len() + 1).sum();
        set_text(&app, cx, script, start..start);
        let text_start = cx
            .update(|cx| {
                tab.read(cx)
                    .editor()
                    .read(cx)
                    .range_to_bounds(&(start..start + 1))
            })
            .expect("the line is drawn");
        let mark = gpui_kit::point(text_start.left() - px(13.), text_start.center().y);
        app.step(cx, |window, cx| window.drag(mark, mark, cx));
    };
    fold(cx, 0);
    assert_eq!(folded(cx), [0]);
    // Running the script runs every statement, the folded lines included.
    app.press(cx, &["secondary-shift-enter"]);
    app.wait_until(cx, "the script", |app, cx| app.outcomes(cx).len() == 2);
    assert_eq!(app.outcomes(cx)[0], "3 rows");
    let (_, rows) = app.result(cx, 0, 10);
    let cell = |text: &str| Some(text.to_string());
    assert_eq!(
        rows,
        [
            [cell("1"), cell("2")],
            [cell("2"), cell("4")],
            [cell("3"), cell("6")]
        ]
    );
    // Saving writes it all.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("folded.sql");
    let workspace = app.workspace.clone();
    app.step(cx, |window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.save_active_tab_to(path.clone(), window, cx)
        })
    });
    app.wait_until(cx, "the file", |_, _| {
        std::fs::read_to_string(&path).is_ok_and(|text| text == script)
    });
    assert_eq!(folded(cx), [0]);
    // Unfold all, from the palette's command.
    app.dispatch(cx, actions::UnfoldAll);
    assert_eq!(folded(cx), Vec::<usize>::new());
    // The subquery, then the `BEGIN … END`: folded, the text that runs and
    // saves is all there.
    for line in [2, 9] {
        fold(cx, line);
        assert_eq!(folded(cx), [line]);
        assert_eq!(cx.update(|cx| tab.read(cx).sql(cx)), script);
        app.dispatch(cx, actions::UnfoldAll);
        assert_eq!(folded(cx), Vec::<usize>::new());
    }

    // Cmd with the wheel over the result grid zooms too; without Cmd it
    // only scrolls (#17817).
    let sizes = |cx: &mut TestAppContext| {
        let settings = settings_now(cx);
        (settings.editor_font_size, settings.grid_font_size)
    };
    let usual = sizes(cx);
    wheel(
        &app,
        cx,
        "results",
        ScrollDelta::Lines(gpui_kit::point(0., 3.)),
        true,
    );
    assert_eq!(sizes(cx), (usual.0 + 1., usual.1 + 1.));
    wheel(
        &app,
        cx,
        "results",
        ScrollDelta::Lines(gpui_kit::point(0., 3.)),
        false,
    );
    assert_eq!(sizes(cx), (usual.0 + 1., usual.1 + 1.));
}

/// ⌘+ / ⌘- / ⌘0 in the editor and ⌘-wheel over it change the editor's
/// and the grids' text sizes together (#17817).
#[gpui_kit::test]
fn zoom_keys_and_the_wheel_size_the_editor_and_the_grids_together(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    let usual = Settings::default();
    let sizes = |cx: &mut TestAppContext| {
        let settings = settings_now(cx);
        (settings.editor_font_size, settings.grid_font_size)
    };
    app.press(cx, &["secondary-=", "secondary-="]);
    assert_eq!(
        sizes(cx),
        (usual.editor_font_size + 2., usual.grid_font_size + 2.)
    );
    // The editor takes it at once.
    assert_eq!(
        cx.update(|cx| cx.theme().mono_font_size),
        px(usual.editor_font_size + 2.)
    );
    app.press(cx, &["secondary--"]);
    assert_eq!(
        sizes(cx),
        (usual.editor_font_size + 1., usual.grid_font_size + 1.)
    );
    app.press(cx, &["secondary-0"]);
    assert_eq!(sizes(cx), (usual.editor_font_size, usual.grid_font_size));
    // Each stays in its range.
    for _ in 0..20 {
        app.press(cx, &["secondary--"]);
    }
    assert_eq!(sizes(cx), (11., 11.));
    app.press(cx, &["secondary-0"]);

    // The wheel with Cmd over the editor: one step per notch, up is larger;
    // a trackpad's pixels add up. Without Cmd it scrolls, as before.
    wheel(
        &app,
        cx,
        "query-editor",
        ScrollDelta::Lines(gpui_kit::point(0., 3.)),
        true,
    );
    assert_eq!(
        sizes(cx),
        (usual.editor_font_size + 1., usual.grid_font_size + 1.)
    );
    wheel(
        &app,
        cx,
        "query-editor",
        ScrollDelta::Lines(gpui_kit::point(0., -3.)),
        true,
    );
    assert_eq!(sizes(cx), (usual.editor_font_size, usual.grid_font_size));
    wheel(
        &app,
        cx,
        "query-editor",
        ScrollDelta::Pixels(gpui_kit::point(px(0.), px(25.))),
        true,
    );
    assert_eq!(sizes(cx), (usual.editor_font_size, usual.grid_font_size));
    wheel(
        &app,
        cx,
        "query-editor",
        ScrollDelta::Pixels(gpui_kit::point(px(0.), px(25.))),
        true,
    );
    assert_eq!(
        sizes(cx),
        (usual.editor_font_size + 1., usual.grid_font_size + 1.)
    );
    // Without Cmd the editor scrolls, and the size stays.
    let long: String = (0..200).map(|n| format!("select {n};\n")).collect();
    set_text(&app, cx, &long, 0..0);
    let tab = app.tab(cx);
    let first_row = |cx: &mut TestAppContext| {
        cx.update(|cx| tab.read(cx).editor().read(cx).visible_row_range())
    };
    assert_eq!(first_row(cx).map(|rows| rows.start), Some(0));
    wheel(
        &app,
        cx,
        "query-editor",
        ScrollDelta::Lines(gpui_kit::point(0., -10.)),
        false,
    );
    assert!(
        first_row(cx).is_some_and(|rows| rows.start > 0),
        "{:?}",
        first_row(cx)
    );
    assert_eq!(
        sizes(cx),
        (usual.editor_font_size + 1., usual.grid_font_size + 1.)
    );
    // Saved like any setting.
    let wanted = usual.editor_font_size + 1.;
    app.wait_until(cx, "the stored size", |_, cx| {
        cx.update(|cx| Settings::load(&AppState::get(cx).storage).editor_font_size == wanted)
    });
}

/// Settings ▸ Editor ▸ Formatter: each option applies to Format SQL at
/// once (#267, #22488); the sample beside them is `format_with` itself.
#[gpui_kit::test]
fn the_formatter_options_apply_to_format_sql(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    app.type_text(cx, "select a, b from t where x = 1");
    app.press(cx, &["secondary-,"]);
    app.click(cx, ("settings-section", 2usize));
    assert!(app.has(cx, "formatter-sample"));
    app.click(cx, "format-case-1");
    app.click(cx, "format-indent-0");
    app.click(cx, "format-comma-1");
    let options = FormatOptions {
        keyword_case: KeywordCase::Lower,
        indent: Indent::Spaces(2),
        comma: Comma::Leading,
        line_width: 0,
    };
    assert_eq!(settings_now(cx).format, options);
    // Back to the editor: Format SQL lays out with them.
    app.press(cx, &["escape"]);
    let tab = app.tab(cx);
    app.step(cx, |window, cx| {
        tab.update(cx, |tab, cx| tab.format(window, cx))
    });
    assert_eq!(
        app.active_query(cx).1,
        "select\n  a\n  , b\nfrom t\nwhere x = 1\n"
    );
    assert_eq!(
        app.active_query(cx).1,
        format_with(
            "select a, b from t where x = 1",
            LexRules::POSTGRES,
            &options
        )
    );
}

/// Format on save (off by default) lays a `.sql` file out before it is
/// written, editor and file alike; a script that does not lay out cleanly
/// is saved as it is.
#[gpui_kit::test]
fn format_on_save_lays_out_a_sql_file_that_formats_cleanly(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    let dir = tempfile::tempdir().unwrap();
    let workspace = app.workspace.clone();
    let save = |cx: &mut TestAppContext, path: &Path| {
        let path = path.to_path_buf();
        app.step(cx, |window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.save_active_tab_to(path, window, cx)
            })
        });
    };
    let written = |cx: &mut TestAppContext, path: &Path, text: &str| {
        let path = path.to_path_buf();
        let text = text.to_string();
        app.wait_until(cx, "the file", move |_, _| {
            std::fs::read_to_string(&path).is_ok_and(|on_disk| on_disk == text)
        });
    };
    let messy = "select a, b from t";
    let laid_out = "SELECT\n    a,\n    b\nFROM t\n";
    // Off: saved as typed.
    app.type_text(cx, messy);
    let off = dir.path().join("off.sql");
    save(cx, &off);
    written(cx, &off, messy);
    // On: the editor and the file are laid out.
    cx.update(|cx| AppState::update_settings(cx, |settings| settings.format_on_save = true));
    let on = dir.path().join("on.sql");
    save(cx, &on);
    written(cx, &on, laid_out);
    assert_eq!(app.active_query(cx).1, laid_out);
    // Not a `.sql` file: as it is.
    set_text(&app, cx, messy, 0..0);
    let notes = dir.path().join("notes.txt");
    save(cx, &notes);
    written(cx, &notes, messy);
    // A string left open: as it is.
    let open_string = "select 'abc";
    set_text(&app, cx, open_string, 0..0);
    let unfinished = dir.path().join("unfinished.sql");
    save(cx, &unfinished);
    written(cx, &unfinished, open_string);
    assert_eq!(app.active_query(cx).1, open_string);
    // The cursor keeps its line.
    set_text(&app, cx, messy, messy.len()..messy.len());
    save(cx, &on);
    written(cx, &on, laid_out);
    let tab = app.tab(cx);
    let cursor = cx.update(|cx| tab.read(cx).editor().read(cx).cursor_position());
    assert_eq!(cursor.line, 0);
}
