//! K25 (docs/plans/M12.md): the look of Settings. Choices among a few options are cards a
//! click, the arrows or the search reach; every page fits the dialog and scrolls inside it,
//! at the usual window size and a small one, at any interface text size.

use gpui_kit::{Bounds, Pixels, SharedString};

use super::*;
use crate::settings::{TabLayout, TableDoubleClick};
use crate::settings_view::SECTIONS;

fn settings_now(cx: &mut TestAppContext) -> Settings {
    cx.update(|cx| AppState::get(cx).settings.clone())
}

fn bounds(
    app: &Fixture,
    cx: &mut TestAppContext,
    id: impl Into<gpui_kit::ElementId>,
) -> Bounds<Pixels> {
    let id = id.into();
    app.step(cx, |window, _| window.find(id).bounds())
}

/// Whether card `index` of `group` is the one chosen: announced as selected, with its check.
fn chosen(app: &Fixture, cx: &mut TestAppContext, group: &'static str, index: usize) -> bool {
    let selected = app.step(cx, |window, _| window.within(group).find(index).selected());
    selected == Some(true) && app.has(cx, crate::settings_ui::check_id(group, index))
}

fn focused(app: &Fixture, cx: &mut TestAppContext, group: &'static str, index: usize) -> bool {
    app.step(cx, |window, _| window.within(group).find(index).focused()) == Some(true)
}

/// A click on a theme card switches the theme, stores the choice and marks that card alone;
/// the same for the layout of the tabs.
#[gpui_kit::test]
fn a_card_applies_its_choice_stores_it_and_is_the_one_marked(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-,"]);
    assert!(
        chosen(&app, cx, "appearance", 0),
        "Follow system to begin with"
    );

    app.step(cx, |window, cx| {
        window.within("appearance").click(2usize, cx)
    });
    cx.update(|cx| assert!(cx.theme().is_dark()));
    assert_eq!(settings_now(cx).appearance, Appearance::Dark);
    assert!(chosen(&app, cx, "appearance", 2));
    assert!(!chosen(&app, cx, "appearance", 0) && !chosen(&app, cx, "appearance", 1));
    app.wait_until(cx, "the stored theme", |app, _| {
        Settings::load(&app.storage).appearance == Appearance::Dark
    });

    app.step(cx, |window, cx| {
        window.within("settings-tab-layout").click(2usize, cx)
    });
    assert_eq!(settings_now(cx).tab_layout, TabLayout::Vertical);
    assert!(chosen(&app, cx, "settings-tab-layout", 2));
    assert!(!chosen(&app, cx, "settings-tab-layout", 0));
}

/// One Tab stop per group of cards (the chosen one); the arrows move the choice and the
/// keyboard with it, around the ends; Tab goes on to the next group.
#[gpui_kit::test]
fn the_arrows_move_the_choice_and_tab_leaves_the_group(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-,"]);
    // The search has the keyboard; the page comes next, its first group's chosen card.
    app.press(cx, &["tab"]);
    assert!(focused(&app, cx, "appearance", 0));

    app.press(cx, &["right"]);
    assert_eq!(settings_now(cx).appearance, Appearance::Light);
    assert!(focused(&app, cx, "appearance", 1) && chosen(&app, cx, "appearance", 1));

    // Tab leaves the group from the middle card for the next group, without changing
    // either; Shift-Tab comes back to the chosen card.
    app.press(cx, &["tab"]);
    assert!(focused(&app, cx, "table-double-click", 0));
    assert_eq!(settings_now(cx).appearance, Appearance::Light);
    app.press(cx, &["shift-tab"]);
    assert!(focused(&app, cx, "appearance", 1));

    app.press(cx, &["left", "left"]);
    assert_eq!(
        settings_now(cx).appearance,
        Appearance::Dark,
        "around the start"
    );
    assert!(focused(&app, cx, "appearance", 2));
    cx.update(|cx| assert!(cx.theme().is_dark()));

    app.press(cx, &["tab", "down"]);
    assert_eq!(
        settings_now(cx).table_double_click,
        TableDoubleClick::Structure
    );
}

/// K13's search finds a setting drawn as cards, under its section's name; the cards work
/// where they are found, and "Show the … page" opens the page they are on.
#[gpui_kit::test]
fn the_search_finds_a_card_setting_and_leads_to_its_page(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-,"]);
    app.click(cx, ("settings-section", 3usize));
    assert!(!app.has(cx, "row-appearance"));
    app.click(cx, "settings-search");
    app.type_text(cx, "dark");
    assert!(app.has(cx, ("settings-found", 0usize)) && app.has(cx, "row-appearance"));
    assert!(!app.has(cx, "row-table-double-click"), "only what matches");
    app.step(cx, |window, cx| {
        window.within("appearance").click(2usize, cx)
    });
    assert_eq!(settings_now(cx).appearance, Appearance::Dark);

    app.click(cx, ("settings-found-open", 0usize));
    assert!(
        !app.has(cx, ("settings-found", 0usize)),
        "the search is emptied"
    );
    assert!(app.has(cx, "row-table-double-click") && app.has(cx, "row-settings-tab-layout"));
    assert!(chosen(&app, cx, "appearance", 2));
}

/// Every page keeps to the dialog: Settings inside the window with its footer, the page's
/// rows inside the page's column, the longer pages scrolling there; at 1200 × 800 and at
/// 900 × 600.
#[gpui_kit::test]
fn every_page_fits_the_dialog_and_scrolls_inside_it(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    for (width, height) in [(1200., 800.), (900., 600.)] {
        cx.simulate_window_resize(app.window.into(), size(px(width), px(height)));
        app.press(cx, &["secondary-,"]);
        for (index, (section, name)) in SECTIONS.iter().enumerate() {
            app.click(cx, ("settings-section", index));
            let dialog = bounds(&app, cx, "settings");
            let body = bounds(&app, cx, "settings-body");
            let footer = bounds(&app, cx, "open-keymap-file");
            let at = format!("{name} at {width} × {height}");
            assert!(
                dialog.left() >= px(0.) && dialog.right() <= px(width),
                "{at}"
            );
            assert!(
                dialog.top() >= px(0.) && dialog.bottom() <= px(height),
                "{at}"
            );
            assert!(
                body.bottom() <= footer.top() && footer.bottom() <= dialog.bottom(),
                "{at}"
            );
            assert!(
                body.left() >= dialog.left() && body.right() <= dialog.right(),
                "{at}"
            );
            for row in crate::settings_view::ROWS
                .iter()
                .filter(|row| row.section == *section)
            {
                let id = SharedString::from(format!("row-{}", row.key));
                let Some(found) = app.step(cx, |window, _| window.try_find(id.clone())) else {
                    continue; // A view of its own.
                };
                let row_bounds = found.bounds();
                assert!(
                    row_bounds.left() >= body.left() && row_bounds.right() <= body.right(),
                    "{id} at {at}: {row_bounds:?} in {body:?}"
                );
            }
        }
        // Appearance is taller than its column at this size: it scrolls there.
        app.click(cx, ("settings-section", 0usize));
        let last = |app: &Fixture, cx: &mut TestAppContext| {
            app.step(cx, |window, _| {
                window.find("row-settings-tab-layout").visible()
            })
        };
        let body = bounds(&app, cx, "settings-body");
        let tabs = bounds(&app, cx, "row-settings-tab-layout");
        if tabs.bottom() > body.bottom() {
            for _ in 0..10 {
                app.step(cx, |window, cx| {
                    window.scroll(
                        "settings-body",
                        gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(-200.))),
                        cx,
                    )
                });
            }
            let tabs = bounds(&app, cx, "row-settings-tab-layout");
            assert!(
                tabs.bottom() <= body.bottom() + px(1.),
                "scrolled into view"
            );
        }
        assert!(last(&app, cx));
        app.press(cx, &["escape", "escape"]);
        assert!(!app.dialog_open(cx));
    }
}

/// At the largest interface text size, in both themes and a small window, every card and
/// its name stay whole inside the page's column.
#[gpui_kit::test]
fn no_card_is_cut_at_the_largest_text_size(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    cx.simulate_window_resize(app.window.into(), size(px(900.), px(600.)));
    cx.update(|cx| {
        AppState::update_settings(cx, |settings| {
            settings.ui_font_size = *crate::settings::UI_FONT_SIZES.end()
        });
        theme::apply_fonts(cx);
    });
    let groups: [(&str, [&str; 3]); 3] = [
        ("appearance", Appearance::ALL.map(Appearance::label)),
        (
            "table-double-click",
            TableDoubleClick::ALL.map(TableDoubleClick::label),
        ),
        ("settings-tab-layout", TabLayout::ALL.map(TabLayout::label)),
    ];
    for dark in [false, true] {
        cx.update(|cx| {
            theme::apply(
                if dark {
                    Appearance::Dark
                } else {
                    Appearance::Light
                },
                None,
                cx,
            )
        });
        app.press(cx, &["secondary-,"]);
        let body = bounds(&app, cx, "settings-body");
        for (group, labels) in groups {
            for (index, label) in labels.into_iter().enumerate() {
                let card = app.step(cx, |window, _| window.within(group).find(index).bounds());
                assert!(
                    card.left() >= body.left() && card.right() <= body.right(),
                    "{group} {index}: {card:?} in {body:?}"
                );
                let text = app.step(cx, |window, _| {
                    gpui_kit::base::test_support::texts(window)
                        .into_iter()
                        .find(|text| text.text() == label)
                        .map(|text| text.bounds())
                });
                let text = text.unwrap_or_else(|| panic!("{label} is drawn"));
                assert!(
                    text.left() >= card.left()
                        && text.right() <= card.right()
                        && text.top() >= card.top()
                        && text.bottom() <= card.bottom(),
                    "{label}: {text:?} in {card:?}"
                );
            }
        }
        app.press(cx, &["escape", "escape"]);
    }
}
