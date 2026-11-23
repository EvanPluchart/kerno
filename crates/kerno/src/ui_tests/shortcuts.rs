//! K20, the owner's request of 2026-11-20 (docs/plans/M12.md): a theme
//! switch, Settings ▸ Shortcuts, more default shortcuts.

use gpui_kit::component::WindowExt as _;
use gpui_kit::{Keystroke, TestAppContext};

use super::*;

pub(super) fn keymap_file(cx: &mut TestAppContext) -> std::path::PathBuf {
    cx.update(|cx| AppState::get(cx).paths.keymap())
}

/// Opens Settings ▸ Shortcuts and waits for it to read the keymap file.
pub(super) fn open_shortcuts(app: &Fixture, cx: &mut TestAppContext) {
    app.dispatch(cx, actions::OpenKeyboardShortcuts);
    app.wait_until(cx, "the shortcuts page", |app, cx| {
        app.has(cx, "shortcuts-filter") || app.has(cx, "shortcuts-unreadable")
    });
}

/// Types `query` in the page's filter, in place of what was there.
pub(super) fn filter(app: &Fixture, cx: &mut TestAppContext, query: &str) {
    app.click(cx, "shortcuts-filter");
    app.press(cx, &["secondary-a", "backspace"]);
    app.type_text(cx, query);
}

fn close_settings(app: &Fixture, cx: &mut TestAppContext) {
    app.step(cx, |window, cx| window.close_dialog(cx));
    assert!(!app.dialog_open(cx));
}

/// The text painted inside the element `id`.
pub(super) fn text_of(app: &Fixture, cx: &mut TestAppContext, id: &'static str) -> String {
    let id = gpui_kit::ElementId::from(id);
    app.step(cx, |window, _| {
        gpui_kit::base::test_support::texts(window)
            .iter()
            .filter(|text| text.path().contains(&id))
            .map(|text| text.text().to_string())
            .collect::<Vec<_>>()
            .join(" ")
    })
}

fn sidebar_visible(cx: &mut TestAppContext) -> bool {
    settings_now(cx).sidebar_visible
}

/// The command whose binding wins for `key` where the keyboard is now.
pub(super) fn winner(app: &Fixture, cx: &mut TestAppContext, key: &str) -> Option<String> {
    app.step(cx, |window, cx| {
        let stroke = Keystroke::parse_with_os(key, actions::HOST_OS).unwrap();
        let keymap = cx.key_bindings();
        let keymap = keymap.borrow();
        let (found, _) = keymap.bindings_for_input(&[stroke], &window.context_stack());
        found
            .first()
            .map(|binding| binding.action().name().to_string())
    })
}

/// K20: every command is listed under its group with its shortcuts, those
/// the user changed marked; the filter finds a command by its name, its group
/// or its shortcut as the tooltips write it, and says when nothing matches.
#[gpui_kit::test]
fn the_shortcuts_page_lists_the_commands_and_filters_them(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    std::fs::write(keymap_file(cx), r#"{"bindings": {"about": "f6"}}"#).unwrap();
    open_shortcuts(&app, cx);
    for id in [
        "shortcuts-group-Connections",
        "shortcuts-group-Kerno",
        "shortcut-new_connection-row",
        "shortcut-about-changed",
    ] {
        assert!(app.has(cx, id), "{id}");
    }
    assert!(!app.has(cx, "shortcut-new_connection-changed"));
    // By name.
    filter(&app, cx, "theme switch");
    assert!(app.has(cx, "shortcut-toggle_theme-row"));
    assert!(!app.has(cx, "shortcut-new_connection-row"));
    assert!(app.has(cx, "shortcut-toggle_theme-key-0"));
    // By shortcut, as the tooltips write it (⌘K on macOS).
    filter(&app, cx, &actions::shown("secondary-k"));
    assert!(app.has(cx, "shortcut-open_command_palette-row"));
    assert!(!app.has(cx, "shortcut-toggle_theme-row"));
    // The user's shortcut, by its key.
    filter(&app, cx, "f6");
    assert!(app.has(cx, "shortcut-about-row"));
    // Nothing: said, with the way back.
    filter(&app, cx, "zzzq");
    assert!(app.has(cx, "shortcuts-no-match"));
    app.click(cx, "shortcuts-filter-clear");
    assert!(app.has(cx, "shortcut-new_connection-row") && !app.has(cx, "shortcuts-no-match"));
}

/// K20: a shortcut recorded on the page works at once (the next press runs
/// the command, Settings closed) and is written to the keymap file; a key
/// that would type is refused and recording goes on; Escape cancels it and
/// leaves Settings open.
#[gpui_kit::test]
fn recording_a_shortcut_binds_it_at_once_and_writes_the_file(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    let file = keymap_file(cx);
    assert!(!file.exists());
    open_shortcuts(&app, cx);
    filter(&app, cx, "About Kerno");
    // Escape cancels and keeps Settings.
    app.click(cx, "shortcut-about-add");
    assert!(app.has(cx, "shortcut-recording"));
    app.press(cx, &["escape"]);
    assert!(!app.has(cx, "shortcut-recording") && app.dialog_open(cx));
    // A letter alone would eat typing: refused, still recording.
    app.click(cx, "shortcut-about-add");
    app.press(cx, &["j"]);
    assert!(app.has(cx, "shortcut-recording"));
    assert!(
        text_of(&app, cx, "shortcut-recording").contains("types a character"),
        "{}",
        text_of(&app, cx, "shortcut-recording")
    );
    app.press(cx, &["secondary-alt-shift-j"]);
    assert!(!app.has(cx, "shortcut-recording"));
    assert!(app.has(cx, "shortcut-about-changed") && app.has(cx, "shortcut-about-key-0"));
    close_settings(&app, cx);
    app.press(cx, &["secondary-alt-shift-j"]);
    assert!(app.dialog_open(cx), "About opens with the new shortcut");
    app.wait_until(cx, "the keymap file", |_, _| {
        std::fs::read_to_string(&file)
            .is_ok_and(|text| text.contains(r#""about": "secondary-alt-shift-j""#))
    });
    // Written over the example: the file stays documented.
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("\"commands\"") && text.contains("\"about\": ["));
}

/// K20: a shortcut another command answers is never taken silently: the
/// page names the command; Cancel keeps both as they were, Replace moves the
/// key (the other command loses it, in the file too).
#[gpui_kit::test]
fn a_shortcut_already_taken_asks_and_replace_moves_it(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    let file = keymap_file(cx);
    open_shortcuts(&app, cx);
    filter(&app, cx, "About Kerno");
    app.click(cx, "shortcut-about-add");
    app.press(cx, &["secondary-b"]);
    assert!(app.has(cx, "shortcut-question"));
    let question = text_of(&app, cx, "shortcut-question");
    assert!(question.contains("“Toggle Sidebar” (View)"), "{question}");
    app.click(cx, "shortcut-keep");
    assert!(!app.has(cx, "shortcut-question") && !app.has(cx, "shortcut-about-changed"));
    // A key with Enter: its key-up does not reach the Add button clicked to
    // record (which would start recording again and take the question away).
    app.click(cx, "shortcut-about-add");
    app.press(cx, &["secondary-enter"]);
    assert!(
        app.has(cx, "shortcut-question") && !app.has(cx, "shortcut-recording"),
        "{}",
        text_of(&app, cx, "shortcut-recording")
    );
    assert!(text_of(&app, cx, "shortcut-question").contains("“Run Statement”"));
    app.click(cx, "shortcut-keep");
    // Replace.
    app.click(cx, "shortcut-about-add");
    app.press(cx, &["secondary-b"]);
    app.click(cx, "shortcut-replace");
    assert!(app.has(cx, "shortcut-about-key-0"));
    close_settings(&app, cx);
    let shown = sidebar_visible(cx);
    app.press(cx, &["secondary-b"]);
    assert!(app.dialog_open(cx), "About has the key");
    assert_eq!(sidebar_visible(cx), shown, "Toggle Sidebar lost it");
    app.wait_until(cx, "the keymap file", |_, _| {
        std::fs::read_to_string(&file).is_ok_and(|text| {
            text.contains(r#""about": "secondary-b""#) && text.contains(r#""toggle_sidebar": null"#)
        })
    });
}

/// K20: Remove and Reset to default act at once; Reset all asks first and
/// gives every command its defaults back, in the file too.
#[gpui_kit::test]
fn reset_gives_the_default_back_and_reset_all_asks(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    let file = keymap_file(cx);
    open_shortcuts(&app, cx);
    filter(&app, cx, "Toggle Sidebar");
    app.click(cx, "shortcut-toggle_sidebar-remove-0");
    assert!(app.has(cx, "shortcut-toggle_sidebar-changed"));
    assert!(!app.has(cx, "shortcut-toggle_sidebar-key-0"));
    close_settings(&app, cx);
    let shown = sidebar_visible(cx);
    app.press(cx, &["secondary-b"]);
    assert_eq!(sidebar_visible(cx), shown, "no shortcut any more");
    // Reset to default: back at once (Settings opened again reads what the
    // page wrote, written or not yet).
    open_shortcuts(&app, cx);
    filter(&app, cx, "Toggle Sidebar");
    app.click(cx, "shortcut-toggle_sidebar-reset");
    assert!(!app.has(cx, "shortcut-toggle_sidebar-changed"));
    close_settings(&app, cx);
    app.press(cx, &["secondary-b"]);
    assert_eq!(sidebar_visible(cx), !shown);
    // Two changes, then Reset all: asked, Cancel keeps them, Reset all
    // ends them.
    open_shortcuts(&app, cx);
    filter(&app, cx, "Toggle Sidebar");
    app.click(cx, "shortcut-toggle_sidebar-remove-0");
    filter(&app, cx, "About Kerno");
    app.click(cx, "shortcut-about-add");
    app.press(cx, &["f6"]);
    app.click(cx, "shortcuts-reset-all");
    let question = text_of(&app, cx, "shortcuts-reset-all-question");
    assert!(question.contains("2 changed commands"), "{question}");
    app.click(cx, "shortcuts-reset-all-cancel");
    assert!(app.has(cx, "shortcut-about-changed"));
    app.click(cx, "shortcuts-reset-all");
    app.click(cx, "shortcuts-reset-all-confirm");
    assert!(!app.has(cx, "shortcut-about-changed"));
    close_settings(&app, cx);
    let before = sidebar_visible(cx);
    app.press(cx, &["secondary-b"]);
    assert_eq!(sidebar_visible(cx), !before);
    app.wait_until(cx, "the keymap file", |_, _| {
        std::fs::read_to_string(&file).is_ok_and(|text| {
            let value: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
            value["bindings"] == serde_json::json!({})
        })
    });
}

/// Real-app pass: the question about a shortcut already taken stayed on screen after the
/// row's "Reset to default"; "Reset all…" said "1 changed command lose their changes". The
/// question goes with anything that ends its recording; the sentence agrees with its count.
#[gpui_kit::test]
fn the_question_goes_when_its_row_changes_and_reset_all_agrees(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    open_shortcuts(&app, cx);
    filter(&app, cx, "About Kerno");
    app.click(cx, "shortcut-about-add");
    app.press(cx, &["f6"]);
    assert!(app.has(cx, "shortcut-about-changed"));
    app.click(cx, "shortcuts-reset-all");
    let question = text_of(&app, cx, "shortcuts-reset-all-question");
    assert!(
        question.contains("1 changed command loses its changes."),
        "{question}"
    );
    app.click(cx, "shortcuts-reset-all-cancel");
    // The question, then the row's Reset to default.
    app.click(cx, "shortcut-about-add");
    app.press(cx, &["secondary-b"]);
    assert!(app.has(cx, "shortcut-question"));
    app.click(cx, "shortcut-about-reset");
    assert!(!app.has(cx, "shortcut-question"));
    // The question, then Reset all…, then the filter.
    app.click(cx, "shortcut-about-add");
    app.press(cx, &["f6"]);
    app.click(cx, "shortcut-about-add");
    app.press(cx, &["secondary-b"]);
    assert!(app.has(cx, "shortcut-question"));
    app.click(cx, "shortcuts-reset-all");
    assert!(!app.has(cx, "shortcut-question"));
    app.click(cx, "shortcuts-reset-all-cancel");
    app.click(cx, "shortcut-about-add");
    app.press(cx, &["secondary-b"]);
    assert!(app.has(cx, "shortcut-question"));
    filter(&app, cx, "About");
    assert!(!app.has(cx, "shortcut-question"));
}

/// K20: a keymap file Kerno cannot read is said on the page, with a button
/// to open it, and is neither rewritten nor applied.
#[gpui_kit::test]
fn an_invalid_keymap_file_is_said_and_never_overwritten(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    let file = keymap_file(cx);
    let broken = "{ \"bindings\": { \"about\": \"secondary-b\" ";
    std::fs::write(&file, broken).unwrap();
    open_shortcuts(&app, cx);
    assert!(app.has(cx, "shortcuts-unreadable") && app.has(cx, "shortcuts-open-file"));
    assert!(text_of(&app, cx, "shortcuts-unreadable").contains("not valid JSON"));
    assert!(!app.has(cx, "shortcut-about-row"));
    close_settings(&app, cx);
    // Not applied: ⌘B still toggles the sidebar.
    let shown = sidebar_visible(cx);
    app.press(cx, &["secondary-b"]);
    assert_eq!(sidebar_visible(cx), !shown);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), broken);
}

/// K20: Enter recorded for a command of the grids is taken once: its key-up
/// does not reach the button clicked to record, which would start again.
#[gpui_kit::test]
fn enter_is_recorded_once_for_a_command_of_the_grids(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    open_shortcuts(&app, cx);
    filter(&app, cx, "Toggle Record View");
    app.click(cx, "shortcut-toggle_record_view-add");
    app.press(cx, &["enter"]);
    assert!(!app.has(cx, "shortcut-recording"));
    assert!(app.has(cx, "shortcut-toggle_record_view-key-1"));
    assert!(app.has(cx, "shortcut-toggle_record_view-changed"));
}

/// K20: a key of the text fields (⌘C) is not given to a command that works
/// everywhere, which would take copying from every field; recording goes on.
#[gpui_kit::test]
fn a_key_of_the_text_fields_is_kept_for_them(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    open_shortcuts(&app, cx);
    filter(&app, cx, "About Kerno");
    app.click(cx, "shortcut-about-add");
    app.press(cx, &["secondary-c"]);
    assert!(app.has(cx, "shortcut-recording"));
    let refused = text_of(&app, cx, "shortcut-recording");
    assert!(refused.contains("text fields' Copy"), "{refused}");
    assert!(!app.has(cx, "shortcut-about-changed"));
    app.press(cx, &["f6"]);
    assert!(!app.has(cx, "shortcut-recording") && app.has(cx, "shortcut-about-changed"));
}

/// K20: a recording stops when Settings shows another section: what is typed
/// next goes where the user put it.
#[gpui_kit::test]
fn another_section_stops_the_recording(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    open_shortcuts(&app, cx);
    filter(&app, cx, "About Kerno");
    app.click(cx, "shortcut-about-add");
    assert!(app.has(cx, "shortcut-recording"));
    app.click(cx, ("settings-section", 0usize));
    app.click(cx, "settings-search");
    app.type_text(cx, "vim");
    assert!(app.has(cx, "settings-vim-mode"), "the search got the text");
}

/// K20: Settings closed while a shortcut is recorded gives the keyboard back:
/// ⌘K opens the palette again.
#[gpui_kit::test]
fn closing_settings_while_recording_gives_the_keyboard_back(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    open_shortcuts(&app, cx);
    filter(&app, cx, "About Kerno");
    app.click(cx, "shortcut-about-add");
    assert!(app.has(cx, "shortcut-recording"));
    close_settings(&app, cx);
    app.press(cx, &["secondary-k"]);
    assert!(app.dialog_open(cx), "the palette opens");
    assert!(!keymap_file(cx).exists(), "nothing was recorded");
}

/// K20: the theme switch shows the opposite of the theme on screen and
/// stores it as the choice, from each of the three modes, by its shortcut,
/// from Settings too; it is in the View menu.
#[gpui_kit::test]
fn the_theme_switch_shows_the_opposite_and_stores_it(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    // Follow system, showing light (the headless window is light).
    assert_eq!(settings_now(cx).appearance, Appearance::System);
    cx.update(|cx| assert!(!cx.theme().is_dark()));
    app.press(cx, &["secondary-shift-d"]);
    cx.update(|cx| assert!(cx.theme().is_dark()));
    assert_eq!(settings_now(cx).appearance, Appearance::Dark);
    // From Dark.
    app.press(cx, &["secondary-shift-d"]);
    cx.update(|cx| assert!(!cx.theme().is_dark()));
    assert_eq!(settings_now(cx).appearance, Appearance::Light);
    // From Light.
    app.dispatch(cx, actions::ToggleTheme);
    assert_eq!(settings_now(cx).appearance, Appearance::Dark);
    // Follow system, showing dark: Light.
    app.dispatch(cx, actions::UseSystemTheme);
    cx.update(|cx| theme::apply(Appearance::Dark, None, cx));
    app.dispatch(cx, actions::ToggleTheme);
    cx.update(|cx| assert!(!cx.theme().is_dark()));
    assert_eq!(settings_now(cx).appearance, Appearance::Light);
    // In Settings, its shortcut works and the Theme row shows the choice.
    app.press(cx, &["secondary-,"]);
    app.press(cx, &["secondary-shift-d"]);
    assert!(app.dialog_open(cx));
    assert_eq!(settings_now(cx).appearance, Appearance::Dark);
    cx.update(|cx| assert!(cx.theme().is_dark()));
    // The View menu has it.
    let menus = cx.update(|cx| cx.get_menus()).unwrap();
    let view = menus.iter().find(|menu| menu.name == "View").unwrap();
    assert!(view.items.iter().any(|item| matches!(
        item,
        gpui_kit::OwnedMenuItem::Action { name, .. } if name == "Theme: Switch Light and Dark"
    )));
}

/// K20: a change of the system's appearance switches the theme in "Follow
/// system" only. The headless window cannot change its appearance (GPUI's
/// `simulate_appearance_change` is crate-private): the observer's body is
/// called as the window would.
#[gpui_kit::test]
fn the_system_appearance_is_followed_only_in_follow_system(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    // Dark on screen, Follow system: the light system wins.
    cx.update(|cx| theme::apply(Appearance::Dark, None, cx));
    app.step(cx, crate::workspace::system_appearance_changed);
    cx.update(|cx| assert!(!cx.theme().is_dark()));
    // Dark chosen: it stays.
    app.dispatch(cx, actions::UseDarkTheme);
    app.step(cx, crate::workspace::system_appearance_changed);
    cx.update(|cx| assert!(cx.theme().is_dark()));
}

/// K20: Settings' search finds the page by its words, and opens it.
#[gpui_kit::test]
fn the_settings_search_finds_the_shortcuts_page(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-,"]);
    app.type_text(cx, "keymap");
    assert!(app.has(cx, "row-shortcuts"));
    let index = crate::settings_view::SECTIONS
        .iter()
        .position(|(_, label)| *label == "Shortcuts")
        .unwrap();
    app.click(cx, ("settings-open", index));
    app.wait_until(cx, "the shortcuts page", |app, cx| {
        app.has(cx, "shortcuts-filter")
    });
}

/// K20: the default keys of commands with no key context that the kit binds
/// too. GPUI ranks a binding with no context with the deepest one and Kerno
/// binds after the kit, so Kerno's wins in every field: these are chosen
/// (⌘↵ runs the statement even from a field), and the page lets them be
/// given to another command, since the fields lost them already.
#[gpui_kit::test]
fn the_global_defaults_over_a_key_of_the_kit_are_known(cx: &mut TestAppContext) {
    let _app = open(cx, memory_storage());
    let found: Vec<String> = actions::COMMANDS
        .iter()
        .filter(|command| command.context.is_none())
        .flat_map(|command| command.keys.iter().map(move |key| (command.id, *key)))
        .filter_map(|(id, key)| {
            let action = cx.update(|cx| actions::kit_binding_of(key, cx))?;
            Some(format!("{id} {key} {action}"))
        })
        .collect();
    assert_eq!(
        found,
        [
            "run_statement secondary-enter Confirm",
            "cancel_query secondary-. ToggleCodeActions"
        ]
    );
}

/// K20: each new default wins where the keyboard is all day: in the SQL
/// editor and in a text field (the kit's inputs take none of them).
#[gpui_kit::test]
fn the_new_defaults_win_in_the_editor_and_in_a_text_field(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    let expected = |id: &str| -> String {
        let command = actions::COMMANDS.iter().find(|c| c.id == id).unwrap();
        (command.action)().name().to_string()
    };
    // No binding of the kit (text fields, editor, lists, dialogs) has these
    // keys: a key bound everywhere would win over it in every field.
    for (id, key) in actions::K20_DEFAULTS {
        assert_eq!(
            cx.update(|cx| actions::kit_binding_of(key, cx)),
            None,
            "{id} {key}"
        );
    }
    // The keys added beside an older one come first: the older one, the
    // last bound, is still what menus and tooltips show.
    for (action, shown) in [
        (
            Box::new(actions::CommitTransaction) as Box<dyn gpui_kit::Action>,
            "ctrl-4",
        ),
        (Box::new(actions::RollbackTransaction), "ctrl-8"),
        (Box::new(actions::NextTab), "ctrl-tab"),
        (Box::new(actions::PreviousTab), "ctrl-shift-tab"),
    ] {
        let last = cx.update(|cx| {
            use gpui_kit::AsKeystroke as _;
            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            let binding = keymap
                .bindings_for_action(action.as_ref())
                .next_back()
                .unwrap();
            binding.keystrokes()[0]
                .as_keystroke()
                .unparse(gpui_kit::OperatingSystem::Mac)
        });
        assert_eq!(last, shown, "{}", action.name());
    }
    app.press(cx, &["secondary-t"]);
    assert_eq!(app.titles(cx), ["Query 1"]);
    for (id, key) in actions::K20_DEFAULTS {
        assert_eq!(
            winner(&app, cx, key),
            Some(expected(id)),
            "editor: {id} {key}"
        );
    }
    app.press(cx, &["secondary-,"]);
    assert!(app.has(cx, "settings-search"));
    for (id, key) in actions::K20_DEFAULTS {
        assert_eq!(
            winner(&app, cx, key),
            Some(expected(id)),
            "field: {id} {key}"
        );
    }
}
