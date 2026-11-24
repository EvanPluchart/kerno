//! The tool wizards (M10 Part 11a): backup, global backup, restore and
//! execute script, their local client, and Settings ▸ Local clients.
//!
//! The runs that only show how a tool is driven use stand-in clients
//! (shell scripts in a temporary folder, added as a local client); the
//! others run the real tools installed on this computer and are skipped,
//! saying so, when none can be used.

use std::path::PathBuf;

use kerno_core::tools::ClientFamily;

use super::*;
use crate::backup_dialog::{self, BackupDialog, Mode};

/// A stand-in for a client tool: a shell script that prints `version` for
/// `--version` and otherwise runs `body`.
fn fake_tool(dir: &Path, name: &str, version: &str, body: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo '{version}'; exit 0; fi\n{body}\n"
        ),
    )
    .unwrap();
    kerno_core::private::restrict(&path, 0o755).unwrap();
    path
}

/// Whether the stand-in tools of [`fake_tool`] run here: they are `/bin/sh`
/// scripts, and Windows runs a client tool only as an `.exe` (K68). A test
/// built on running them prints why and stops on Windows; Windows' real
/// tools are run by the tests that use the installed client.
pub(super) fn fake_tools_run() -> bool {
    if cfg!(windows) {
        eprintln!(
            "SKIPPED: the stand-in client tools are /bin/sh scripts, and Windows runs a client \
             tool only as an .exe"
        );
    }
    !cfg!(windows)
}

/// A stand-in client: the folder `root/name` holding every tool of
/// `family`, each one `fake_tool(…, version, body)`. Its real path.
pub(super) fn fake_client(
    root: &Path,
    name: &str,
    family: ClientFamily,
    version: &str,
    body: &str,
) -> PathBuf {
    let dir = root.join(name);
    for tool in family.tools() {
        fake_tool(&dir, tool.name(), version, body);
    }
    std::fs::canonicalize(dir).unwrap()
}

impl Fixture {
    /// Opens the backup of `database`, or the restore of a file into it, in
    /// its wizard: "Advanced settings…" of the simple view (K26).
    pub(super) fn backup_dialog(
        &self,
        cx: &mut TestAppContext,
        database: &str,
        restore: bool,
    ) -> Option<Entity<BackupDialog>> {
        let dialog = self.simple_dialog(cx, database, restore)?;
        cx.update(|cx| dialog.update(cx, |dialog, cx| dialog.show_advanced(cx)));
        cx.run_until_parked();
        Some(dialog)
    }

    /// Opens the backup of `database`, or the restore of a file into it,
    /// as the tree and the palette do: in its simple view.
    pub(super) fn simple_dialog(
        &self,
        cx: &mut TestAppContext,
        database: &str,
        restore: bool,
    ) -> Option<Entity<BackupDialog>> {
        let target = self.target(cx, Some(database));
        let workspace = self.workspace.clone();
        self.step(cx, move |window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.backup_restore(target, restore, window, cx)
            })
        })
    }

    /// Opens the wizard of `mode` on `database`.
    pub(super) fn wizard(
        &self,
        cx: &mut TestAppContext,
        database: &str,
        mode: Mode,
    ) -> Option<Entity<BackupDialog>> {
        let target = self.target(cx, Some(database));
        let connections = self.connections.clone();
        self.step(cx, move |window, cx| {
            backup_dialog::open(connections, target, mode, window, cx, |_, _, _, _| {})
        })
    }

    /// Waits until the dialog's state starts with one of `ends`.
    pub(super) fn wait_backup(
        &self,
        cx: &mut TestAppContext,
        dialog: &Entity<BackupDialog>,
        ends: &[&str],
    ) -> String {
        self.wait_until(cx, &format!("the dialog to show {ends:?}"), |_, cx| {
            let state = cx.update(|cx| dialog.read(cx).state(cx));
            ends.iter().any(|end| state.starts_with(end))
        });
        let state = cx.update(|cx| dialog.read(cx).state(cx));
        // What the tool said, for the message of a test that did not expect a failure.
        if state.starts_with("failed") {
            let log = cx.update(|cx| dialog.read(cx).log());
            eprintln!("the tool's log:\n{}", log.join("\n"));
        }
        state
    }

    pub(super) fn wizard_state(
        &self,
        cx: &mut TestAppContext,
        dialog: &Entity<BackupDialog>,
    ) -> String {
        cx.update(|cx| dialog.read(cx).state(cx))
    }

    /// Makes `dir` an added local client and the default of `family`.
    pub(super) fn use_client(&self, cx: &mut TestAppContext, family: ClientFamily, dir: &Path) {
        let dir = dir.to_path_buf();
        cx.update(|cx| {
            AppState::update_settings(cx, |settings| {
                settings.client_folders.push(dir.clone());
                settings.default_clients.insert(family, dir);
            })
        });
    }

    /// Scrolls the wizard's page (or the settings' section) until `id`
    /// shows whole, then clicks it: a click at the middle of a field half
    /// out of the page would land beside it (K65: the fonts of Linux lay the
    /// page out a few pixels taller).
    pub(super) fn tap(&self, cx: &mut TestAppContext, id: impl Into<gpui_kit::ElementId>) {
        let id = id.into();
        for _ in 0..40 {
            let Some(page) = ["wizard-page", "settings-body"]
                .into_iter()
                .find(|page| self.has(cx, *page))
            else {
                break;
            };
            // On the page and above it: scroll up; below or cut at its
            // bottom: down. A button under the page is clicked at once.
            let up = self.step(cx, |window, _| {
                let page_bounds = window.find(page).bounds();
                let element = window.within(page).try_find(id.clone())?;
                let bounds = element.bounds();
                if bounds.top() < page_bounds.top() {
                    Some(true)
                } else if element.visible() && bounds.bottom() <= page_bounds.bottom() {
                    None
                } else {
                    Some(false)
                }
            });
            let Some(up) = up else {
                break;
            };
            let delta = if up { 120. } else { -120. };
            self.step(cx, |window, cx| {
                window.scroll(
                    page,
                    gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(delta))),
                    cx,
                )
            });
        }
        self.click(cx, id);
    }

    /// Chooses `dir` as the wizard's client with "Local client…" ▸ Browse….
    fn browse_client(&self, cx: &mut TestAppContext, dir: &Path) {
        self.tap(cx, "backup-local-client");
        self.browse_client_from_panel(cx, dir);
    }

    /// Chooses `path` in the open prompt the last click opened.
    pub(super) fn answer_open_prompt(&self, cx: &mut TestAppContext, path: &Path) {
        let path = path.to_path_buf();
        cx.simulate_path_prompt_response(move |_| Some(vec![path]));
        cx.run_until_parked();
    }

    /// Goes on to the settings page.
    pub(super) fn to_settings(&self, cx: &mut TestAppContext, dialog: &Entity<BackupDialog>) {
        while cx.update(|cx| dialog.read(cx).page()) != "Settings" {
            self.tap(cx, "wizard-next");
        }
    }

    /// The output folder and the file name pattern of a backup.
    pub(super) fn write_to(&self, cx: &mut TestAppContext, folder: &Path, pattern: Option<&str>) {
        self.tap(cx, "backup-choose-folder");
        self.answer_open_prompt(cx, folder);
        if let Some(pattern) = pattern {
            self.replace_text(cx, "backup-pattern", pattern);
        }
    }

    /// Replaces the text of the field `id`.
    pub(super) fn replace_text(&self, cx: &mut TestAppContext, id: &'static str, text: &str) {
        self.tap(cx, id);
        self.press(cx, &["secondary-a", "backspace"]);
        if !text.is_empty() {
            self.type_text(cx, text);
        }
    }

    /// Clicks Start (or Restore… / Run…) and confirms what asks.
    fn confirm(&self, cx: &mut TestAppContext) {
        self.wait_until(cx, "the confirmation", |app, cx| app.has(cx, "sql-preview"));
        self.tap(cx, "sql-confirm");
    }
}

/// Whether the window's last frame painted text holding `part`.
pub(super) fn paints(app: &Fixture, cx: &mut TestAppContext, part: &str) -> bool {
    app.step(cx, |window, _| {
        gpui_kit::base::test_support::texts(window)
            .into_iter()
            .any(|text| text.text().contains(part))
    })
}

/// The warning about the client in use.
fn warning(cx: &mut TestAppContext, dialog: &Entity<BackupDialog>) -> String {
    cx.update(|cx| dialog.read(cx).notes().0)
        .unwrap_or_default()
}

/// The file the backup would write now.
fn preview(cx: &mut TestAppContext, dialog: &Entity<BackupDialog>) -> String {
    cx.update(|cx| dialog.read(cx).file_preview(cx))
        .map(|path| path.display().to_string())
        .unwrap_or_else(|why| why)
}

fn written(cx: &mut TestAppContext, dialog: &Entity<BackupDialog>) -> PathBuf {
    cx.update(|cx| dialog.read(cx).written()).expect("a file")
}

// Kept from the backup dialog (M8), on the wizard ------------------------------

#[gpui_kit::test]
fn a_backup_tool_is_found_or_chosen_and_its_output_streams(cx: &mut TestAppContext) {
    if !fake_tools_run() {
        return;
    }
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();

    // The tree's database offers it.
    app.wait_until(cx, "the databases", |app, cx| {
        app.tree(cx).contains(&row(0, "kerno_other"))
    });
    app.act(cx, Action::Backup, &["kerno_other"]);
    // A database's backup opens in its simple view (K26).
    assert!(app.has(cx, "backup-dialog") && !app.has(cx, ("wizard-step", 0usize)));
    app.wait_until(cx, "the simple view", |app, cx| {
        app.has(cx, "backup-simple")
    });
    app.tap(cx, "backup-close");
    assert!(!app.has(cx, "backup-dialog"));

    // A default folder that holds no client is never run: the newest
    // client found is, and the wizard says why.
    let empty = root.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    app.use_client(cx, ClientFamily::Postgres, &empty);
    let dialog = app
        .backup_dialog(cx, "kerno_other", false)
        .expect("the backup dialog");
    let state = app.wait_backup(cx, &dialog, &["ready", "old client", "no tool"]);
    app.to_settings(cx, &dialog);
    if state.starts_with("ready") {
        assert!(warning(cx, &dialog).contains(&format!(
            "{} is not a client detected or added",
            empty.display()
        )),);
    }

    // Too old for the server: refused, with its version.
    let old = fake_client(
        &root,
        "pg9",
        ClientFamily::Postgres,
        "pg_dump (PostgreSQL) 9.6.24",
        "exit 0",
    );
    app.browse_client(cx, &old);
    let state = app.wait_backup(cx, &dialog, &["old client"]);
    assert!(
        state.contains("This client is PostgreSQL 9, older than the server")
            && state.contains("pg_dump refuses a server newer than itself"),
        "{state}"
    );
    app.tap(cx, "backup-start");
    assert_eq!(app.wizard_state(cx, &dialog), state, "Start is off");

    // A recent one: the command it will run, with no secret in it.
    let partial = fake_client(
        &root,
        "pg99",
        ClientFamily::Postgres,
        "pg_dump (PostgreSQL) 99.0",
        r#"echo "pg_dump: dumping contents of table public.notes"
[ -n "$PGPASSWORD" ] && echo "the password came through the environment"
echo "pg_dump: warning: a stand-in" >&2
for arg in "$@"; do case $arg in --file=*) echo partial > "${arg#--file=}";; esac; done
exit 3"#,
    );
    app.browse_client(cx, &partial);
    let state = app.wait_backup(cx, &dialog, &["ready"]);
    for part in [
        "PGPASSWORD=•••",
        partial.join("pg_dump").to_str().unwrap(),
        "--dbname=kerno_other",
        "--format=c",
    ] {
        assert!(state.contains(part), "{part}: {state}");
    }
    assert!(!state.contains(testing::DB_PASSWORD), "{state}");
    // The folder is added to the local clients and kept for the connection.
    assert!(cx.update(|cx| AppState::get(cx).settings.client_folders.contains(&partial)));
    let kept = cx.update(|cx| {
        app.connections.read(cx).saved()[0]
            .config
            .client_dir
            .clone()
    });
    assert_eq!(kept, Some(partial.clone()));

    // The command follows the settings and the objects.
    app.tap(cx, ("backup-format", 0usize));
    app.tap(cx, ("backup-content", 1usize));
    let state = app.wizard_state(cx, &dialog);
    assert!(
        state.contains("--format=p") && state.contains("--schema-only"),
        "{state}"
    );
    app.tap(cx, "wizard-back");
    app.wait_until(cx, "the objects", |_, cx| {
        cx.update(|cx| dialog.read(cx).objects_loaded())
    });
    app.tap(cx, "backup-complete");
    let state = app.wizard_state(cx, &dialog);
    assert!(state.contains(r#"--schema="public""#), "{state}");
    app.tap(cx, "backup-schemas-none");
    assert!(
        app.wizard_state(cx, &dialog)
            .contains("Tick at least one schema or table")
    );
    app.tap(cx, "backup-complete");
    app.to_settings(cx, &dialog);

    // It runs; its lines stream into the log, and a failure removes the
    // file it left.
    app.write_to(cx, &root, Some("failed"));
    let failed = root.join("failed.sql");
    assert_eq!(preview(cx, &dialog), failed.display().to_string());
    app.tap(cx, "backup-start");
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    assert!(
        state.contains("pg_dump failed (exit status 3)")
            && state.contains("The incomplete file was removed"),
        "{state}"
    );
    assert!(!failed.exists());
    let log = cx.update(|cx| dialog.read(cx).log());
    for line in [
        "pg_dump: dumping contents of table public.notes",
        "the password came through the environment",
        "pg_dump: warning: a stand-in",
    ] {
        assert!(log.iter().any(|shown| shown == line), "{line}: {log:?}");
    }
    assert!(app.has(cx, "backup-log") && app.has(cx, "backup-copy-log"));
    assert!(paints(&app, cx, "Took"));
    assert!(!app.has(cx, "backup-reveal"));

    // A file already there is replaced only once confirmed; a tool that
    // fails before writing leaves it as it was.
    let earlier = root.join("earlier.sql");
    std::fs::write(&earlier, "an earlier backup\n").unwrap();
    let refused = fake_client(
        &root,
        "refused",
        ClientFamily::Postgres,
        "pg_dump (PostgreSQL) 99.0",
        r#"echo "pg_dump: error: connection refused" >&2
exit 1"#,
    );
    app.tap(cx, "backup-back");
    app.browse_client(cx, &refused);
    app.wait_backup(cx, &dialog, &["ready"]);
    app.replace_text(cx, "backup-pattern", "earlier");
    app.tap(cx, "backup-start");
    app.confirm(cx);
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    assert_eq!(state, "failed: pg_dump failed (exit status 1).");
    assert_eq!(
        std::fs::read_to_string(&earlier).unwrap(),
        "an earlier backup\n"
    );

    // A tool that succeeds: the file, its size, and where it is.
    let works = fake_client(
        &root,
        "works",
        ClientFamily::Postgres,
        "pg_dump (PostgreSQL) 99.0",
        r#"for arg in "$@"; do case $arg in --file=*) echo dumped > "${arg#--file=}";; esac; done
echo done"#,
    );
    app.tap(cx, "backup-back");
    app.browse_client(cx, &works);
    app.wait_backup(cx, &dialog, &["ready"]);
    app.tap(cx, "backup-start");
    app.confirm(cx);
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    assert!(
        state.starts_with("done: pg_dump finished (exit status 0). Backed up kerno_other to ")
            && state.contains("earlier.sql (7 B)"),
        "{state}"
    );
    assert_eq!(std::fs::read_to_string(&earlier).unwrap(), "dumped\n");
    assert!(app.has(cx, "backup-reveal"));
    app.tap(cx, "backup-close");
}

#[gpui_kit::test]
fn a_database_is_backed_up_and_restored_with_the_real_tools(cx: &mut TestAppContext) {
    for (engine, scratch) in [
        (Engine::Postgres, "kerno_ui_restore_pg"),
        (Engine::MySql, "kerno_ui_restore_my"),
    ] {
        if !testing::available(engine) {
            return;
        }
        let drop = match engine {
            Engine::Postgres => format!("DROP DATABASE IF EXISTS {scratch} WITH (FORCE)"),
            _ => format!("DROP DATABASE IF EXISTS {scratch}"),
        };
        let _scratch = Scratch::new(engine, &[&drop], &[&format!("CREATE DATABASE {scratch}")]);
        let app = connected(cx, engine);
        let dir = tempfile::tempdir().unwrap();

        let dialog = app
            .backup_dialog(cx, "kerno_other", false)
            .expect("the backup dialog");
        let state = app.wait_backup(cx, &dialog, &["no tool", "ready", "old client"]);
        if !state.starts_with("ready") {
            eprintln!("SKIPPED ({engine:?}): no usable client tool: {state}");
            continue;
        }
        app.to_settings(cx, &dialog);
        app.write_to(cx, dir.path(), None);
        app.tap(cx, "backup-start");
        let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
        assert!(state.starts_with("done: "), "{engine:?}: {state}");
        assert!(
            !cx.update(|cx| dialog.read(cx).log()).is_empty(),
            "--verbose says what it does"
        );
        assert!(app.has(cx, "backup-reveal"));
        let file = written(cx, &dialog);
        app.tap(cx, "backup-close");

        // The scratch database's empty Tables folder, open in the tree.
        let tables: Vec<&str> = match engine {
            Engine::Postgres => vec![scratch, "public", "Tables"],
            _ => vec![scratch, "Tables"],
        };
        app.open_path(cx, &tables);
        app.toggle(cx, &tables);
        let notes = [tables.as_slice(), &["notes"]].concat();
        assert!(!app.shows(cx, &notes));

        // Restored into the scratch database, after the confirmation.
        let dialog = app
            .backup_dialog(cx, scratch, true)
            .expect("the restore dialog");
        app.wait_backup(cx, &dialog, &["choose a file"]);
        app.tap(cx, "restore-choose-file");
        app.answer_open_prompt(cx, &file);
        let state = app.wait_backup(cx, &dialog, &["ready", "refused", "no tool", "file"]);
        assert!(state.starts_with("ready: "), "{engine:?}: {state}");
        app.tap(cx, "restore-start");
        assert!(app.has(cx, "sql-preview"), "the command is shown first");
        app.tap(cx, "sql-confirm");
        let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
        assert!(state.starts_with("done: "), "{engine:?}: {state}");
        let count = match engine {
            Engine::Postgres => on_database(engine, Some(scratch), "SELECT count(*) FROM notes"),
            _ => on_server(engine, &format!("SELECT count(*) FROM {scratch}.notes")),
        };
        assert_eq!(count, "2", "{engine:?}");
        app.tap(cx, "backup-close");
        // The tree reads the database again (on PostgreSQL its schemas
        // come back closed: they are opened again on the way).
        app.wait_until(cx, "the restored table in the tree", |app, cx| {
            for depth in 1..=tables.len() {
                let part = &tables[..depth];
                if app.shows(cx, part) && !app.is_open(cx, part) {
                    app.toggle(cx, part);
                }
            }
            app.shows(cx, &notes)
        });
    }
}

#[gpui_kit::test]
fn a_file_with_client_commands_is_refused_until_explicitly_allowed(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let (pwned, started) = (root.join("pwned"), root.join("started"));
    // The "psql" leaves a mark if it ever starts.
    let client = fake_client(
        &root,
        "client",
        ClientFamily::Postgres,
        "psql (PostgreSQL) 99.0",
        &format!("touch {}", started.display()),
    );
    app.use_client(cx, ClientFamily::Postgres, &client);
    let hostile = root.join("hostile.sql");
    std::fs::write(
        &hostile,
        format!("SELECT 1;\n\\! touch {}\n", pwned.display()),
    )
    .unwrap();
    let clean = root.join("clean.sql");
    std::fs::write(&clean, "SELECT 1;\n").unwrap();

    // Never run: the database need not exist.
    let dialog = app
        .backup_dialog(cx, "kerno_ui_restore_never", true)
        .expect("the restore dialog");
    app.wait_backup(cx, &dialog, &["choose a file"]);
    app.tap(cx, "restore-choose-file");
    app.answer_open_prompt(cx, &hostile);
    let state = app.wait_backup(cx, &dialog, &["refused", "ready", "file", "no tool"]);
    assert_eq!(state, "refused: line 2: \\!");
    // The check is a step of its own, before Restore….
    assert!(!app.has(cx, "restore-start") && app.has(cx, ("wizard-step", 2usize)));
    app.tap(cx, "wizard-next");
    assert!(app.has(cx, "restore-refusal") && app.has(cx, "restore-allow-commands"));
    app.tap(cx, "restore-start");
    assert!(!app.has(cx, "sql-preview"), "a refused file is not offered");
    let checked = |cx: &mut TestAppContext| {
        cx.update(|cx| dialog.read(cx).pending_command(cx))
            .expect("a command")
            .script_check
            .is_some()
    };
    assert!(
        checked(cx),
        "the file is checked again when the tool starts"
    );

    // The opt-in: unchecked at first, then the command runs the file as is.
    app.tap(cx, "restore-allow-commands");
    let state = app.wizard_state(cx, &dialog);
    assert!(state.starts_with("ready: "), "{state}");
    assert!(!checked(cx));

    // Another file, then the first again: the opt-in starts over.
    app.tap(cx, "wizard-back");
    app.tap(cx, "restore-choose-file");
    app.answer_open_prompt(cx, &clean);
    let state = app.wait_backup(cx, &dialog, &["refused", "ready", "file"]);
    assert!(state.starts_with("ready: ") && checked(cx), "{state}");
    app.tap(cx, "restore-choose-file");
    app.answer_open_prompt(cx, &hostile);
    assert_eq!(
        app.wait_backup(cx, &dialog, &["refused", "ready", "file"]),
        "refused: line 2: \\!"
    );

    // A pg_dump --create file switches databases: refused too.
    let create = root.join("create.sql");
    std::fs::write(
        &create,
        "CREATE DATABASE other;\n\\connect other\nSELECT 1;\n",
    )
    .unwrap();
    app.tap(cx, "restore-choose-file");
    app.answer_open_prompt(cx, &create);
    assert_eq!(
        app.wait_backup(cx, &dialog, &["refused", "ready", "file"]),
        "refused: line 2: \\connect"
    );
    app.tap(cx, "backup-close");
    assert!(!pwned.exists() && !started.exists(), "nothing ran");
}

#[gpui_kit::test]
fn a_restore_asks_twice_on_production_and_is_refused_on_read_only(cx: &mut TestAppContext) {
    if !fake_tools_run() {
        return;
    }
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

    // Read-only: no restore and no script at all; a backup reads only.
    let mut config = testing::config(Engine::Postgres);
    config.read_only = true;
    let app = connected_as(cx, config);
    assert!(app.backup_dialog(cx, "kerno_other", true).is_none());
    assert!(app.wizard(cx, "kerno_other", Mode::ExecuteScript).is_none());
    assert!(!app.has(cx, "backup-dialog"));
    assert!(app.backup_dialog(cx, "kerno_other", false).is_some());
    app.tap(cx, "backup-close");
    app.wait_until(cx, "the tree", |app, cx| {
        app.tree(cx).contains(&row(0, "kerno_other"))
    });
    assert_eq!(
        app.why_not(cx, Action::Restore, &["kerno_other"]),
        Some("read-only connection")
    );
    assert_eq!(app.why_not(cx, Action::Backup, &["kerno_other"]), None);

    // Production: the first click only arms the button.
    let mut config = testing::config(Engine::Postgres);
    config.environment = kerno_core::environment::PRODUCTION.into();
    let app = connected_as(cx, config);
    app.use_client(cx, ClientFamily::Postgres, &client);
    let dialog = app
        .backup_dialog(cx, "kerno_ui_restore_never", true)
        .expect("the restore dialog");
    app.wait_backup(cx, &dialog, &["choose a file"]);
    app.tap(cx, "restore-choose-file");
    app.answer_open_prompt(cx, &file);
    app.wait_backup(cx, &dialog, &["ready"]);
    app.tap(cx, "restore-start");
    assert!(app.has(cx, "sql-preview") && app.has(cx, "sql-production"));
    app.tap(cx, "sql-confirm");
    assert!(app.has(cx, "sql-preview") && !ran.exists(), "armed only");
    app.tap(cx, "sql-confirm");
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    assert!(
        state.starts_with("done: psql finished (exit status 0). Restored ")
            && state.contains("into kerno_ui_restore_never"),
        "{state}"
    );
    assert!(ran.exists());
    assert_eq!(cx.update(|cx| dialog.read(cx).log()), ["restored"]);
}

#[gpui_kit::test]
fn a_running_backup_is_cancelled_from_the_progress_page(cx: &mut TestAppContext) {
    if !fake_tools_run() {
        return;
    }
    if !testing::available(Engine::MySql) {
        return;
    }
    let app = connected(cx, Engine::MySql);
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    // The file it writes has a time before the run's start, as Linux's
    // coarse file clock can give it (K65): it is removed all the same.
    // Mutation seen: the file's time compared with the run's start again.
    let client = fake_client(
        &root,
        "client",
        ClientFamily::MySql,
        "mysqldump  Ver 9.4.0 for macos15 on arm64 (Homebrew)",
        r#"for arg in "$@"; do case $arg in --result-file=*) echo partial > "${arg#--result-file=}"; touch -t 202001010000 "${arg#--result-file=}";; esac; done
echo "-- started"
exec sleep 60"#,
    );
    app.use_client(cx, ClientFamily::MySql, &client);
    let dialog = app
        .backup_dialog(cx, "kerno_other", false)
        .expect("the backup dialog");
    let state = app.wait_backup(cx, &dialog, &["ready", "no tool"]);
    assert!(
        state.contains("--result-file=") && state.ends_with(" kerno_other"),
        "{state}"
    );
    app.to_settings(cx, &dialog);
    // Another version than the server's only warns.
    assert!(warning(cx, &dialog).contains("usually works"));
    app.write_to(cx, &root, Some("cut"));
    let file = root.join("cut.sql");
    app.tap(cx, "backup-start");
    app.wait_until(cx, "the first line", |_, cx| {
        cx.update(|cx| dialog.read(cx).log()) == ["-- started"]
    });
    assert_eq!(app.wizard_state(cx, &dialog), "running");
    assert!(paints(&app, cx, "Running · "));
    app.tap(cx, "backup-cancel");
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    assert!(
        state.contains("Cancelled: mysqldump was stopped.")
            && state.contains("The incomplete file was removed"),
        "{state}"
    );
    assert!(!file.exists());

    // Closing while it runs asks, then stops it and closes.
    app.tap(cx, "backup-back");
    app.tap(cx, "backup-start");
    app.wait_until(cx, "the second run", |_, cx| {
        cx.update(|cx| dialog.read(cx).log()) == ["-- started"]
    });
    app.tap(cx, "close");
    assert!(app.has(cx, "backup-stop-and-close") && app.has(cx, "backup-dialog"));
    app.tap(cx, "backup-keep-running");
    assert_eq!(app.wizard_state(cx, &dialog), "running");
    app.tap(cx, "close");
    app.tap(cx, "backup-stop-and-close");
    app.wait_until(cx, "the wizard closed", |app, cx| {
        !app.has(cx, "backup-dialog")
    });
    assert!(app.wizard_state(cx, &dialog).contains("Cancelled"));
    assert!(!file.exists());
}

#[gpui_kit::test]
fn a_backup_goes_through_the_ssh_tunnel(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) || !testing::ssh_available() {
        return;
    }
    let storage = memory_storage();
    let mut config = testing::config(Engine::Postgres);
    config.host = testing::service_name(Engine::Postgres).into();
    config.port = 5432;
    config.ssh = Some(testing::ssh(kerno_core::config::SshAuth::Password));
    let id = storage.save_connection(None, &config).unwrap();
    let app = open(cx, storage);
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    kerno_core::secrets::save_secrets(secrets.as_ref(), &id, &testing::secrets()).unwrap();
    app.step(cx, |window, cx| {
        window.double_click(("connection", 0usize), cx)
    });
    app.wait_until(cx, "the host key question", |app, cx| app.has(cx, "ok"));
    app.tap(cx, "ok");
    app.wait_until(cx, "the connection", |app, cx| {
        assert!(
            !matches!(app.status(cx, 0), Status::Failed { .. }),
            "{:?}",
            app.status(cx, 0)
        );
        matches!(app.status(cx, 0), Status::Connected { .. })
    });

    let dialog = app
        .backup_dialog(cx, "kerno_other", false)
        .expect("the backup dialog");
    let state = app.wait_backup(cx, &dialog, &["ready", "no tool", "old client", "blocked"]);
    if !state.starts_with("ready") {
        eprintln!("SKIPPED: no usable pg_dump: {state}");
        return;
    }
    assert!(
        state.contains("PGHOSTADDR=127.0.0.1")
            && state.contains("--host=kerno-postgres --port=<SSH tunnel>"),
        "{state}"
    );
    app.to_settings(cx, &dialog);
    app.tap(cx, ("backup-format", 0usize));
    app.tap(cx, ("backup-content", 1usize));
    let dir = tempfile::tempdir().unwrap();
    app.write_to(cx, dir.path(), Some("tunnel"));
    app.tap(cx, "backup-start");
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    assert!(state.starts_with("done: "), "{state}");
    let dump = std::fs::read_to_string(written(cx, &dialog)).unwrap();
    assert!(dump.contains("CREATE TABLE public.notes"), "{dump}");
}

// Part 11a ---------------------------------------------------------------------

#[gpui_kit::test]
fn a_backup_picks_objects_in_a_tree_and_writes_the_file(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let (cleanup, setup, database): (&str, Vec<&str>, &str) = match engine {
            Engine::Postgres => (
                "DROP SCHEMA IF EXISTS kerno_ui_wizard CASCADE",
                vec![
                    "CREATE SCHEMA kerno_ui_wizard",
                    "CREATE TABLE kerno_ui_wizard.alpha (id int)",
                    "CREATE TABLE kerno_ui_wizard.beta (id int)",
                    "CREATE TABLE kerno_ui_wizard.gamma (id int)",
                    "CREATE VIEW kerno_ui_wizard.seen AS SELECT 1 AS one",
                ],
                "kerno",
            ),
            _ => (
                "DROP DATABASE IF EXISTS kerno_ui_wizard",
                vec![
                    "CREATE DATABASE kerno_ui_wizard",
                    "CREATE TABLE kerno_ui_wizard.alpha (id int)",
                    "CREATE TABLE kerno_ui_wizard.beta (id int)",
                    "CREATE TABLE kerno_ui_wizard.gamma (id int)",
                ],
                "kerno_ui_wizard",
            ),
        };
        let _scratch = Scratch::new(engine, &[cleanup], &setup);
        let app = connected(cx, engine);
        let dir = tempfile::tempdir().unwrap();
        let dialog = app.backup_dialog(cx, database, false).expect("the wizard");
        let state = app.wait_backup(cx, &dialog, &["ready", "no tool", "old client"]);
        if !state.starts_with("ready") {
            eprintln!("SKIPPED ({engine:?}): no usable client tool: {state}");
            continue;
        }
        app.wait_until(cx, "the objects", |_, cx| {
            cx.update(|cx| dialog.read(cx).objects_loaded())
        });
        assert_eq!(cx.update(|cx| dialog.read(cx).page()), "Objects");
        // A complete backup at first; off, everything is ticked; None.
        app.tap(cx, "backup-complete");
        if engine == Engine::Postgres {
            app.tap(cx, "backup-schemas-none");
            let schemas = cx.update(|cx| dialog.read(cx).schema_names());
            let at = schemas
                .iter()
                .position(|name| name == "kerno_ui_wizard")
                .expect("the scratch schema");
            app.tap(cx, ("backup-schema-name", at));
        } else {
            app.tap(cx, "backup-tables-none");
        }
        let tables = cx.update(|cx| dialog.read(cx).table_names());
        assert_eq!(tables, ["alpha", "beta", "gamma"], "views are hidden");
        for name in ["alpha", "gamma"] {
            let at = tables.iter().position(|table| table == name).unwrap();
            app.tap(cx, ("backup-table", at));
        }
        let schema = "kerno_ui_wizard";
        let picked = cx.update(|cx| dialog.read(cx).settings(cx)).unwrap();
        assert_eq!(
            (picked.schemas.as_slice(), picked.tables.as_slice()),
            (
                &[][..],
                &[
                    TableRef::new(schema, "alpha"),
                    TableRef::new(schema, "gamma")
                ][..]
            )
        );
        if engine == Engine::Postgres {
            // Views shown, not ticked; the schema ticked whole is a
            // --schema.
            app.tap(cx, "backup-show-views");
            assert!(
                cx.update(|cx| dialog.read(cx).table_names())
                    .contains(&"seen".to_string())
            );
            let at = cx
                .update(|cx| dialog.read(cx).schema_names())
                .iter()
                .position(|name| name == "kerno_ui_wizard")
                .unwrap();
            app.tap(cx, ("backup-schema", at));
            app.tap(cx, ("backup-schema", at));
            let picked = cx.update(|cx| dialog.read(cx).settings(cx)).unwrap();
            assert_eq!(picked.schemas, ["kerno_ui_wizard"]);
            assert!(picked.tables.is_empty());
            app.tap(cx, ("backup-schema", at));
            app.tap(cx, "backup-show-views");
            for name in ["alpha", "gamma"] {
                let at = tables.iter().position(|table| table == name).unwrap();
                app.tap(cx, ("backup-table", at));
            }
        }
        app.tap(cx, "wizard-next");
        if engine == Engine::Postgres {
            app.tap(cx, ("backup-format", 0usize));
        }
        app.write_to(cx, dir.path(), Some("${schema}-${table}"));
        assert!(preview(cx, &dialog).ends_with("kerno_ui_wizard-alpha.sql"));
        app.tap(cx, "backup-start");
        let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
        assert!(state.starts_with("done: "), "{engine:?}: {state}");
        let dump = std::fs::read_to_string(written(cx, &dialog)).unwrap();
        let created = |name: &str| match engine {
            Engine::Postgres => dump.contains(&format!("CREATE TABLE kerno_ui_wizard.{name} ")),
            _ => dump.contains(&format!("CREATE TABLE `{name}`")),
        };
        assert!(created("alpha") && created("gamma"), "{engine:?}: {dump}");
        assert!(
            !created("beta") && !dump.contains("CREATE VIEW"),
            "{engine:?}"
        );
        app.tap(cx, "backup-close");
    }
}

#[gpui_kit::test]
fn the_options_page_changes_the_command(cx: &mut TestAppContext) {
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
        .backup_dialog(cx, "kerno_other", false)
        .expect("the wizard");
    app.wait_backup(cx, &dialog, &["ready"]);
    app.to_settings(cx, &dialog);
    let state = |cx: &mut TestAppContext| app.wizard_state(cx, &dialog);
    assert!(state(cx).contains("--format=c"), "DBeaver's default");
    app.tap(cx, "backup-no-owner");
    app.tap(cx, "backup-clean");
    let now = state(cx);
    assert!(
        now.contains("--no-owner --clean") && !now.contains("--if-exists"),
        "{now}"
    );
    app.tap(cx, "backup-if-exists");
    assert!(state(cx).contains("--clean --if-exists"));
    // Without DROP statements, no IF EXISTS either.
    app.tap(cx, "backup-clean");
    let now = state(cx);
    assert!(
        !now.contains("--clean") && !now.contains("--if-exists"),
        "{now}"
    );
    app.tap(cx, "backup-inserts");
    app.tap(cx, "backup-no-privileges");
    app.tap(cx, "backup-create");
    let now = state(cx);
    for part in ["--inserts", "--no-privileges", "--create"] {
        assert!(now.contains(part), "{part}: {now}");
    }

    // The directory format: parallel jobs, no extension.
    app.tap(cx, ("backup-format", 2usize));
    assert!(app.has(cx, "backup-jobs"));
    app.replace_text(cx, "backup-jobs", "4");
    let now = state(cx);
    assert!(now.contains("--format=d --jobs=4"), "{now}");
    let shown = preview(cx, &dialog);
    assert!(
        !shown.ends_with(".dump") && !shown.ends_with(".tar"),
        "{shown}"
    );
    // Tar: no compression, refused in words.
    app.tap(cx, ("backup-format", 3usize));
    assert!(!state(cx).contains("--jobs"));
    app.replace_text(cx, "backup-compression", "5");
    assert_eq!(
        state(cx),
        "not ready: Not possible: the tar format is not compressed."
    );
    app.replace_text(cx, "backup-compression", "12");
    assert_eq!(state(cx), "not ready: Compression is a number from 0 to 9.");
    app.tap(cx, ("backup-format", 1usize));
    app.replace_text(cx, "backup-compression", "9");
    app.replace_text(cx, "backup-encoding", "UTF8");
    let now = state(cx);
    assert!(now.contains("--compress=9 --encoding=UTF8"), "{now}");

    // Extra arguments: last but checked.
    app.replace_text(cx, "backup-extra-args", "--no-comments --host=evil");
    assert!(app.has(cx, "backup-extra-error"));
    assert!(state(cx).starts_with("not ready: “--host=evil” is not allowed here"));
    app.replace_text(cx, "backup-extra-args", "--no-comments");
    assert!(state(cx).ends_with(" --no-comments"));

    // The file name pattern and its preview.
    app.replace_text(cx, "backup-pattern", "${database}_${schema}_x");
    assert!(preview(cx, &dialog).ends_with("kerno_other_kerno_other_x.dump"));

    // Another user: the name in the command and the settings, the
    // password in neither.
    app.tap(cx, "backup-auth");
    app.replace_text(cx, "backup-auth-user", "kerno_ui_wizard_role");
    app.replace_text(cx, "backup-auth-password", "pw-not-kept");
    let now = state(cx);
    assert!(
        now.contains("--username=kerno_ui_wizard_role") && !now.contains("pw-not-kept"),
        "{now}"
    );
    let settings = cx.update(|cx| dialog.read(cx).settings(cx)).unwrap();
    assert_eq!(settings.auth_user.as_deref(), Some("kerno_ui_wizard_role"));
    let json = serde_json::to_string(&settings).unwrap();
    assert!(!json.contains("pw-not-kept"), "{json}");
    let command = cx
        .update(|cx| dialog.read(cx).pending_command(cx))
        .expect("a command");
    assert_eq!(
        command
            .password
            .map(|(_, secret)| secret.expose().to_string()),
        Some("pw-not-kept".into()),
        "the password goes through the environment"
    );
    app.tap(cx, "backup-close");

    // MySQL's own options.
    if !testing::available(Engine::MySql) {
        return;
    }
    let app = connected(cx, Engine::MySql);
    let client = fake_client(
        &root,
        "my",
        ClientFamily::MySql,
        "mysqldump  Ver 8.4.3 for Linux on x86_64",
        "exit 0",
    );
    app.use_client(cx, ClientFamily::MySql, &client);
    let dialog = app
        .backup_dialog(cx, "kerno_other", false)
        .expect("the wizard");
    app.wait_backup(cx, &dialog, &["ready"]);
    app.to_settings(cx, &dialog);
    let state = |cx: &mut TestAppContext| app.wizard_state(cx, &dialog);
    // Plain SQL, or the folder of one file per table (K63).
    assert!(app.has(cx, ("backup-format", 1usize)) && !app.has(cx, ("backup-format", 2usize)));
    app.tap(cx, ("backup-method", 1usize));
    app.tap(cx, "backup-events");
    app.tap(cx, "backup-routines");
    app.tap(cx, "backup-hex-blob");
    app.tap(cx, "backup-extended-inserts");
    let now = state(cx);
    assert!(
        // utf8mb4 when the server and the client have it (K63, F3).
        now.contains(
            "--lock-all-tables --default-character-set=utf8mb4 --events --skip-extended-insert"
        ) && !now.contains("--routines")
            && !now.contains("--hex-blob"),
        "{now}"
    );
}

#[gpui_kit::test]
fn a_restore_asks_with_its_command_and_restores(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let (source, target) = ("kerno_ui_wizard_src", "kerno_ui_wizard_dst");
    let _scratch = Scratch::new(
        Engine::Postgres,
        &[
            &format!("DROP DATABASE IF EXISTS {source} WITH (FORCE)"),
            &format!("DROP DATABASE IF EXISTS {target} WITH (FORCE)"),
        ],
        &[
            &format!("CREATE DATABASE {source}"),
            &format!("CREATE DATABASE {target}"),
        ],
    );
    on_database(
        Engine::Postgres,
        Some(source),
        "CREATE TABLE kept (id int); INSERT INTO kept VALUES (1), (2), (3)",
    );
    on_database(
        Engine::Postgres,
        Some(target),
        "CREATE TABLE kept (id int); INSERT INTO kept VALUES (9)",
    );
    let app = connected(cx, Engine::Postgres);
    let dir = tempfile::tempdir().unwrap();

    // A custom archive of the source.
    let dialog = app.backup_dialog(cx, source, false).expect("the backup");
    let state = app.wait_backup(cx, &dialog, &["ready", "no tool", "old client"]);
    if !state.starts_with("ready") {
        eprintln!("SKIPPED: no usable pg_dump: {state}");
        return;
    }
    app.to_settings(cx, &dialog);
    app.write_to(cx, dir.path(), None);
    app.tap(cx, "backup-start");
    assert!(
        app.wait_backup(cx, &dialog, &["done", "failed"])
            .starts_with("done: ")
    );
    let archive = written(cx, &dialog);
    app.tap(cx, "backup-close");

    // Restored over the target: clean drops what the archive holds first.
    let dialog = app.backup_dialog(cx, target, true).expect("the restore");
    app.wait_backup(cx, &dialog, &["choose a file"]);
    app.tap(cx, "restore-choose-file");
    app.answer_open_prompt(cx, &archive);
    app.wait_backup(cx, &dialog, &["ready"]);
    assert_eq!(
        cx.update(|cx| dialog.read(cx).notes().1).as_deref(),
        Some(
            format!("Custom archive, restored by pg_restore (made from the database “{source}”)")
                .as_str()
        )
    );
    // "Create database" restores into the archive's database: not in one
    // transaction (not run here: it would recreate the source).
    app.tap(cx, "restore-create");
    let now = app.wizard_state(cx, &dialog);
    assert!(
        now.contains("--create") && !now.contains("--single-transaction"),
        "{now}"
    );
    app.tap(cx, "restore-create");
    app.tap(cx, "restore-single-transaction");
    app.tap(cx, "restore-clean");
    app.tap(cx, "restore-if-exists");
    app.tap(cx, "restore-no-owner");
    let now = app.wizard_state(cx, &dialog);
    assert!(
        now.contains("--clean --if-exists --no-owner --single-transaction"),
        "{now}"
    );
    app.tap(cx, "restore-start");
    app.confirm(cx);
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    assert!(state.starts_with("done: pg_restore finished"), "{state}");
    assert_eq!(
        on_database(
            Engine::Postgres,
            Some(target),
            "SELECT string_agg(id::text, ',' ORDER BY id) FROM kept"
        ),
        "1,2,3"
    );
}

#[gpui_kit::test]
fn an_older_client_warns_before_running(cx: &mut TestAppContext) {
    if !fake_tools_run() {
        return;
    }
    if !testing::available(Engine::Postgres) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let old = fake_client(
        &root,
        "old",
        ClientFamily::Postgres,
        "pg_dump (PostgreSQL) 9.6.24",
        "exit 0",
    );
    let newer = fake_client(
        &root,
        "newer",
        ClientFamily::Postgres,
        "pg_dump (PostgreSQL) 98.0",
        "exit 0",
    );
    let app = connected(cx, Engine::Postgres);
    app.use_client(cx, ClientFamily::Postgres, &old);
    let dialog = app
        .backup_dialog(cx, "kerno_other", false)
        .expect("the wizard");
    let state = app.wait_backup(cx, &dialog, &["old client", "ready"]);
    assert!(state.contains("older than the server"), "{state}");
    app.to_settings(cx, &dialog);
    assert!(warning(cx, &dialog).contains("Choose a client"));
    app.tap(cx, "backup-start");
    assert!(app.wizard_state(cx, &dialog).starts_with("old client"));

    // The other installs, the old one marked; a newer one is chosen.
    app.tap(cx, "backup-local-client");
    assert_eq!(cx.update(|cx| dialog.read(cx).page()), "Local client");
    assert!(app.has(cx, ("client-install", 0usize)));
    app.browse_client_from_panel(cx, &newer);
    app.wait_backup(cx, &dialog, &["ready"]);
    assert_eq!(
        cx.update(|cx| dialog.read(cx).client_in_use()),
        Some(newer.clone())
    );

    // MySQL: another version only warns.
    if !testing::available(Engine::MySql) {
        return;
    }
    let app = connected(cx, Engine::MySql);
    let my = fake_client(
        &root,
        "my57",
        ClientFamily::MySql,
        "mysqldump  Ver 10.13 Distrib 5.7.44, for Linux (x86_64)",
        "exit 0",
    );
    app.use_client(cx, ClientFamily::MySql, &my);
    let dialog = app
        .backup_dialog(cx, "kerno_other", false)
        .expect("the wizard");
    app.wait_backup(cx, &dialog, &["ready"]);
    app.to_settings(cx, &dialog);
    assert!(warning(cx, &dialog).contains("This client is version 5"));
}

impl Fixture {
    /// Browse… from the "Local client…" panel already open.
    fn browse_client_from_panel(&self, cx: &mut TestAppContext, dir: &Path) {
        self.tap(cx, "client-browse");
        self.answer_open_prompt(cx, dir);
        // The folder is probed off the UI thread; the panel closes once it
        // is chosen.
        self.wait_until(cx, "the client chosen", |app, cx| {
            !app.has(cx, "client-browse")
        });
    }
}

#[gpui_kit::test]
fn a_connection_overrides_the_local_client(cx: &mut TestAppContext) {
    if !fake_tools_run() {
        return;
    }
    if !testing::available(Engine::Postgres) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let mine = fake_client(
        &root,
        "mine",
        ClientFamily::Postgres,
        "pg_dump (PostgreSQL) 97.0",
        "exit 0",
    );
    let usual = fake_client(
        &root,
        "usual",
        ClientFamily::Postgres,
        "pg_dump (PostgreSQL) 96.0",
        "exit 0",
    );
    let stranger = fake_client(
        &root,
        "stranger",
        ClientFamily::Postgres,
        "pg_dump (PostgreSQL) 99.0",
        "exit 0",
    );

    // The connection's own folder wins over the default.
    let mut config = testing::config(Engine::Postgres);
    config.client_dir = Some(mine.clone());
    let app = connected_as(cx, config);
    app.use_client(cx, ClientFamily::Postgres, &usual);
    cx.update(|cx| {
        AppState::update_settings(cx, |settings| settings.client_folders.push(mine.clone()))
    });
    let dialog = app
        .backup_dialog(cx, "kerno_other", false)
        .expect("the wizard");
    app.wait_backup(cx, &dialog, &["ready"]);
    assert_eq!(
        cx.update(|cx| dialog.read(cx).client_in_use()),
        Some(mine.clone())
    );
    // "Use the default": forgotten for the connection, and saved.
    app.tap(cx, "backup-local-client");
    app.tap(cx, "client-use-default");
    app.wait_backup(cx, &dialog, &["ready"]);
    assert_eq!(
        cx.update(|cx| dialog.read(cx).client_in_use()),
        Some(usual.clone())
    );
    let kept = cx.update(|cx| {
        app.connections.read(cx).saved()[0]
            .config
            .client_dir
            .clone()
    });
    assert_eq!(kept, None);
    app.tap(cx, "backup-close");

    // A folder the connection names but nobody added is never run: the
    // default is, and the wizard says why.
    let mut config = testing::config(Engine::Postgres);
    config.client_dir = Some(stranger.clone());
    let app = connected_as(cx, config);
    app.use_client(cx, ClientFamily::Postgres, &usual);
    let dialog = app
        .backup_dialog(cx, "kerno_other", false)
        .expect("the wizard");
    app.wait_backup(cx, &dialog, &["ready"]);
    assert_eq!(
        cx.update(|cx| dialog.read(cx).client_in_use()),
        Some(usual.clone())
    );
    app.to_settings(cx, &dialog);
    assert!(warning(cx, &dialog).contains(&format!(
        "{} is not a client detected or added",
        stranger.display()
    )));
}

#[gpui_kit::test]
fn settings_lists_local_clients_and_sets_the_default(cx: &mut TestAppContext) {
    if !fake_tools_run() {
        return;
    }
    let app = open(cx, memory_storage());
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let client = fake_client(
        &root,
        "pg",
        ClientFamily::Postgres,
        "pg_dump (PostgreSQL) 99.0",
        "exit 0",
    );
    let empty = root.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    app.press(cx, &["secondary-,"]);
    app.tap(cx, ("settings-section", 5usize));
    app.wait_until(cx, "the clients found", |app, cx| {
        app.has(cx, "local-clients") && !app.has(cx, "local-clients-scanning")
    });

    // A folder with nothing is not added.
    app.tap(cx, "local-clients-add");
    app.answer_open_prompt(cx, &empty);
    app.wait_until(cx, "the refusal", |app, cx| {
        app.has(cx, "local-clients-error")
    });
    assert!(settings_now(cx).client_folders.is_empty());

    // A client folder is, listed first (the newest), marked added.
    app.tap(cx, "local-clients-add");
    app.answer_open_prompt(cx, &client);
    app.wait_until(cx, "the added client", |app, cx| {
        paints(app, cx, "PostgreSQL 99.0 · added")
    });
    // The newest PostgreSQL client: the first row.
    let at = 0usize;
    assert!(paints(&app, cx, "PostgreSQL 99.0 · added"));
    assert_eq!(
        settings_now(cx).client_folders,
        std::slice::from_ref(&client)
    );

    // The default.
    app.tap(cx, ("local-client-default", at));
    assert_eq!(
        settings_now(cx)
            .default_clients
            .get(&ClientFamily::Postgres),
        Some(&client)
    );
    assert!(paints(&app, cx, "PostgreSQL 99.0 · added · default"));
    app.tap(cx, "local-clients-newest-postgres");
    assert!(settings_now(cx).default_clients.is_empty());

    // Removed: from the list and the settings.
    app.tap(cx, ("local-client-default", at));
    app.tap(cx, ("local-client-remove", at));
    app.wait_until(cx, "the client gone", |app, cx| {
        !app.has(cx, "local-clients-scanning") && !paints(app, cx, "PostgreSQL 99.0 · added")
    });
    let now = settings_now(cx);
    assert!(now.client_folders.is_empty() && now.default_clients.is_empty());
}

#[gpui_kit::test]
fn a_global_backup_writes_the_roles(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    let dir = tempfile::tempdir().unwrap();
    let dialog = app
        .wizard(cx, "kerno", Mode::GlobalBackup)
        .expect("the global backup");
    let state = app.wait_backup(cx, &dialog, &["ready", "no tool", "old client"]);
    if !state.starts_with("ready") {
        eprintln!("SKIPPED: no usable pg_dumpall: {state}");
        return;
    }
    app.wait_until(cx, "the databases", |_, cx| {
        cx.update(|cx| dialog.read(cx).objects_loaded())
    });
    assert_eq!(cx.update(|cx| dialog.read(cx).page()), "Databases");
    assert!(app.has(cx, ("global-database", 0usize)));
    app.tap(cx, "global-databases-none");
    app.tap(cx, "wizard-next");
    assert!(
        app.wizard_state(cx, &dialog)
            .contains("Tick at least one database")
    );
    // Roles and tablespaces only, never their passwords: it reads, and no
    // other test's databases come in.
    app.tap(cx, "global-globals-only");
    let now = app.wizard_state(cx, &dialog);
    assert!(
        now.contains("--globals-only --no-role-passwords") && !now.contains("--exclude-database"),
        "{now}"
    );
    app.write_to(cx, dir.path(), Some("globals"));
    app.tap(cx, "backup-start");
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    assert!(state.starts_with("done: pg_dumpall finished"), "{state}");
    let dump = std::fs::read_to_string(written(cx, &dialog)).unwrap();
    assert!(dump.contains("CREATE ROLE kerno"), "{dump}");
}

#[gpui_kit::test]
fn a_script_runs_after_its_command_is_confirmed(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let (cleanup, setup) = match engine {
            Engine::Postgres => (
                "DROP SCHEMA IF EXISTS kerno_ui_wizard_script CASCADE",
                "CREATE SCHEMA kerno_ui_wizard_script",
            ),
            _ => (
                "DROP DATABASE IF EXISTS kerno_ui_wizard_script",
                "CREATE DATABASE kerno_ui_wizard_script",
            ),
        };
        let _scratch = Scratch::new(engine, &[cleanup], &[setup]);
        let app = connected(cx, engine);
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fix.sql");
        std::fs::write(
            &script,
            "CREATE TABLE kerno_ui_wizard_script.done (id int);\n\
             INSERT INTO kerno_ui_wizard_script.done VALUES (1);\n",
        )
        .unwrap();
        let dialog = app
            .wizard(cx, "kerno", Mode::ExecuteScript)
            .expect("the script wizard");
        app.wait_backup(cx, &dialog, &["choose a file"]);
        app.tap(cx, "restore-choose-file");
        app.answer_open_prompt(cx, &script);
        let state = app.wait_backup(cx, &dialog, &["ready", "no tool", "old client", "refused"]);
        if state.starts_with("no tool") || state.starts_with("old client") {
            eprintln!("SKIPPED ({engine:?}): no usable client: {state}");
            continue;
        }
        let expected = match engine {
            Engine::Postgres => "--echo-errors",
            _ => "--no-beep",
        };
        assert!(
            state.starts_with("ready: ") && state.contains(expected),
            "{engine:?}: {state}"
        );
        app.tap(cx, "restore-start");
        app.confirm(cx);
        let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
        assert!(
            state.contains(" finished (exit status 0). Ran "),
            "{engine:?}: {state}"
        );
        assert_eq!(
            on_server(engine, "SELECT count(*) FROM kerno_ui_wizard_script.done"),
            "1",
            "{engine:?}"
        );
    }
}
