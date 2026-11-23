//! K55 (the passes in the real app, 2026-11-23): the window's chrome. The
//! palette finds the words typed in any order, and Escape closes it; the
//! tab strip never starts inside a tab; nothing in a query tab's bar moves
//! when Cancel comes and goes.

use gpui_kit::Pixels;

use super::*;

/// The titles the palette lists for `query`, in its order.
fn listed(query: &str) -> Vec<&'static str> {
    crate::workspace::palette_groups(query, &[])
        .into_iter()
        .flat_map(|(_, commands)| commands)
        .map(|command| command.label)
        .collect()
}

/// Every word typed is found in a command's name, its section or its
/// keywords, in any order, case and accents aside; the name itself first,
/// then the names that start with what is typed, then the words in the
/// order typed, then the others.
#[test]
fn the_palette_finds_the_words_typed_in_any_order() {
    for query in ["theme light", "light theme", "Light  THEME", "thème light"] {
        assert_eq!(listed(query).first(), Some(&"Theme: Light"), "{query}");
    }
    // A word of the name and a keyword ("sql"), in any order: among the
    // words apart, more of them in the name first, then a shorter name.
    for query in ["sql new", "new sql", "query new", "tab query new"] {
        let found = listed(query);
        assert_eq!(found.first(), Some(&"New Query Tab"), "{query}: {found:?}");
    }
    assert_eq!(listed("dark theme")[0], "Theme: Dark");
    // A word of its section: "Tabs" holds Close Tab and Duplicate Tab.
    let tabs = listed("duplicate tabs");
    assert_eq!(tabs.first(), Some(&"Duplicate Tab"), "{tabs:?}");
    // The name itself, then a name that starts with it, as before.
    assert_eq!(listed("theme: dark")[0], "Theme: Dark");
    assert_eq!(listed("settings")[0], "Settings");
    let close = listed("close tab");
    assert_eq!(close[0], "Close Tab");
    // The words in the order typed come before the same words apart.
    let theme = listed("theme switch");
    assert_eq!(theme[0], "Theme: Switch Light and Dark", "{theme:?}");
    // Every word must be found.
    assert!(listed("theme zzz").is_empty());
    // Nothing typed (or only spaces): the sections, as before.
    assert_eq!(
        crate::workspace::palette_groups("  ", &[]).len(),
        crate::workspace::palette_groups("", &[]).len()
    );
}

/// The palette itself (not only its list) finds a command by its words in
/// another order, and Enter runs it.
#[gpui_kit::test]
fn the_palette_runs_a_command_typed_in_another_order(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-k"]);
    app.type_text(cx, "dark theme");
    app.press(cx, &["enter"]);
    cx.update(|cx| assert!(cx.theme().is_dark()));
    app.press(cx, &["secondary-k"]);
    app.type_text(cx, "sql new");
    app.press(cx, &["enter"]);
    assert_eq!(app.titles(cx), ["Query 1"]);
}

/// One Escape closes the palette, with a command typed, with a text that
/// finds nothing, or with nothing typed; the next keys go to the editor.
#[gpui_kit::test]
fn escape_always_closes_the_palette(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.press(cx, &["secondary-t"]);
    for typed in ["close tab", "zzz", ""] {
        app.press(cx, &["secondary-k"]);
        assert!(app.dialog_open(cx), "{typed:?}");
        app.type_text(cx, typed);
        app.press(cx, &["escape"]);
        assert!(!app.dialog_open(cx), "{typed:?}: one Escape closes it");
    }
    assert_eq!(app.titles(cx), ["Query 1"], "nothing ran");
    app.type_text(cx, "select 1");
    let sql = cx.update(|cx| {
        let workspace = app.workspace.read(cx);
        workspace.active_query_tab().map(|tab| tab.read(cx).sql(cx))
    });
    assert_eq!(sql.as_deref(), Some("select 1"));
}

/// `count` query tabs, the last one active.
fn query_tabs(count: usize) -> Arc<Storage> {
    let storage = memory_storage();
    let tabs: Vec<TabSnapshot> = (1..=count)
        .map(|number| TabSnapshot::Query {
            title: format!("Query {number}"),
            sql: String::new(),
            target: None,
            file: None,
            unsaved: false,
            source: None,
        })
        .collect();
    storage.save_tabs(&tabs).unwrap();
    storage
        .set_setting(crate::state::ACTIVE_TAB_KEY, &(count - 1))
        .unwrap();
    storage
}

/// Draws frames until the strip of tabs stops moving (the kit's reveal of
/// a clicked tab is animated), then checks that it starts on a tab's start
/// and shows the active tab whole.
fn strip_settles(app: &Fixture, cx: &mut TestAppContext, what: &str) {
    for _ in 0..30 {
        app.step(cx, |window, _| window.refresh());
        std::thread::sleep(Duration::from_millis(10));
    }
    let (cut, (inside, overflows)) = cx.update(|cx| {
        let workspace = app.workspace.read(cx);
        (
            workspace.tab_cut_at_start(cx),
            workspace.active_tab_in_view(),
        )
    });
    assert_eq!(cut, None, "{what}: a tab cut at the strip's start");
    assert!(inside, "{what}: the active tab is whole");
    assert!(overflows, "{what}: the tabs do not fit");
}

/// Forty tabs, in each layout: whatever scrolled the strip (a
/// restore, a tab activated by key, a click on the first tab in view, a new
/// tab, a closed one), it never starts inside a tab, and the active tab is
/// whole.
#[gpui_kit::test]
fn the_tab_strip_never_starts_inside_a_tab(cx: &mut TestAppContext) {
    let app = open(cx, query_tabs(40));
    let layouts: [(Option<Box<dyn gpui_kit::Action>>, &str); 3] = [
        (None, "one row"),
        (Some(Box::new(actions::TabsOnSeveralRows)), "rows"),
        (Some(Box::new(actions::TabsAtTheSide)), "side list"),
    ];
    for (action, layout) in layouts {
        if let Some(action) = action {
            app.step(cx, |window, cx| window.dispatch_action(action, cx));
        }
        strip_settles(&app, cx, &format!("{layout}, the last tab active"));
        for step in 0..6 {
            app.dispatch(cx, actions::PreviousTab);
            strip_settles(&app, cx, &format!("{layout}, {step} back by key"));
        }
        app.dispatch(cx, actions::NextTab);
        strip_settles(&app, cx, &format!("{layout}, one on by key"));
        // The first tab in view, clicked: the kit's reveal shows part of
        // the tab before it.
        let first = cx.update(|cx| app.workspace.read(cx).first_tab_in_view(cx));
        let first = first.expect("a tab in view");
        app.click(cx, ("tab-title", first));
        strip_settles(&app, cx, &format!("{layout}, tab {first} clicked"));
        app.press(cx, &["secondary-t"]);
        strip_settles(&app, cx, &format!("{layout}, a new tab"));
        app.press(cx, &["secondary-w"]);
        strip_settles(&app, cx, &format!("{layout}, a tab closed"));
    }
}

/// Where the buttons of a query tab's bar are, right of its pickers.
fn bar_places(app: &Fixture, cx: &mut TestAppContext) -> Vec<Bounds<Pixels>> {
    app.step(cx, |window, _| {
        [
            "tab-history",
            "tab-saved",
            "tab-explain",
            "tab-run-all",
            "tab-run",
            "tab-run-menu",
        ]
        .map(|id| window.find(id).bounds())
        .to_vec()
    })
}

/// Cancel appears in the room kept for it while nothing runs: the buttons
/// of the bar stay where they are when a run starts and when it ends.
#[gpui_kit::test]
fn nothing_in_the_query_bar_moves_when_a_run_starts_or_ends(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    app.press(cx, &["secondary-t"]);
    app.type_text(cx, "select pg_sleep(30) as kerno_ui_k55_bar");
    let idle = bar_places(&app, cx);
    let room = app.step(cx, |window, _| {
        window.try_find("tab-cancel-room").map(|room| room.bounds())
    });
    assert!(!app.has(cx, "tab-cancel"));
    app.press(cx, &["secondary-enter"]);
    app.wait_until(cx, "the statement to start", |app, cx| {
        app.running(cx) && running_on_server(Engine::Postgres, "kerno_ui_k55_bar")
    });
    assert_eq!(bar_places(&app, cx), idle, "while it runs");
    let cancel = app.step(cx, |window, _| window.find("tab-cancel").bounds());
    let across = |bounds: Bounds<Pixels>| (bounds.left(), bounds.right());
    assert_eq!(
        room.map(across),
        Some(across(cancel)),
        "Cancel takes its room"
    );
    assert!(!app.has(cx, "tab-cancel-room"));
    app.press(cx, &["secondary-."]);
    app.wait_until(cx, "the cancel", |app, cx| !app.running(cx));
    assert!(!app.has(cx, "tab-cancel"));
    assert_eq!(bar_places(&app, cx), idle, "after the run");
}
