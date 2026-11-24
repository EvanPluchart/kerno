//! The fourth visual list (M10 Part 15): the tinted status bar, the
//! production confirmation, a failed connection's details, the local client
//! panel, the first step of a restore or a script, the backup settings, the
//! global backup's command, a database with no foreign data wrapper, and
//! one size for the settings.

use kerno_core::environment::{PRODUCTION, STAGING};
use kerno_core::tools::{ClientFamily, EngineBackup};

use super::tool_wizards::fake_client;
use super::*;
use crate::backup_dialog::{BackupDialog, Mode};

/// A stand-in PostgreSQL client of `version`, in `root`, made the default.
fn pg_client(
    app: &Fixture,
    cx: &mut TestAppContext,
    root: &Path,
    name: &str,
    version: &str,
) -> std::path::PathBuf {
    let dir = fake_client(
        root,
        name,
        ClientFamily::Postgres,
        &format!("pg_dump (PostgreSQL) {version}"),
        "exit 0",
    );
    app.use_client(cx, ClientFamily::Postgres, &dir);
    dir
}

fn tool_line(app: &Fixture, cx: &mut TestAppContext) -> String {
    if app.has(cx, "backup-tool") {
        app.label_of(cx, "backup-tool")
    } else {
        String::new()
    }
}

#[gpui_kit::test]
fn every_status_bar_item_reads_on_every_built_in_tint(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    for environment in [STAGING, PRODUCTION] {
        let config = ConnectionConfig {
            environment: environment.into(),
            // A warning on the left (read-only); the memory-only warning on
            // the right comes with the test's secret store.
            read_only: true,
            ..testing::config(Engine::Postgres)
        };
        let app = connected_as(cx, config);
        app.press(cx, &["secondary-t"]);
        for appearance in [Appearance::Light, Appearance::Dark] {
            cx.update(|cx| crate::theme::apply(appearance, None, cx));
            let looks = cx.update(|cx| app.workspace.read(cx).status_looks(cx));
            let texts: Vec<&str> = looks.iter().map(|(text, ..)| text.as_str()).collect();
            assert!(
                texts.contains(&"read-only") && texts.iter().any(|t| t.starts_with("Passwords")),
                "{texts:?}"
            );
            for (text, color, bar, bold) in &looks {
                let ratio = theme::contrast(theme::rgb_of(*color), theme::rgb_of(*bar));
                assert!(
                    ratio >= 4.5,
                    "{environment} {appearance:?}: “{text}” is {ratio:.2}:1"
                );
                // A warning keeps a mark, its weight, not its colour.
                let warning = text == "read-only" || text.starts_with("Passwords");
                assert_eq!(*bold, warning, "{environment} {appearance:?}: {text}");
            }
        }
    }
}

#[gpui_kit::test]
fn the_confirmation_on_production_names_it_and_carries_its_colour(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let config = ConnectionConfig {
        environment: PRODUCTION.into(),
        auto_commit: Some(true),
        ..testing::config(Engine::Postgres)
    };
    let name = config.name.clone();
    let app = connected_as(cx, config);
    let band = format!("Production · {name}");

    // Every run asks: the title and the band name the environment.
    app.run_in_new_tab(cx, "select 1 as kerno_ui_v4_confirm", "secondary-enter");
    assert!(app.has(cx, "sql-preview"));
    assert_eq!(
        app.label_of(cx, "sql-title"),
        format!("Run this statement on Production: {name}?")
    );
    assert_eq!(app.label_of(cx, "sql-environment"), band);
    app.click(cx, "sql-cancel");
    assert!(!app.has(cx, "sql-preview"));

    // A write asks twice: the second time says so, under the same title.
    app.press(cx, &["secondary-a"]);
    app.type_text(cx, "create temporary table kerno_ui_v4_confirm (n int)");
    app.press(cx, &["secondary-enter"]);
    assert_eq!(
        app.label_of(cx, "sql-title"),
        format!("Run this statement on Production: {name}?")
    );
    app.click(cx, "sql-confirm");
    assert!(app.has(cx, "sql-preview") && app.has(cx, "sql-production"));
    assert_eq!(app.label_of(cx, "sql-environment"), band);
    assert_eq!(
        app.label_of(cx, "sql-title"),
        format!("Run this statement on Production: {name}?")
    );
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the table", |app, cx| {
        !app.has(cx, "sql-preview") && app.outcomes(cx).len() == 1 && !app.running(cx)
    });

    // Local asks only for what destroys: the plain title, no band.
    let app = connected(cx, Engine::Postgres);
    app.run_in_new_tab(
        cx,
        "drop table if exists kerno_ui_v4_confirm_none",
        "secondary-enter",
    );
    assert!(app.has(cx, "sql-preview"));
    assert_eq!(app.label_of(cx, "sql-title"), "Run this statement?");
    assert!(!app.has(cx, "sql-environment"));
    app.click(cx, "sql-cancel");
}

#[gpui_kit::test]
fn a_failed_connection_shows_its_whole_message_in_details(cx: &mut TestAppContext) {
    // Port 1 of this machine: nothing listens there, the refusal is quick.
    let config = ConnectionConfig {
        port: 1,
        ..testing::config(Engine::Postgres)
    };
    let storage = memory_storage();
    storage.save_connection(None, &config).unwrap();
    let app = open(cx, storage);
    app.open_connection_row(cx, 0);
    app.wait_until(cx, "the failure", |app, cx| {
        matches!(app.status(cx, 0), Status::Failed { .. })
    });
    let Status::Failed { message } = app.status(cx, 0) else {
        unreachable!()
    };
    let explained = kerno_core::error::explain(&message, &config);

    // Beside "Try again", "Details…": the three parts whole, with Copy.
    assert!(app.has(cx, ("connection-retry", 0usize)));
    app.click(cx, ("connection-details", 0usize));
    assert!(app.has(cx, "connection-failure-dialog"));
    for id in [
        "connection-failure-headline",
        "connection-failure-next",
        "connection-failure-raw",
    ] {
        assert!(app.has(cx, id), "{id}");
    }
    for part in [&explained.headline, &explained.next_step, &explained.raw] {
        let line = part.lines().next().unwrap();
        assert!(super::tool_wizards::paints(&app, cx, line), "{line}");
    }
    app.click(cx, "connection-failure-copy");
    let copied = cx
        .read_from_clipboard()
        .and_then(|item| item.text())
        .unwrap_or_default();
    assert_eq!(
        copied,
        format!(
            "{}\n\n{}\n\n{}",
            explained.headline, explained.next_step, explained.raw
        )
    );

    // "Edit connection…" closes it and opens the connection's form.
    app.click(cx, "connection-failure-edit");
    app.wait_until(cx, "the connection's form", |app, cx| {
        app.has(cx, "conn-test") && !app.has(cx, "connection-failure-dialog")
    });
}

#[gpui_kit::test]
fn the_local_client_panel_says_what_the_server_asks_and_disables_older_clients(
    cx: &mut TestAppContext,
) {
    if !super::tool_wizards::fake_tools_run() {
        return;
    }
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let old = fake_client(
        &root,
        "pg9",
        ClientFamily::Postgres,
        "pg_dump (PostgreSQL) 9.6.24",
        "exit 0",
    );
    cx.update(|cx| AppState::update_settings(cx, |s| s.client_folders.push(old.clone())));
    let new = pg_client(&app, cx, &root, "pg99", "99.0");
    let dialog = app.backup_dialog(cx, "kerno", false).expect("the wizard");
    app.wait_backup(cx, &dialog, &["ready"]);
    let major: u32 = on_server(Engine::Postgres, "SHOW server_version_num")
        .parse::<u32>()
        .unwrap()
        / 10_000;

    app.tap(cx, "backup-local-client");
    assert_eq!(cx.update(|cx| dialog.read(cx).page()), "Local client");
    // One line says what the server asks, once.
    assert_eq!(
        app.label_of(cx, "client-server"),
        format!("Server: PostgreSQL {major}. pg_dump must be {major} or newer.")
    );
    assert!(app.has(cx, "client-list"));
    let rows = cx.update(|cx| dialog.read(cx).client_rows(cx));
    let row = |dir: &Path| {
        let at = rows.iter().position(|(found, ..)| found == dir).unwrap();
        (at, rows[at].1, rows[at].2)
    };
    // The older client: a short note, and Use off (pg_dump would refuse).
    let (old_at, note, usable) = row(&old);
    assert_eq!((note, usable), (Some("older than the server"), false));
    let (_, note, usable) = row(&new);
    assert_eq!((note, usable), (None, true));
    for (_, note, usable) in &rows {
        assert_eq!(*usable, *note != Some("older than the server"));
    }
    // The list scrolls inside the panel: down to the older client.
    for _ in 0..40 {
        let shown = app.step(cx, |window, _| {
            window.find(("client-use", old_at)).visible()
        });
        if shown {
            break;
        }
        app.step(cx, |window, cx| {
            window.scroll(
                "client-list",
                gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(-120.))),
                cx,
            )
        });
    }
    assert!(
        app.step(cx, |window, _| window.find("client-server").visible()),
        "the line above the list stays"
    );
    app.click(cx, ("client-use", old_at));
    assert_eq!(
        cx.update(|cx| dialog.read(cx).client_in_use()),
        Some(new.clone()),
        "a disabled Use does nothing"
    );
}

#[gpui_kit::test]
fn restore_and_script_find_their_client_before_the_file_and_take_a_typed_path(
    cx: &mut TestAppContext,
) {
    if !super::tool_wizards::fake_tools_run() {
        return;
    }
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let client = pg_client(&app, cx, &root, "pg99", "99.0");
    let script = root.join("kerno_ui_v4.sql");
    std::fs::write(&script, "SELECT 1;\n").unwrap();

    // A script's tool is known: its client is found with no file chosen.
    let dialog = app
        .wizard(cx, "kerno", Mode::ExecuteScript)
        .expect("the script wizard");
    app.wait_until(cx, "the script's client", |app, cx| {
        tool_line(app, cx).ends_with(client.join("psql").to_str().unwrap())
    });
    assert_eq!(app.wizard_state(cx, &dialog), "choose a file");
    app.tap(cx, "backup-close");

    // A restore's tool follows the file's format: it says so.
    let dialog: Entity<BackupDialog> = app
        .wizard(cx, "kerno", Mode::Restore)
        .expect("the restore wizard");
    app.wait_until(cx, "the restore's client line", |app, cx| {
        tool_line(app, cx).contains("Choose the file first")
    });
    assert!(!tool_line(&app, cx).contains("Looking for"));
    // A path typed in the field is read on Enter.
    app.tap(cx, "restore-file");
    app.type_text(cx, script.to_str().unwrap());
    app.press(cx, &["enter"]);
    let state = app.wait_backup(cx, &dialog, &["ready", "file", "no tool", "old client"]);
    assert!(
        state.starts_with("ready: ") && state.contains("psql"),
        "{state}"
    );
    assert_eq!(
        cx.update(|cx| dialog.read(cx).notes().1).as_deref(),
        Some("SQL script, run by psql")
    );
}

#[gpui_kit::test]
fn if_exists_waits_for_drop_statements_and_the_global_command_shows_whole(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    pg_client(&app, cx, &root, "pg99", "99.0");
    let dialog = app.backup_dialog(cx, "kerno", false).expect("the wizard");
    app.wait_backup(cx, &dialog, &["ready"]);
    app.to_settings(cx, &dialog);
    let if_exists = |cx: &mut TestAppContext| match cx
        .update(|cx| dialog.read(cx).settings(cx))
        .unwrap()
        .engine
    {
        EngineBackup::Postgres(pg) => pg.if_exists,
        EngineBackup::MySql(_) => unreachable!(),
    };
    // Indented under "Add DROP statements", and off until it is ticked.
    let x = |app: &Fixture, cx: &mut TestAppContext, id: &'static str| {
        app.step(cx, |window, _| window.find(id).bounds().origin.x)
    };
    app.tap(cx, "backup-if-exists");
    assert!(x(&app, cx, "backup-if-exists") > x(&app, cx, "backup-clean"));
    assert!(!if_exists(cx), "disabled while DROP statements are off");
    app.tap(cx, "backup-clean");
    app.tap(cx, "backup-if-exists");
    assert!(if_exists(cx));
    app.tap(cx, "backup-clean");
    assert!(!if_exists(cx));
    app.tap(cx, "backup-close");

    // The global backup's command is never squeezed below its text: it
    // shows whole, or scrolls inside its bound (110 px), and the page
    // scrolls to it.
    let dialog = app
        .wizard(cx, "kerno", Mode::GlobalBackup)
        .expect("the global backup");
    app.wait_backup(cx, &dialog, &["ready"]);
    app.to_settings(cx, &dialog);
    // A long command: the arguments wrap over more lines than the box holds
    // when squeezed.
    let folder = root
        .join("kerno_ui_v4_a_rather_long_folder_name_for_the_command")
        .join("and_another_level_of_it_to_make_the_path_wrap")
        .join("and_a_third_one_to_be_sure_it_takes_more_lines");
    std::fs::create_dir_all(&folder).unwrap();
    app.write_to(cx, &folder, None);
    app.tap(cx, "backup-command");
    let (shown, text, padding, visible) = app.step(cx, |window, _| {
        let command = window.find("backup-command");
        let text = window.find("backup-command-text").bounds();
        (
            command.bounds().size.height,
            text.size.height,
            text.origin.y - command.bounds().origin.y,
            command.visible(),
        )
    });
    assert!(visible);
    // Whole: the text and the box's padding above and below it (K65: read
    // from the layout, 7.5 px at the 15 px of the tests' text, not assumed;
    // and Linux's short temporary folder gives a command of 5 lines, which
    // the box holds whole, where macOS's long one reaches the bound).
    assert!(
        shown >= px(108.) || shown >= text + padding * 2.,
        "the command box is {shown:?} high for {text:?} of text and {padding:?} of padding"
    );
}

#[gpui_kit::test]
fn a_database_without_a_wrapper_says_to_install_one_first(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let database = "kerno_ui_v4_fdw";
    let _scratch = Scratch::new(
        Engine::Postgres,
        &["DROP DATABASE IF EXISTS kerno_ui_v4_fdw WITH (FORCE)"],
        &["CREATE DATABASE kerno_ui_v4_fdw"],
    );
    let app = connected(cx, Engine::Postgres);
    let target = app.target(cx, Some(database));
    let connections = app.connections.clone();
    let dialog = app
        .step(cx, |window, cx| {
            crate::fdw_dialog::open(connections, target, window, cx)
        })
        .expect("the dialog");
    app.wait_until(cx, "the wrappers", |_, cx| {
        cx.update(|cx| dialog.read(cx).listing()).is_some()
    });
    assert!(
        cx.update(|cx| dialog.read(cx).listing())
            .unwrap()
            .wrappers
            .is_empty()
    );
    assert!(app.has(cx, "fdw-no-wrapper"));
    // The form is off: typing into it changes nothing, Create asks nothing.
    app.click(cx, "fdw-server");
    app.type_text(cx, "kerno_ui_v4_server");
    assert_eq!(cx.update(|cx| dialog.read(cx).form(cx).server), "");
    app.click(cx, "fdw-create");
    assert!(!app.has(cx, "sql-preview"));
    app.click(cx, "fdw-close");
}

#[gpui_kit::test]
fn the_settings_keep_one_size_from_section_to_section(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.dispatch(cx, actions::OpenSettings);
    let mut sizes = Vec::new();
    for index in 0..crate::settings_view::SECTIONS.len() {
        app.click(cx, ("settings-section", index));
        sizes.push(app.step(cx, |window, _| window.find("settings").bounds()));
    }
    assert!(sizes.iter().all(|bounds| *bounds == sizes[0]), "{sizes:#?}");
}
