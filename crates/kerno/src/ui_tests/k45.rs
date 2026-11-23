//! K45 (docs/plans/M12.md): the command palette opens on the commands run
//! last from it.

use super::*;

/// The palette's sections with nothing typed, and their commands.
fn sections(cx: &mut TestAppContext) -> Vec<(&'static str, Vec<&'static str>)> {
    let recent = settings_now(cx).recent_commands;
    crate::workspace::palette_groups("", &recent)
        .into_iter()
        .map(|(group, commands)| (group, commands.iter().map(|c| c.label).collect()))
        .collect()
}

fn recent(cx: &mut TestAppContext) -> Vec<String> {
    settings_now(cx).recent_commands
}

/// Two commands run from the palette come back first, the latest first, in
/// "Recently used"; Enter on the palette just opened runs the latest; an id
/// no command has is dropped; the list is in the stored settings.
#[gpui_kit::test]
fn the_palette_opens_on_the_commands_run_last(cx: &mut TestAppContext) {
    let storage = memory_storage();
    let app = open(cx, storage.clone());
    // Nothing run yet (an unknown id only): no such section.
    cx.update(|cx| {
        AppState::update_settings(cx, |settings| {
            settings.recent_commands = vec!["gone".into()]
        })
    });
    assert_ne!(sections(cx)[0].0, "Recently used");

    app.press(cx, &["secondary-k"]);
    app.type_text(cx, "theme: dark");
    app.press(cx, &["enter"]);
    app.press(cx, &["secondary-k"]);
    app.type_text(cx, "new query tab");
    app.press(cx, &["enter"]);
    assert_eq!(app.titles(cx), ["Query 1"]);
    assert_eq!(recent(cx), ["new_query_tab", "use_dark_theme", "gone"]);

    // Opened again: the section first, the latest first, the unknown id
    // dropped; Enter runs the first.
    let (heading, commands) = sections(cx).remove(0);
    assert_eq!(heading, "Recently used");
    assert_eq!(commands, ["New Query Tab", "Theme: Dark"]);
    app.press(cx, &["secondary-k"]);
    app.press(cx, &["enter"]);
    assert_eq!(app.titles(cx), ["Query 1", "Query 2"]);
    assert_eq!(recent(cx)[..2], ["new_query_tab", "use_dark_theme"]);

    // Stored with the settings: read back as at the next start.
    app.wait_until(cx, "the stored settings", |_, _| {
        Settings::load(&storage).recent_commands[..2] == ["new_query_tab", "use_dark_theme"]
    });
}
