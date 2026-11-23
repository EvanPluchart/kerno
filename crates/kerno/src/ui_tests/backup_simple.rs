//! K26: a backup and a restore of a database open in a simple view; the
//! wizard is its "Advanced settings…" (docs/plans/M12.md, D-099).

use std::path::Path;

use gpui_kit::Pixels;
use kerno_core::tools::{BackupFormat, ClientFamily, EngineBackup};

use super::tool_wizards::{fake_client, paints};
use super::*;
use crate::backup_dialog::{BackupDialog, Mode, Purpose};
use crate::navigator::actions::ToolEntry;

fn view(cx: &mut TestAppContext, dialog: &Entity<BackupDialog>) -> &'static str {
    cx.update(|cx| dialog.read(cx).view_name())
}

/// The command the dialog would run: its program and arguments, the
/// file's argument left out (its name holds the minute it was stamped).
fn command_args(cx: &mut TestAppContext, dialog: &Entity<BackupDialog>) -> Vec<String> {
    let command = cx
        .update(|cx| dialog.read(cx).pending_command(cx))
        .expect("a command");
    std::iter::once(command.program.display().to_string())
        .chain(
            command
                .args
                .into_iter()
                .filter(|arg| !arg.starts_with("--file=") && !arg.starts_with("--result-file=")),
        )
        .collect()
}

/// The file a backup would write, its stamp masked.
fn masked_file(cx: &mut TestAppContext, dialog: &Entity<BackupDialog>) -> String {
    let file = cx
        .update(|cx| dialog.read(cx).file_preview(cx))
        .expect("a file")
        .display()
        .to_string();
    file.chars()
        .map(|c| if c.is_ascii_digit() { '#' } else { c })
        .collect()
}

fn bounds(app: &Fixture, cx: &mut TestAppContext, id: &'static str) -> Bounds<Pixels> {
    app.step(cx, |window, _| window.find(id).bounds())
}

impl Fixture {
    /// Answers the save panel a click on `id` opens with `path`.
    fn save_as(&self, cx: &mut TestAppContext, id: &'static str, path: &Path) {
        self.tap(cx, id);
        let path = path.to_path_buf();
        cx.simulate_new_path_selection(move |_| Some(path));
        cx.run_until_parked();
    }
}

/// The simple view of each engine: the whole database in one sentence, the
/// format the engine's tool restores best (PostgreSQL's custom archive, a
/// script with procedures, functions and triggers for MySQL and MariaDB), the
/// same file name and command the wizard builds with its defaults, and the
/// command shown on demand without the password.
#[gpui_kit::test]
fn a_backup_opens_simple_with_each_engines_defaults(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    for engine in ENGINES {
        if !testing::available(engine) {
            continue;
        }
        let app = connected(cx, engine);
        let (family, version) = match engine {
            Engine::Postgres => (ClientFamily::Postgres, "pg_dump (PostgreSQL) 99.0"),
            _ => (
                ClientFamily::MySql,
                "mysqldump  Ver 9.4.0 for macos15 on arm64",
            ),
        };
        let client = fake_client(&root, &format!("{engine:?}"), family, version, "exit 0");
        app.use_client(cx, family, &client);
        let dialog = app
            .simple_dialog(cx, "kerno_other", false)
            .expect("the backup");
        app.wait_backup(cx, &dialog, &["ready"]);
        app.wait_until(cx, "the tables counted", |app, cx| {
            paints(app, cx, "The whole database “kerno_other”: ")
        });
        assert_eq!(view(cx, &dialog), "simple", "{engine:?}");
        assert!(
            app.has(cx, "backup-simple") && !app.has(cx, ("wizard-step", 0usize)),
            "{engine:?}"
        );
        // The intro of the wizard stays: the dialog says what it is first.
        assert!(paints(&app, cx, "Back up the database “kerno_other”"));
        let args = command_args(cx, &dialog);
        let joined = args.join(" ");
        match engine {
            Engine::Postgres => {
                assert!(app.has(cx, crate::settings_ui::check_id("simple-format", 0)));
                assert!(joined.contains("--format=c"), "{joined}");
                assert!(paints(&app, cx, "pg_dump's custom archive"));
            }
            _ => {
                assert!(!app.has(cx, "simple-format"), "{engine:?}: SQL only");
                for part in ["--single-transaction", "--routines"] {
                    assert!(joined.contains(part), "{engine:?}: {part}: {joined}");
                }
                assert!(!joined.contains("--skip-triggers") && !joined.contains("--events"));
                assert!(paints(
                    &app,
                    cx,
                    "read in one consistent snapshot (single transaction). It holds the tables \
                     and their rows, the procedures and functions, the triggers."
                ));
            }
        }
        // The file shown is the one written.
        let file = cx
            .update(|cx| dialog.read(cx).file_preview(cx))
            .expect("a file");
        assert!(paints(&app, cx, &file.display().to_string()), "{engine:?}");
        // The command, on demand: the password never in it.
        assert!(!app.has(cx, "backup-command-text"));
        app.tap(cx, "simple-show-command");
        let shown = cx
            .update(|cx| dialog.read(cx).pending_command(cx))
            .unwrap()
            .display();
        assert!(app.has(cx, "backup-command-text") && paints(&app, cx, &shown));
        assert!(
            !shown.contains(testing::DB_PASSWORD) && shown.contains("•••"),
            "{shown}"
        );
        let simple = (masked_file(cx, &dialog), args);
        app.tap(cx, "backup-close");

        // The wizard with its defaults writes the same name with the same
        // command.
        let wizard = app
            .backup_dialog(cx, "kerno_other", false)
            .expect("the wizard");
        app.wait_backup(cx, &wizard, &["ready"]);
        assert_eq!(view(cx, &wizard), "advanced");
        assert_eq!(
            simple,
            (masked_file(cx, &wizard), command_args(cx, &wizard)),
            "{engine:?}"
        );
        app.tap(cx, "backup-close");
    }
}

/// "Choose…" takes the file from the save panel; "Advanced settings…" opens
/// the wizard on it and on its format; the way back is free while nothing
/// the simple view hides was changed, and otherwise asks, naming it.
#[gpui_kit::test]
fn the_wizard_keeps_the_simple_choices_and_the_way_back_asks(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let app = connected(cx, Engine::Postgres);
    let client = fake_client(
        &root,
        "pg",
        ClientFamily::Postgres,
        "pg_dump (PostgreSQL) 99.0",
        "exit 0",
    );
    app.use_client(cx, ClientFamily::Postgres, &client);
    let dialog = app
        .simple_dialog(cx, "kerno_other", false)
        .expect("the backup");
    app.wait_backup(cx, &dialog, &["ready"]);

    // A name ending in .sql chooses the script; its folder is kept.
    app.save_as(cx, "simple-choose-file", &root.join("mine.sql"));
    let file = |cx: &mut TestAppContext| cx.update(|cx| dialog.read(cx).file_preview(cx));
    assert_eq!(file(cx), Ok(root.join("mine.sql")));
    assert!(app.has(cx, crate::settings_ui::check_id("simple-format", 1)));
    // The archive again: the same name, its own extension.
    app.step(cx, |window, cx| {
        window.within("simple-format").click(0usize, cx)
    });
    assert_eq!(file(cx), Ok(root.join("mine.dump")));
    app.step(cx, |window, cx| {
        window.within("simple-format").click(1usize, cx)
    });

    app.tap(cx, "backup-advanced");
    assert_eq!(view(cx, &dialog), "advanced");
    assert_eq!(cx.update(|cx| dialog.read(cx).page()), "Objects");
    app.to_settings(cx, &dialog);
    assert_eq!(file(cx), Ok(root.join("mine.sql")));
    assert_eq!(
        cx.update(|cx| dialog.read(cx).settings(cx)).unwrap().format,
        BackupFormat::Sql
    );

    // Nothing hidden changed: back at once.
    app.tap(cx, "wizard-simple");
    assert_eq!(view(cx, &dialog), "simple");
    assert!(!app.has(cx, "simple-reset"));

    // A hidden option: the way back says which and asks.
    app.tap(cx, "backup-advanced");
    app.to_settings(cx, &dialog);
    app.tap(cx, "backup-no-owner");
    app.replace_text(cx, "backup-extra-args", "--no-comments");
    app.tap(cx, "wizard-simple");
    assert!(app.has(cx, "simple-reset"));
    assert!(paints(
        &app,
        cx,
        "The simple view does not show the dump tool's options and the extra arguments."
    ));
    app.tap(cx, "simple-stay");
    assert_eq!(view(cx, &dialog), "advanced");
    assert!(
        cx.update(|cx| dialog.read(cx).pending_command(cx))
            .unwrap()
            .args
            .contains(&"--no-owner".to_string())
    );
    app.tap(cx, "wizard-simple");
    app.tap(cx, "simple-reset");
    assert_eq!(view(cx, &dialog), "simple");
    let args = command_args(cx, &dialog);
    assert!(
        !args.contains(&"--no-owner".to_string()) && !args.contains(&"--no-comments".to_string()),
        "{args:?}"
    );
    assert_eq!(file(cx), Ok(root.join("mine.sql")), "the file stays");

    // The objects page, then the way back: the objects are named too.
    app.tap(cx, "backup-advanced");
    app.wait_until(cx, "the objects", |_, cx| {
        cx.update(|cx| dialog.read(cx).objects_loaded())
    });
    app.tap(cx, "backup-complete");
    app.tap(cx, "wizard-simple");
    assert!(paints(
        &app,
        cx,
        "The simple view does not show the objects ticked."
    ));
    app.tap(cx, "simple-reset");
    assert_eq!(view(cx, &dialog), "simple");
    assert!(
        cx.update(|cx| dialog.read(cx).settings(cx))
            .unwrap()
            .schemas
            .is_empty()
    );
}

/// What carries explicit settings opens in the wizard: Tools ▸ Back up… on a
/// table or a schema, a new task, a saved task; the global backup and Execute
/// script have no simple view. A database's backup and restore are simple,
/// also from the toolbar on a tab bound to a schema.
#[gpui_kit::test]
fn explicit_settings_and_tasks_open_the_wizard(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    let opened = |app: &Fixture, cx: &mut TestAppContext| {
        cx.update(|cx| app.workspace.read(cx).opened_wizard.clone())
            .and_then(|wizard| wizard.upgrade())
            .expect("the wizard")
    };
    let target = app.target(cx, Some("kerno"));
    let cases: [(&str, Mode, Vec<TableRef>, Option<&str>); 4] = [
        (
            "a table",
            Mode::Backup,
            vec![TableRef::new("shop", "orders")],
            None,
        ),
        ("a schema", Mode::Backup, Vec::new(), Some("shop")),
        ("the database", Mode::Backup, Vec::new(), None),
        ("a restore", Mode::Restore, Vec::new(), None),
    ];
    for (what, mode, tables, schema) in cases {
        let target = Target {
            schema: schema.map(str::to_string),
            ..target.clone()
        };
        let entry = match mode {
            Mode::Restore => ToolEntry::Restore,
            _ => ToolEntry::Backup,
        };
        app.step(cx, |window, cx| {
            app.workspace.update(cx, |workspace, cx| {
                workspace.run_tool(target, entry, tables, window, cx)
            })
        });
        let dialog = opened(&app, cx);
        let expected = if what == "a table" || what == "a schema" {
            "advanced"
        } else {
            "simple"
        };
        assert_eq!(view(cx, &dialog), expected, "{what}");
        app.press(cx, &["escape"]);
    }
    for mode in [Mode::GlobalBackup, Mode::ExecuteScript] {
        let dialog = app.wizard(cx, "kerno", mode).expect("the wizard");
        assert_eq!(view(cx, &dialog), "advanced", "{mode:?}");
        app.press(cx, &["escape"]);
    }

    // A new task, then a saved one: the wizard, without "Simple view".
    let dialog = app.simple_dialog(cx, "kerno", false).expect("the backup");
    cx.update(|cx| {
        dialog.update(cx, |dialog, cx| {
            dialog.set_purpose(Purpose::NewTask, |_| {}, cx)
        })
    });
    assert_eq!(view(cx, &dialog), "advanced");
    app.wait_backup(cx, &dialog, &["ready", "no tool", "old client"]);
    assert!(!app.has(cx, "wizard-simple"));
    app.press(cx, &["escape"]);
    let dialog = app.simple_dialog(cx, "kerno", false).expect("the backup");
    let settings = kerno_core::tools::TaskSettings::Backup(kerno_core::tools::BackupOptions::new(
        Engine::Postgres,
        "kerno",
    ));
    let task = kerno_core::storage::SavedTask::new("nightly", "c", settings.clone());
    app.step(cx, |window, cx| {
        dialog.update(cx, |dialog, cx| {
            dialog.apply(settings, window, cx);
            dialog.set_purpose(Purpose::Task(Box::new(task)), |_| {}, cx);
        })
    });
    assert_eq!(view(cx, &dialog), "advanced");
    app.press(cx, &["escape"]);

    // The toolbar's Tools ▸ Back up… on a tab bound to a schema: the whole
    // database, simple.
    let connection = target.connection.clone();
    app.step(cx, |window, cx| {
        app.workspace.update(cx, |workspace, cx| {
            workspace.open_query(
                "Query 1".into(),
                "",
                Some(Target {
                    connection,
                    database: Some("kerno".into()),
                    schema: Some("shop".into()),
                }),
                window,
                cx,
            );
        })
    });
    app.click(cx, "toolbar-tools");
    click_menu_entry(&app, cx, "Back up…");
    let dialog = opened(&app, cx);
    assert_eq!(view(cx, &dialog), "simple");
    app.wait_backup(cx, &dialog, &["ready", "no tool", "old client"]);
    assert!(
        cx.update(|cx| dialog.read(cx).settings(cx))
            .unwrap()
            .schemas
            .is_empty()
    );
    app.press(cx, &["escape"]);
}

/// A real backup from the simple view writes a file the simple restore reads
/// back into a scratch database, on PostgreSQL and MySQL, when the client
/// tools are installed. Enter starts the backup, never the restore.
#[gpui_kit::test]
fn a_simple_backup_is_restored_by_the_simple_restore(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            continue;
        }
        let (source, target) = ("kerno_test_k26_src", "kerno_test_k26_dst");
        let drop = |name: &str| match engine {
            Engine::Postgres => format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"),
            _ => format!("DROP DATABASE IF EXISTS {name}"),
        };
        let _scratch = Scratch::new(
            engine,
            &[&drop(source), &drop(target)],
            &[
                &format!("CREATE DATABASE {source}"),
                &format!("CREATE DATABASE {target}"),
            ],
        );
        match engine {
            Engine::Postgres => {
                on_database(
                    engine,
                    Some(source),
                    "CREATE TABLE kept (id int); INSERT INTO kept VALUES (1), (2), (3)",
                );
            }
            _ => {
                on_server(engine, &format!("CREATE TABLE {source}.kept (id int)"));
                on_server(
                    engine,
                    &format!("INSERT INTO {source}.kept VALUES (1), (2), (3)"),
                );
            }
        }
        let app = connected(cx, engine);
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();

        let dialog = app.simple_dialog(cx, source, false).expect("the backup");
        let state = app.wait_backup(cx, &dialog, &["ready", "no tool", "old client"]);
        if !state.starts_with("ready") {
            eprintln!("SKIPPED ({engine:?}): no usable client tool: {state}");
            continue;
        }
        app.save_as(cx, "simple-choose-file", &root.join("k26"));
        // Enter is the simple backup's Back up.
        app.press(cx, &["enter"]);
        let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
        assert!(state.starts_with("done: "), "{engine:?}: {state}");
        let file = cx.update(|cx| dialog.read(cx).written()).expect("a file");
        assert!(file.exists() && file.starts_with(&root), "{file:?}");
        assert!(app.has(cx, "backup-reveal") && app.has(cx, "backup-log"));
        // The folder is proposed again next time.
        assert_eq!(
            cx.update(|cx| AppState::get(cx).settings.backup_folder.clone()),
            Some(root.clone())
        );
        app.tap(cx, "backup-close");

        // The same file chosen again in the macOS save panel, which asks before replacing
        // it: Kerno does not ask a second time.
        if cfg!(target_os = "macos") {
            let dialog = app.simple_dialog(cx, source, false).expect("the backup");
            app.wait_backup(cx, &dialog, &["ready"]);
            app.save_as(cx, "simple-choose-file", &file);
            app.press(cx, &["enter"]);
            assert!(!app.has(cx, "sql-preview"), "{engine:?}");
            let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
            assert!(state.starts_with("done: "), "{engine:?}: {state}");
            app.tap(cx, "backup-close");
        }

        let dialog = app.simple_dialog(cx, target, true).expect("the restore");
        app.wait_backup(cx, &dialog, &["choose a file"]);
        assert_eq!(view(cx, &dialog), "simple");
        assert!(paints(
            &app,
            cx,
            &format!("A restore writes into “{target}”")
        ));
        app.tap(cx, "restore-choose-file");
        app.answer_open_prompt(cx, &file);
        let state = app.wait_backup(cx, &dialog, &["ready", "refused", "no tool", "file"]);
        assert!(state.starts_with("ready: "), "{engine:?}: {state}");
        if engine == Engine::Postgres {
            assert!(paints(&app, cx, "Custom archive, restored by pg_restore"));
        }
        // A stray Enter starts nothing.
        app.press(cx, &["enter"]);
        assert!(!app.has(cx, "sql-preview"), "{engine:?}");
        assert!(app.wizard_state(cx, &dialog).starts_with("ready: "));
        app.tap(cx, "restore-start");
        app.wait_until(cx, "the confirmation", |app, cx| app.has(cx, "sql-preview"));
        app.tap(cx, "sql-confirm");
        let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
        assert!(state.starts_with("done: "), "{engine:?}: {state}");
        let count = match engine {
            Engine::Postgres => on_database(engine, Some(target), "SELECT count(*) FROM kept"),
            _ => on_server(engine, &format!("SELECT count(*) FROM {target}.kept")),
        };
        assert_eq!(count, "3", "{engine:?}");
        app.tap(cx, "backup-close");
    }
}

/// The simple restore keeps the environment's rules: production asks twice,
/// a read-only connection has none; an old client is said and stops Back
/// up; a file's client commands are refused, the way to allow them named.
#[gpui_kit::test]
fn the_simple_views_keep_every_guard(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let ran = root.join("ran");
    let client = fake_client(
        &root,
        "client",
        ClientFamily::Postgres,
        "psql (PostgreSQL) 99.0",
        &format!("touch {}\necho restored", ran.display()),
    );
    let file = root.join("clean.sql");
    std::fs::write(&file, "SELECT 1;\n").unwrap();
    let hostile = root.join("hostile.sql");
    std::fs::write(&hostile, "SELECT 1;\n\\! echo hi\n").unwrap();

    let mut config = testing::config(Engine::Postgres);
    config.read_only = true;
    let app = connected_as(cx, config);
    assert!(app.simple_dialog(cx, "kerno_other", true).is_none());
    assert!(!app.has(cx, "backup-dialog"));

    let mut config = testing::config(Engine::Postgres);
    config.environment = kerno_core::environment::PRODUCTION.into();
    let app = connected_as(cx, config);
    app.use_client(cx, ClientFamily::Postgres, &client);
    let dialog = app
        .simple_dialog(cx, "kerno_ui_restore_never", true)
        .expect("the restore");
    app.wait_backup(cx, &dialog, &["choose a file"]);
    app.tap(cx, "restore-choose-file");
    app.answer_open_prompt(cx, &hostile);
    assert_eq!(
        app.wait_backup(cx, &dialog, &["refused", "ready", "file"]),
        "refused: line 2: \\!"
    );
    assert!(app.has(cx, "restore-refusal") && paints(&app, cx, "open Advanced settings…"));
    app.tap(cx, "restore-start");
    assert!(!app.has(cx, "sql-preview"), "a refused file is not offered");
    app.tap(cx, "restore-choose-file");
    app.answer_open_prompt(cx, &file);
    app.wait_backup(cx, &dialog, &["ready"]);
    app.tap(cx, "restore-start");
    assert!(app.has(cx, "sql-preview") && app.has(cx, "sql-production"));
    app.tap(cx, "sql-confirm");
    assert!(app.has(cx, "sql-preview") && !ran.exists(), "armed only");
    app.tap(cx, "sql-confirm");
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    assert!(state.starts_with("done: psql finished"), "{state}");
    assert!(ran.exists());
    app.tap(cx, "backup-close");

    // A client older than the server: said in the simple view, Back up off.
    let old = fake_client(
        &root,
        "old",
        ClientFamily::Postgres,
        "pg_dump (PostgreSQL) 9.6.24",
        "exit 0",
    );
    app.use_client(cx, ClientFamily::Postgres, &old);
    let dialog = app
        .simple_dialog(cx, "kerno_other", false)
        .expect("the backup");
    let state = app.wait_backup(cx, &dialog, &["old client", "ready"]);
    assert!(state.starts_with("old client"), "{state}");
    assert!(app.has(cx, "backup-client-warning") && paints(&app, cx, "older than the server"));
    app.tap(cx, "backup-start");
    app.press(cx, &["enter"]);
    assert!(app.wizard_state(cx, &dialog).starts_with("old client"));
    // Its way out: "Local client…", from the simple view.
    app.tap(cx, "backup-local-client");
    assert_eq!(cx.update(|cx| dialog.read(cx).page()), "Local client");
}

/// At 900 × 600 the simple views and the wizard keep to the window: the
/// buttons inside the dialog, the page above them.
#[gpui_kit::test]
fn the_backup_dialogs_fit_a_small_window(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let client = fake_client(
        &root,
        "pg",
        ClientFamily::Postgres,
        "pg_dump (PostgreSQL) 99.0",
        "exit 0",
    );
    let archive = root.join("archive.sql");
    std::fs::write(&archive, "SELECT 1;\n").unwrap();
    let app = connected(cx, Engine::Postgres);
    app.use_client(cx, ClientFamily::Postgres, &client);
    cx.simulate_window_resize(app.window.into(), size(px(900.), px(600.)));
    let fits = |app: &Fixture, cx: &mut TestAppContext, buttons: &[&'static str], at: &str| {
        let surface = app.step(cx, |window, _| window.find("surface").bounds());
        assert!(
            surface.top() >= px(0.) && surface.bottom() <= px(600.) && surface.right() <= px(900.),
            "{at}: {surface:?}"
        );
        let page = bounds(app, cx, "wizard-page");
        for id in buttons {
            let button = bounds(app, cx, id);
            assert!(
                button.bottom() <= surface.bottom() && button.top() >= page.bottom(),
                "{at}: {id} {button:?} under {page:?} in {surface:?}"
            );
        }
        assert!(page.right() <= surface.right(), "{at}");
    };

    let dialog = app
        .simple_dialog(cx, "kerno_other", false)
        .expect("the backup");
    app.wait_backup(cx, &dialog, &["ready"]);
    app.tap(cx, "simple-show-command");
    fits(
        &app,
        cx,
        &["backup-advanced", "backup-close", "backup-start"],
        "simple backup",
    );
    app.tap(cx, "backup-advanced");
    app.to_settings(cx, &dialog);
    fits(
        &app,
        cx,
        &["backup-local-client", "wizard-back", "backup-start"],
        "wizard",
    );
    app.press(cx, &["escape"]);

    let dialog = app
        .simple_dialog(cx, "kerno_other", true)
        .expect("the restore");
    app.wait_backup(cx, &dialog, &["choose a file"]);
    app.tap(cx, "restore-choose-file");
    app.answer_open_prompt(cx, &archive);
    app.wait_backup(cx, &dialog, &["ready"]);
    app.tap(cx, "simple-show-command");
    fits(
        &app,
        cx,
        &["backup-advanced", "backup-close", "restore-start"],
        "simple restore",
    );
    app.press(cx, &["escape"]);
}

/// The arguments of the dialog's command, joined.
fn joined(cx: &mut TestAppContext, dialog: &Entity<BackupDialog>) -> String {
    command_args(cx, dialog).join(" ")
}

/// K48: the simple backup's "Faster" makes a directory archive on several
/// connections, a folder; the wizard shows the same state and the way back
/// does not call it a hidden change; other jobs are one; unticking, or the
/// script, gives the single file back.
#[gpui_kit::test]
fn faster_backs_up_into_a_folder_and_the_wizard_shows_it(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let app = connected(cx, Engine::Postgres);
    let client = fake_client(
        &root,
        "pg",
        ClientFamily::Postgres,
        "pg_dump (PostgreSQL) 99.0",
        "exit 0",
    );
    app.use_client(cx, ClientFamily::Postgres, &client);
    let dialog = app
        .simple_dialog(cx, "kerno_other", false)
        .expect("the backup");
    app.wait_backup(cx, &dialog, &["ready"]);
    let jobs = crate::backup_dialog::faster_jobs();
    let file = |cx: &mut TestAppContext| cx.update(|cx| dialog.read(cx).file_preview(cx));

    // Off by default: the archive, one file.
    assert!(app.has(cx, "simple-faster"));
    let args = joined(cx, &dialog);
    assert!(
        args.contains("--format=c") && !args.contains("--jobs"),
        "{args}"
    );
    app.save_as(cx, "simple-choose-file", &root.join("mine.dump"));
    assert_eq!(file(cx), Ok(root.join("mine.dump")));

    // Ticked: a directory archive on that many connections, into a folder.
    app.tap(cx, "simple-faster");
    let args = joined(cx, &dialog);
    assert!(
        args.contains(&format!("--format=d --jobs={jobs}")),
        "{args}"
    );
    assert_eq!(file(cx), Ok(root.join("mine")));
    assert!(paints(
        &app,
        cx,
        &format!("{}/", root.join("mine").display())
    ));
    assert!(paints(&app, cx, "pg_dump's directory archive: a folder"));
    assert!(paints(
        &app,
        cx,
        "The backup is then a folder, one file per table, which Kerno restores like any backup."
    ));
    assert!(
        app.has(cx, crate::settings_ui::check_id("simple-format", 0)),
        "still the archive"
    );
    // The save panel names the folder, as typed: an extension does not
    // choose the format.
    app.save_as(cx, "simple-choose-file", &root.join("nightly.dump"));
    assert_eq!(file(cx), Ok(root.join("nightly.dump")));
    assert!(joined(cx, &dialog).contains("--format=d"));
    // The archive's card again: still faster.
    app.step(cx, |window, cx| {
        window.within("simple-format").click(0usize, cx)
    });
    assert!(joined(cx, &dialog).contains(&format!("--format=d --jobs={jobs}")));

    // The wizard shows the same: the directory format and its jobs; a task
    // keeps them.
    app.tap(cx, "backup-advanced");
    app.to_settings(cx, &dialog);
    assert!(app.has(cx, "backup-jobs"));
    let settings = cx.update(|cx| dialog.read(cx).settings(cx)).unwrap();
    assert_eq!(settings.format, BackupFormat::Directory);
    assert!(
        matches!(&settings.engine, EngineBackup::Postgres(pg) if pg.jobs == Some(jobs)),
        "{settings:?}"
    );
    let task = cx
        .update(|cx| dialog.read(cx).task_backup(cx))
        .expect("a task");
    assert_eq!(task.format, BackupFormat::Directory);
    assert!(matches!(&task.engine, EngineBackup::Postgres(pg) if pg.jobs == Some(jobs)));
    // Nothing the simple view hides: back at once, still faster.
    app.tap(cx, "wizard-simple");
    assert_eq!(view(cx, &dialog), "simple");
    assert!(!app.has(cx, "simple-reset"));
    assert!(joined(cx, &dialog).contains(&format!("--format=d --jobs={jobs}")));
    // A hidden option is named alone, and its reset keeps "Faster".
    app.tap(cx, "backup-advanced");
    app.to_settings(cx, &dialog);
    app.tap(cx, "backup-no-owner");
    app.tap(cx, "wizard-simple");
    assert!(paints(
        &app,
        cx,
        "The simple view does not show the dump tool's options."
    ));
    app.tap(cx, "simple-reset");
    let args = joined(cx, &dialog);
    assert!(
        args.contains(&format!("--format=d --jobs={jobs}")) && !args.contains("--no-owner"),
        "{args}"
    );

    // Other jobs typed in the wizard are a hidden change: named, and reset
    // to the archive in one file.
    app.tap(cx, "backup-advanced");
    app.to_settings(cx, &dialog);
    app.replace_text(cx, "backup-jobs", "3");
    app.tap(cx, "wizard-simple");
    assert!(paints(
        &app,
        cx,
        "The simple view does not show the format and the compression, encoding or jobs."
    ));
    app.tap(cx, "simple-reset");
    assert_eq!(view(cx, &dialog), "simple");
    let args = joined(cx, &dialog);
    assert!(
        args.contains("--format=c") && !args.contains("--jobs"),
        "{args}"
    );
    assert_eq!(file(cx), Ok(root.join("nightly.dump")));

    // Unticked: the archive in one file again.
    app.tap(cx, "simple-faster");
    assert!(joined(cx, &dialog).contains("--format=d"));
    app.tap(cx, "simple-faster");
    let args = joined(cx, &dialog);
    assert!(
        args.contains("--format=c") && !args.contains("--jobs"),
        "{args}"
    );

    // The script, chosen while ticked: one file by one connection, and no
    // Speed row; the archive again is the single file.
    app.tap(cx, "simple-faster");
    app.step(cx, |window, cx| {
        window.within("simple-format").click(1usize, cx)
    });
    let args = joined(cx, &dialog);
    assert!(
        args.contains("--format=p") && !args.contains("--jobs"),
        "{args}"
    );
    assert!(!app.has(cx, "simple-faster"));
    app.step(cx, |window, cx| {
        window.within("simple-format").click(0usize, cx)
    });
    let args = joined(cx, &dialog);
    assert!(
        args.contains("--format=c") && !args.contains("--jobs"),
        "{args}"
    );
    assert!(app.has(cx, "simple-faster"));
}

/// The simple restore's "Faster" is the simple view's own state too: the
/// wizard and back keep it, without asking.
#[gpui_kit::test]
fn the_simple_restores_faster_is_not_a_hidden_change(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let client = fake_client(
        &root,
        "pg",
        ClientFamily::Postgres,
        "pg_restore (PostgreSQL) 99.0",
        "exit 0",
    );
    let archive = root.join("archive.dump");
    std::fs::write(&archive, "PGDMP a stand-in archive").unwrap();
    let app = connected(cx, Engine::Postgres);
    app.use_client(cx, ClientFamily::Postgres, &client);
    let dialog = app
        .simple_dialog(cx, "kerno_other", true)
        .expect("the restore");
    app.wait_backup(cx, &dialog, &["choose a file"]);
    app.tap(cx, "restore-choose-file");
    app.answer_open_prompt(cx, &archive);
    app.wait_backup(cx, &dialog, &["ready"]);
    app.tap(cx, "simple-parallel");
    let jobs = format!("--jobs={}", crate::backup_dialog::faster_jobs());
    let state = app.wizard_state(cx, &dialog);
    assert!(
        state.contains(&jobs) && !state.contains("--single-transaction"),
        "{state}"
    );
    app.tap(cx, "backup-advanced");
    assert_eq!(view(cx, &dialog), "advanced");
    app.tap(cx, "wizard-simple");
    assert_eq!(view(cx, &dialog), "simple");
    assert!(!app.has(cx, "simple-reset"));
    assert!(app.wizard_state(cx, &dialog).contains(&jobs));
    // A hidden option's reset keeps it too.
    app.tap(cx, "backup-advanced");
    app.tap(cx, "restore-clean");
    app.tap(cx, "wizard-simple");
    assert!(paints(
        &app,
        cx,
        "The simple view does not show the restore options."
    ));
    app.tap(cx, "simple-reset");
    let state = app.wizard_state(cx, &dialog);
    assert!(
        state.contains(&jobs) && !state.contains("--clean") && !state.contains("--single"),
        "{state}"
    );
}

/// A faster backup that fails or is cancelled removes the folder it made;
/// a folder already there is never written into nor removed, also when it
/// appears between Start's check and the run.
#[gpui_kit::test]
fn a_faster_backup_removes_only_the_folder_it_made(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    // The stand-in makes the folder and writes into it, then fails or waits.
    let made = r#"for arg in "$@"; do case $arg in --file=*) mkdir -p "${arg#--file=}"; echo partial > "${arg#--file=}/toc.dat";; esac; done"#;
    let failing = fake_client(
        &root,
        "failing",
        ClientFamily::Postgres,
        "pg_dump (PostgreSQL) 99.0",
        &format!("{made}\necho 'pg_dump: error: a stand-in failure' >&2\nexit 1"),
    );
    let slow = fake_client(
        &root,
        "slow",
        ClientFamily::Postgres,
        "pg_dump (PostgreSQL) 99.0",
        &format!("{made}\necho 'pg_dump: started'\nexec sleep 60"),
    );
    let app = connected(cx, Engine::Postgres);
    app.use_client(cx, ClientFamily::Postgres, &failing);
    let dialog = app
        .simple_dialog(cx, "kerno_other", false)
        .expect("the backup");
    app.wait_backup(cx, &dialog, &["ready"]);
    app.tap(cx, "simple-faster");

    // A failure: the folder it made is removed, and said so.
    let failed = root.join("failed");
    app.save_as(cx, "simple-choose-file", &failed);
    app.tap(cx, "backup-start");
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    assert_eq!(
        state,
        "failed: pg_dump failed (exit status 1). The incomplete folder was removed."
    );
    assert!(!failed.exists());
    assert!(
        cx.update(|cx| dialog.read(cx).log())
            .contains(&"pg_dump: error: a stand-in failure".to_string())
    );

    // A folder already there: Back up says so and runs nothing.
    app.tap(cx, "backup-back");
    let earlier = root.join("earlier");
    std::fs::create_dir(&earlier).unwrap();
    std::fs::write(earlier.join("keep"), "an earlier backup\n").unwrap();
    app.save_as(cx, "simple-choose-file", &earlier);
    let before = notification_count(&app, cx);
    app.tap(cx, "backup-start");
    app.wait_until(cx, "the refusal", |app, cx| {
        notification_count(app, cx) == before + 1
    });
    assert!(app.wizard_state(cx, &dialog).starts_with("ready: "));
    // One that appears after that check is not the run's either: nothing
    // runs, nothing is removed.
    app.step(cx, |window, cx| {
        dialog.update(cx, |dialog, cx| dialog.run_unchecked(window, cx))
    });
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    assert!(
        state.starts_with(&format!("failed: {} already exists", earlier.display()))
            && state.ends_with("Nothing was run."),
        "{state}"
    );
    assert_eq!(
        std::fs::read_to_string(earlier.join("keep")).unwrap(),
        "an earlier backup\n"
    );
    assert!(!earlier.join("toc.dat").exists());
    app.tap(cx, "backup-close");

    // Cancelled: the folder it made is removed.
    app.use_client(cx, ClientFamily::Postgres, &slow);
    let dialog = app
        .simple_dialog(cx, "kerno_other", false)
        .expect("the backup");
    app.wait_backup(cx, &dialog, &["ready"]);
    app.tap(cx, "simple-faster");
    let cut = root.join("cut");
    app.save_as(cx, "simple-choose-file", &cut);
    app.tap(cx, "backup-start");
    app.wait_until(cx, "the first line", |_, cx| {
        cx.update(|cx| dialog.read(cx).log()) == ["pg_dump: started"]
    });
    assert!(cut.join("toc.dat").exists());
    app.tap(cx, "backup-cancel");
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    assert_eq!(
        state,
        "failed: Cancelled: pg_dump was stopped. The incomplete folder was removed."
    );
    assert!(!cut.exists());
}

/// The round trip from the simple views alone, with the real tools: a
/// faster backup of a scratch database writes a folder, said with its size;
/// the simple restore takes that folder from the open panel (which lets a
/// folder be chosen) or typed, reads it as a directory archive, and
/// restores it on several connections.
#[gpui_kit::test]
fn a_faster_backup_is_a_folder_the_simple_restore_reads_back(cx: &mut TestAppContext) {
    let engine = Engine::Postgres;
    if !testing::available(engine) {
        return;
    }
    let (source, target) = ("kerno_test_k48_src", "kerno_test_k48_dst");
    let drop = |name: &str| format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)");
    let _scratch = Scratch::new(
        engine,
        &[&drop(source), &drop(target)],
        &[
            &format!("CREATE DATABASE {source}"),
            &format!("CREATE DATABASE {target}"),
        ],
    );
    on_database(
        engine,
        Some(source),
        "CREATE TABLE one (id int); CREATE TABLE two (id int); CREATE TABLE three (id int); \
         INSERT INTO one SELECT generate_series(1, 1000); \
         INSERT INTO two SELECT generate_series(1, 200); INSERT INTO three VALUES (7)",
    );
    let app = connected(cx, engine);
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let jobs = format!("--jobs={}", crate::backup_dialog::faster_jobs());

    let dialog = app.simple_dialog(cx, source, false).expect("the backup");
    let state = app.wait_backup(cx, &dialog, &["ready", "no tool", "old client"]);
    if !state.starts_with("ready") {
        eprintln!("SKIPPED: no usable client tool: {state}");
        return;
    }
    app.tap(cx, "simple-faster");
    let folder = root.join("k48");
    app.save_as(cx, "simple-choose-file", &folder);
    assert!(app.wizard_state(cx, &dialog).contains(&jobs));
    app.tap(cx, "backup-start");
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    assert!(state.starts_with("done: pg_dump finished"), "{state}");
    assert_eq!(
        cx.update(|cx| dialog.read(cx).written()),
        Some(folder.clone())
    );
    assert!(folder.join("toc.dat").is_file());
    let size: u64 = std::fs::read_dir(&folder)
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .sum();
    assert!(
        state.ends_with(&format!(
            "to {} ({}).",
            folder.display(),
            kerno_core::format::byte_size(size)
        )),
        "{state}"
    );
    assert!(app.has(cx, "backup-reveal"));
    app.tap(cx, "backup-close");

    // Restored from the open panel, which offers folders.
    let dialog = app.simple_dialog(cx, target, true).expect("the restore");
    app.wait_backup(cx, &dialog, &["choose a file"]);
    app.tap(cx, "restore-choose-file");
    let chosen = folder.clone();
    cx.simulate_path_prompt_response(move |options| {
        assert!(options.directories && options.files, "{options:?}");
        Some(vec![chosen])
    });
    cx.run_until_parked();
    let state = app.wait_backup(cx, &dialog, &["ready", "refused", "no tool", "file"]);
    assert!(state.starts_with("ready: "), "{state}");
    assert!(paints(
        &app,
        cx,
        "Directory archive, restored by pg_restore"
    ));
    app.tap(cx, "simple-parallel");
    let state = app.wizard_state(cx, &dialog);
    assert!(
        state.contains(&jobs) && !state.contains("--single-transaction"),
        "{state}"
    );
    app.tap(cx, "restore-start");
    app.wait_until(cx, "the confirmation", |app, cx| app.has(cx, "sql-preview"));
    app.tap(cx, "sql-confirm");
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    assert!(state.starts_with("done: pg_restore finished"), "{state}");
    assert_eq!(
        on_database(
            engine,
            Some(target),
            "SELECT (SELECT count(*) FROM one) || '/' || (SELECT count(*) FROM two) || '/' || \
             (SELECT id FROM three)"
        ),
        "1000/200/7"
    );
    app.tap(cx, "backup-close");

    // Typed: read the same way.
    let dialog = app.simple_dialog(cx, target, true).expect("the restore");
    app.wait_backup(cx, &dialog, &["choose a file"]);
    app.replace_text(cx, "restore-file", &folder.display().to_string());
    app.press(cx, &["enter"]);
    let state = app.wait_backup(cx, &dialog, &["ready", "refused", "no tool", "file"]);
    assert!(state.starts_with("ready: "), "{state}");
    assert!(paints(
        &app,
        cx,
        "Directory archive, restored by pg_restore"
    ));
}
