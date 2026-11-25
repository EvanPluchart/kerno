//! K76 (docs/plans/M12.md, D-141): a tooltip of the tree whose row is clicked before the
//! tooltip shows no longer appears over the dialog the click opened. The tests of the "Fill
//! with test data" dialog's K76 changes are in `k72.rs`, beside the dialog's others.

use gpui_kit::component::WindowExt as _;
use kerno_core::config::DatabasesShown;

use super::*;

const HIDDEN_TIP: &str = "Edit the connection to choose the databases shown";

/// Whether the tooltip of the "databases hidden" line is drawn.
fn tip_shown(app: &Fixture, cx: &mut TestAppContext) -> bool {
    app.step(cx, |window, cx| {
        window.render_frame(cx);
        gpui_kit::base::test_support::texts(window)
            .iter()
            .any(|text| text.text() == HIDDEN_TIP)
    })
}

/// The tooltip of the tree's "databases hidden" line shows when the pointer rests on it; a
/// click before it shows opens the connection's dialog, and the tooltip never comes over the
/// dialog, nor back when the dialog closes with the pointer still there.
#[gpui_kit::test]
fn a_tooltip_does_not_come_over_the_dialog_its_row_opened(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let mut config = testing::config(Engine::Postgres);
    config.databases_shown = DatabasesShown::DefaultOnly;
    let app = connected_as(cx, config);
    let line = ("databases-hidden", 0usize);
    app.wait_until(cx, "the hidden line", |app, cx| app.has(cx, line));
    // The pointer rests on the line: its tooltip shows (the rows above it may still move while
    // the database's size is read).
    app.wait_until(cx, "the tooltip", |app, cx| {
        app.step(cx, |window, cx| window.hover(line, cx));
        cx.executor().advance_clock(Duration::from_secs(1));
        tip_shown(app, cx)
    });
    // Away, then back and a click at once, before the tooltip shows.
    app.step(cx, |window, cx| window.hover("navigator-filter", cx));
    cx.executor().advance_clock(Duration::from_secs(1));
    assert!(!tip_shown(&app, cx));
    app.step(cx, |window, cx| window.hover(line, cx));
    cx.executor().advance_clock(Duration::from_millis(100));
    app.click(cx, line);
    app.wait_until(cx, "the connection's dialog", |app, cx| {
        app.has(cx, "conn-databases-shown")
    });
    cx.executor().advance_clock(Duration::from_secs(2));
    assert!(!tip_shown(&app, cx), "the tooltip over the dialog");
    // Closed with the pointer still on the line: the tooltip does not come back.
    app.step(cx, |window, cx| window.close_all_dialogs(cx));
    app.wait_until(cx, "the dialog closed", |app, cx| {
        cx.executor().advance_clock(Duration::from_millis(100));
        !app.has(cx, "conn-databases-shown")
    });
    cx.executor().advance_clock(Duration::from_secs(2));
    assert!(!tip_shown(&app, cx), "the tooltip back after the dialog");
}
