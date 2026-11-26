//! K82 (docs/plans/M12.md, D-148): a time with a zone read in the computer's
//! own. The switch under a value, off by default and kept in the settings;
//! on, a line for a value, a list for a JSON document, the Unix-time hint's
//! time here too; the stored value and Copy unchanged; nothing for a value
//! without a zone; the switch off with why when the zone cannot be read. The
//! zone is given to the tests (Europe/Paris' rules), whatever the computer's.

use kerno_core::local_time;
use kerno_core::{Column, TypeKind};

use super::tool_wizards::paints;
use super::*;
use crate::query::viewer::{self, LocalZone};

fn in_paris(app: &Fixture, cx: &mut TestAppContext) -> bool {
    cx.update(|cx| cx.set_global(LocalZone(Ok(local_time::europe_paris()))));
    local_times(app, cx)
}

/// Whether the switch is on, in the settings and, once their writer has
/// written (it writes off the UI thread, a moment after the change), in
/// storage.
fn local_times(app: &Fixture, cx: &mut TestAppContext) -> bool {
    let on = cx.update(|cx| AppState::get(cx).settings.local_times);
    app.wait_until(cx, "the setting stored", |app, _| {
        Settings::load(&app.storage).local_times == on
    });
    on
}

fn view(app: &Fixture, cx: &mut TestAppContext, column: Column, value: &str) {
    app.step(cx, |window, cx| {
        viewer::open(&column, Some(value.as_bytes()), window, cx)
    });
}

fn tstz() -> Column {
    Column::new("ordered_at", "timestamptz", TypeKind::DateTimeTz)
}

/// Off by default: the switch only. On: the time here, with the offset of
/// its own date (June and December differ); Copy still the stored value;
/// the setting kept.
#[gpui_kit::test]
fn a_time_with_a_zone_is_read_in_my_time_zone(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    assert!(!in_paris(&app, cx), "off by default");
    let stored = "2026-06-08 16:17:04.768895+00";
    view(&app, cx, tstz(), stored);
    assert!(app.has(cx, "value-local-times"));
    let switch = app.step(cx, |window, _| window.find("value-local-times"));
    assert_eq!(
        (switch.label(), switch.checked()),
        (Some("Times in my time zone (Europe/Paris)"), Some(false)),
        "{switch:?}"
    );
    assert!(!app.has(cx, "value-local-time"));

    app.click(cx, "value-local-times");
    assert!(local_times(&app, cx));
    assert!(paints(
        &app,
        cx,
        "In Europe/Paris: 2026-06-08 18:17:04.768895 +02:00"
    ));
    app.click(cx, "value-copy");
    assert_eq!(app.clipboard(cx).as_deref(), Some(stored));
    app.click(cx, "value-close");

    view(&app, cx, tstz(), "2026-12-08 09:30:00+00");
    assert!(paints(
        &app,
        cx,
        "In Europe/Paris: 2026-12-08 10:30:00 +01:00"
    ));
    // Off again: nothing but the switch.
    app.click(cx, "value-local-times");
    assert!(!local_times(&app, cx));
    assert!(!app.has(cx, "value-local-time"));
}

/// A date and time without a zone (PostgreSQL's `timestamp`, MySQL's
/// `DATETIME` and `TIMESTAMP` as Kerno reads them) and a plain text: no
/// switch, no line, even with the setting on.
#[gpui_kit::test]
fn a_value_without_a_zone_offers_nothing(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    in_paris(&app, cx);
    app.dispatch(cx, actions::ToggleLocalTimes);
    assert!(local_times(&app, cx), "the palette's command turns it on");
    for (column, value) in [
        (
            Column::new("at", "timestamp", TypeKind::DateTime),
            "2026-06-08 16:17:04",
        ),
        (
            Column::new("at", "TIMESTAMP", TypeKind::DateTime),
            "2026-06-08 16:17:04",
        ),
        (Column::new("on", "date", TypeKind::Date), "2026-06-08"),
        (Column::new("t", "time", TypeKind::Time), "16:17:04"),
        (Column::new("name", "text", TypeKind::Text), "Ada"),
        (
            Column::new("doc", "jsonb", TypeKind::Json),
            r#"{"at": "2026-06-08 16:17:04"}"#,
        ),
    ] {
        let at = format!("{} {value}", column.type_name);
        view(&app, cx, column, value);
        assert!(app.has(cx, "value-text"), "{at}");
        assert!(!app.has(cx, "value-local-times"), "{at}");
        assert!(!app.has(cx, "value-local-time"), "{at}");
        app.click(cx, "value-close");
    }
    app.dispatch(cx, actions::ToggleLocalTimes);
    assert!(!local_times(&app, cx));
}

/// A JSON document lists its times by path, nested ones included, and its
/// text is the document as before.
#[gpui_kit::test]
fn a_json_document_lists_its_times_here(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    in_paris(&app, cx);
    let doc = r#"{"price": 600, "ordered_at": "2026-06-08 16:17:04.768895+00", "customer": "Ada", "items": [{"sku": "A-1", "shipped_at": "2026-12-08T09:30:00Z"}]}"#;
    view(&app, cx, Column::new("doc", "jsonb", TypeKind::Json), doc);
    assert!(!app.has(cx, "value-local-time"));
    app.click(cx, "value-local-times");
    assert!(paints(
        &app,
        cx,
        "In Europe/Paris:\n\
         ordered_at \u{2192} 2026-06-08 18:17:04.768895 +02:00\n\
         items[0].shipped_at \u{2192} 2026-12-08 10:30:00 +01:00"
    ));
    app.click(cx, "value-copy");
    assert_eq!(app.clipboard(cx), kerno_core::format::pretty_json(doc));
}

/// The Unix-time hint keeps its UTC line and, on, says the time here too.
#[gpui_kit::test]
fn a_unix_time_is_read_here_too(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    in_paris(&app, cx);
    app.dispatch(cx, actions::ToggleLocalTimes);
    view(
        &app,
        cx,
        Column::new("at", "int8", TypeKind::Integer),
        "1794619425",
    );
    assert!(paints(
        &app,
        cx,
        "Unix seconds \u{2192} 2026-11-14 01:23:45 UTC"
    ));
    assert!(paints(
        &app,
        cx,
        "In Europe/Paris: 2026-11-14 02:23:45 +01:00"
    ));
}

/// When the computer's zone cannot be read, the switch is off and disabled,
/// and says why.
#[gpui_kit::test]
fn an_unreadable_zone_disables_the_switch(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    cx.update(|cx| {
        cx.set_global(LocalZone(Err(
            "this computer's time zone could not be read (test)".into(),
        )))
    });
    view(&app, cx, tstz(), "2026-06-08 16:17:04+00");
    assert!(paints(
        &app,
        cx,
        "Not available: this computer's time zone could not be read (test)."
    ));
    app.click(cx, "value-local-times");
    assert!(!local_times(&app, cx));
    assert!(!app.has(cx, "value-local-time"));
}

/// The value editor (Shift+Enter) shows the same.
#[gpui_kit::test]
fn the_value_editor_reads_a_time_here(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    in_paris(&app, cx);
    app.dispatch(cx, actions::ToggleLocalTimes);
    let workspace = app.workspace.clone();
    app.step(cx, |window, cx| {
        let back = workspace.focus_handle(cx);
        let save = viewer::Save {
            nullable: true,
            apply: std::rc::Rc::new(|_, _, _| {}),
        };
        viewer::edit(
            &tstz(),
            Some(b"2026-06-08 16:17:04+00"),
            Some(save),
            back,
            window,
            cx,
        )
    });
    assert!(app.has(cx, "value-editor"));
    assert!(paints(
        &app,
        cx,
        "In Europe/Paris: 2026-06-08 18:17:04 +02:00"
    ));
}

/// The value panel beside a result: a `timestamptz` cell's line, a
/// `timestamp` cell's nothing (PostgreSQL).
#[gpui_kit::test]
fn the_value_panel_reads_a_time_here(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    in_paris(&app, cx);
    app.dispatch(cx, actions::ToggleLocalTimes);
    app.run_in_new_tab(
        cx,
        "select '2026-06-08 16:17:04.768895+00'::timestamptz at time zone 'UTC' as plain, \
         '2026-06-08 16:17:04.768895+00'::timestamptz as zoned",
        "secondary-enter",
    );
    app.wait_until(cx, "the row", |app, cx| app.outcomes(cx) == ["1 rows"]);
    app.press(cx, &["f7"]);
    let tab = app.tab(cx);
    let grid = cx.update(|cx| tab.read(cx).grid_of(0)).expect("a grid");
    select_cell(&app, cx, &grid, (0, 1));
    assert!(app.has(cx, "value-panel-body"));
    assert!(!app.has(cx, "value-local-times"));
    select_cell(&app, cx, &grid, (0, 2));
    // The session's zone is the server's: whatever it prints, the time here
    // is the same.
    assert!(paints(
        &app,
        cx,
        "In Europe/Paris: 2026-06-08 18:17:04.768895 +02:00"
    ));
}
