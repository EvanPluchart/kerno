//! Saved tasks (M10 Part 11b): saved from a wizard, listed in the Tasks
//! view, run again, edited, duplicated, renamed, deleted; no secret kept;
//! a task of a deleted connection cannot run; a destructive task asks
//! before each run and follows its connection's rules as they are then.
//!
//! The runs use stand-in clients (shell scripts in a temporary folder,
//! added as a local client), as `tool_wizards.rs` does: nothing needs a
//! real client tool.

use std::path::PathBuf;

use kerno_core::storage::{HistoryStatus, SavedTask};
use kerno_core::tools::{BackupOptions, ClientFamily, ScriptOptions, TaskSettings};

use super::tool_wizards::{fake_client, paints};
use super::*;
use crate::backup_dialog::BackupDialog;
use crate::tasks_view::{TaskAction, TasksView};

impl Fixture {
    /// Opens the Tasks view (Tools ▸ Tasks), once its list is read.
    fn tasks_view(&self, cx: &mut TestAppContext) -> Entity<TasksView> {
        let workspace = self.workspace.clone();
        let view = self.step(cx, |window, cx| {
            workspace.update(cx, |workspace, cx| workspace.open_tasks(window, cx))
        });
        self.wait_until(cx, "the tasks", |app, cx| {
            let stored = app.storage.tasks().unwrap().len();
            cx.update(|cx| view.read(cx).listed().len()) == stored
        });
        view
    }

    /// Does what an entry of a task's menu does.
    fn task_does(
        &self,
        cx: &mut TestAppContext,
        view: &Entity<TasksView>,
        action: TaskAction,
        id: i64,
    ) {
        self.step(cx, |window, cx| {
            view.update(cx, |view, cx| view.perform(action, id, window, cx))
        });
    }

    /// The wizard the Tasks view opened last.
    fn task_wizard(
        &self,
        cx: &mut TestAppContext,
        view: &Entity<TasksView>,
    ) -> Entity<BackupDialog> {
        self.wait_until(cx, "the task's wizard", |_, cx| {
            cx.update(|cx| view.read(cx).opened()).is_some()
        });
        cx.update(|cx| view.read(cx).opened()).unwrap()
    }

    fn stored_tasks(&self) -> Vec<SavedTask> {
        self.storage.tasks().unwrap()
    }

    /// Saves the open wizard's settings as a task named `name`.
    fn save_as_task(&self, cx: &mut TestAppContext, name: &str) {
        let before = self.stored_tasks().len();
        self.tap(cx, "wizard-save-as-task");
        assert!(self.has(cx, "task-name"), "the name is asked");
        self.replace_text(cx, "task-name", name);
        self.click(cx, "task-name-ok");
        self.wait_until(cx, "the saved task", |app, _| {
            app.stored_tasks().len() == before + 1
        });
    }
}

/// A stand-in PostgreSQL client whose pg_dump writes "dumped" into its
/// `--file` and prints the password it was given.
fn dumping_client(root: &Path) -> PathBuf {
    fake_client(
        root,
        "pg99",
        ClientFamily::Postgres,
        "pg_dump (PostgreSQL) 99.0",
        r#"for arg in "$@"; do case $arg in --file=*) echo dumped > "${arg#--file=}";; esac; done
echo "pw=$PGPASSWORD""#,
    )
}

#[gpui_kit::test]
fn a_backup_task_is_saved_listed_run_again_edited_duplicated_and_deleted(cx: &mut TestAppContext) {
    if !super::tool_wizards::fake_tools_run() {
        return;
    }
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    app.use_client(cx, ClientFamily::Postgres, &dumping_client(&root));

    // "Save as task…" on the wizard's last step.
    let dialog = app
        .backup_dialog(cx, "kerno_other", false)
        .expect("the wizard");
    app.wait_backup(cx, &dialog, &["ready"]);
    app.to_settings(cx, &dialog);
    app.write_to(cx, &root, Some("kerno_ui_task_dump"));
    let file = cx
        .update(|cx| dialog.read(cx).file_preview(cx))
        .expect("the file");
    app.save_as_task(cx, "kerno_ui_task nightly");
    let task = app.stored_tasks()[0].clone();
    let connection = cx.update(|cx| app.connections.read(cx).saved()[0].id.clone());
    assert_eq!(task.name, "kerno_ui_task nightly");
    assert_eq!(task.connection_id, connection);
    let TaskSettings::Backup(options) = &task.settings else {
        panic!("{:?}", task.settings)
    };
    assert_eq!(
        (options.database.as_str(), options.output_folder.as_path()),
        ("kerno_other", root.as_path())
    );
    assert_eq!(options.file_pattern, "kerno_ui_task_dump");
    // The wizard is now the task's: "Save task" writes it back.
    assert!(app.has(cx, "wizard-save-task"));
    app.tap(cx, "backup-close");

    // Listed in the Tasks view, never run.
    let view = app.tasks_view(cx);
    assert!(app.has(cx, ("task-row", 0usize)));
    assert!(paints(&app, cx, "kerno_ui_task nightly") && paints(&app, cx, "never run"));

    // Run: the wizard opens on the task's settings and starts by itself;
    // the run is kept with the task, without the password.
    assert!(!file.exists());
    app.task_does(cx, &view, TaskAction::Run, task.id);
    app.wait_until(cx, "the run kept with the task", |app, _| {
        app.stored_tasks()[0].last_status == Some(HistoryStatus::Ok)
    });
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "dumped\n");
    let log = app.stored_tasks()[0].last_log.clone();
    assert!(
        log.starts_with("$ ")
            && log.contains("PGPASSWORD=•••")
            && log.contains("--dbname=kerno_other"),
        "{log}"
    );
    assert!(
        log.contains("pw=•••") && !log.contains(testing::DB_PASSWORD),
        "{log}"
    );
    let wizard = app.task_wizard(cx, &view);
    assert!(
        cx.update(|cx| wizard.read(cx).log())
            .contains(&"pw=•••".to_string())
    );
    app.wait_until(cx, "the list shows the run", |app, cx| {
        paints(app, cx, "succeeded")
    });
    app.tap(cx, "backup-close");

    // Run again: the file is written again.
    std::fs::remove_file(&file).unwrap();
    let first_run = app.stored_tasks()[0].last_run_at_ms;
    app.task_does(cx, &view, TaskAction::Run, task.id);
    app.wait_until(cx, "the second run", |app, _| {
        app.stored_tasks()[0].last_run_at_ms > first_run
    });
    assert!(file.exists());
    app.tap(cx, "backup-close");

    // Edit: the wizard on the saved settings; "Save task" keeps the
    // change and the last run.
    app.task_does(cx, &view, TaskAction::Edit, task.id);
    let wizard = app.task_wizard(cx, &view);
    app.wait_backup(cx, &wizard, &["ready"]);
    assert_eq!(
        cx.update(|cx| wizard.read(cx).file_preview(cx)),
        Ok(file.clone())
    );
    app.to_settings(cx, &wizard);
    app.replace_text(cx, "backup-pattern", "kerno_ui_task_edited");
    app.tap(cx, "wizard-save-task");
    app.wait_until(cx, "the edited task", |app, _| {
        matches!(
            &app.stored_tasks()[0].settings,
            TaskSettings::Backup(options) if options.file_pattern == "kerno_ui_task_edited"
        )
    });
    assert_eq!(app.stored_tasks().len(), 1);
    assert_eq!(app.stored_tasks()[0].last_status, Some(HistoryStatus::Ok));
    app.tap(cx, "backup-close");

    // Duplicate, then rename the copy.
    app.task_does(cx, &view, TaskAction::Duplicate, task.id);
    app.wait_until(cx, "the copy, listed", |app, cx| {
        app.stored_tasks().len() == 2 && cx.update(|cx| view.read(cx).listed().len()) == 2
    });
    let copy = app
        .stored_tasks()
        .into_iter()
        .find(|stored| stored.name == "kerno_ui_task nightly (copy)")
        .expect("the copy");
    assert_eq!(copy.settings, app.stored_tasks()[0].settings);
    assert!(copy.last_run_at_ms.is_none() && copy.last_log.is_empty());
    app.task_does(cx, &view, TaskAction::Rename, copy.id);
    app.replace_text(cx, "task-name", "kerno_ui_task weekly");
    app.click(cx, "task-name-ok");
    app.wait_until(cx, "the renamed copy", |app, _| {
        app.stored_tasks()
            .iter()
            .any(|stored| stored.id == copy.id && stored.name == "kerno_ui_task weekly")
    });

    // Delete asks first.
    app.task_does(cx, &view, TaskAction::Delete, copy.id);
    assert!(app.has(cx, "task-delete-confirm"));
    app.click(cx, "task-delete-cancel");
    assert_eq!(app.stored_tasks().len(), 2);
    app.task_does(cx, &view, TaskAction::Delete, copy.id);
    app.click(cx, "task-delete-confirm");
    app.wait_until(cx, "the copy deleted", |app, cx| {
        app.stored_tasks().len() == 1 && cx.update(|cx| view.read(cx).listed().len()) == 1
    });

    // A real right-click on the task's row: its menu, and its log.
    app.right_click(cx, ("task-row", 0usize), None);
    for label in [
        "Run",
        "Edit…",
        "Duplicate",
        "Rename…",
        "Delete…",
        "Show log",
    ] {
        assert!(menu_entry(&app, cx, label).is_some(), "{label}");
    }
    click_menu_entry(&app, cx, "Show log");
    assert!(app.has(cx, "task-log"));
}

#[gpui_kit::test]
fn a_task_whose_connection_was_deleted_says_so_and_cannot_run(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    let options = BackupOptions::new(Engine::Postgres, "kerno");
    let orphan = SavedTask::new(
        "kerno_ui_task orphan",
        "a-deleted-connection",
        TaskSettings::Backup(options),
    );
    let id = app.storage.save_task(&orphan).unwrap();
    let view = app.tasks_view(cx);
    assert!(paints(&app, cx, "deleted connection"));
    let menu = cx.update(|cx| view.read(cx).menu_of(id, cx));
    assert!(menu.contains(&(TaskAction::Run, Some("its connection was deleted"))));
    assert!(menu.contains(&(TaskAction::Edit, Some("its connection was deleted"))));
    assert!(menu.contains(&(TaskAction::Delete, None)));
    // Run says why it cannot (a notification), and opens nothing.
    let before = notification_count(&app, cx);
    app.task_does(cx, &view, TaskAction::Run, id);
    assert!(!app.has(cx, "backup-dialog"));
    assert_eq!(notification_count(&app, cx), before + 1);
}

#[gpui_kit::test]
fn a_task_keeps_no_password_and_asks_the_override_s_at_each_run(cx: &mut TestAppContext) {
    if !super::tool_wizards::fake_tools_run() {
        return;
    }
    if !testing::available(Engine::Postgres) {
        return;
    }
    let secret = "kerno_ui_task_secret_pw";
    let app = connected(cx, Engine::Postgres);
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    app.use_client(cx, ClientFamily::Postgres, &dumping_client(&root));
    let dialog = app
        .backup_dialog(cx, "kerno_other", false)
        .expect("the wizard");
    app.wait_backup(cx, &dialog, &["ready"]);
    app.to_settings(cx, &dialog);
    app.write_to(cx, &root, Some("kerno_ui_task_auth"));
    app.tap(cx, "backup-auth");
    app.replace_text(cx, "backup-auth-user", "kerno_ui_task_user");
    app.tap(cx, "backup-auth-password");
    app.type_text(cx, secret);
    app.save_as_task(cx, "kerno_ui_task as another user");
    let task = app.stored_tasks()[0].clone();
    assert_eq!(task.settings.auth_user(), Some("kerno_ui_task_user"));
    let json = serde_json::to_string(&task.settings).unwrap();
    assert!(!json.contains(secret), "{json}");
    app.tap(cx, "backup-close");

    // A run waits for the password, typed again.
    let view = app.tasks_view(cx);
    let before = notification_count(&app, cx);
    app.task_does(cx, &view, TaskAction::Run, task.id);
    let wizard = app.task_wizard(cx, &view);
    app.wait_backup(cx, &wizard, &["ready"]);
    // Said with a notification; the wizard waits on its settings.
    assert_eq!(notification_count(&app, cx), before + 1);
    assert_eq!(cx.update(|cx| wizard.read(cx).page()), "Settings");
    assert!(!cx.update(|cx| wizard.read(cx).ready_to_start(cx)));
    assert!(app.stored_tasks()[0].last_run_at_ms.is_none());
    app.tap(cx, "backup-auth-password");
    app.type_text(cx, secret);
    app.tap(cx, "backup-start");
    app.wait_until(cx, "the run", |app, _| {
        app.stored_tasks()[0].last_status == Some(HistoryStatus::Ok)
    });
    // The tool got it; neither the screen nor the task's log show it.
    let log = app.stored_tasks()[0].last_log.clone();
    assert!(log.contains("pw=•••") && !log.contains(secret), "{log}");
    assert!(
        !cx.update(|cx| wizard.read(cx).log())
            .concat()
            .contains(secret)
    );
    assert!(!paints(&app, cx, secret));
}

#[gpui_kit::test]
fn a_script_task_asks_before_each_run_and_follows_the_connection_s_rules(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let client = fake_client(
        &root,
        "psql99",
        ClientFamily::Postgres,
        "psql (PostgreSQL) 99.0",
        "echo ran",
    );
    app.use_client(cx, ClientFamily::Postgres, &client);
    let script = root.join("kerno_ui_task_script.sql");
    std::fs::write(&script, "select 1;\n").unwrap();
    let connection = cx.update(|cx| app.connections.read(cx).saved()[0].id.clone());
    let task = SavedTask::new(
        "kerno_ui_task script",
        connection.clone(),
        TaskSettings::ExecuteScript(ScriptOptions::new(Engine::Postgres, "kerno_other", &script)),
    );
    let id = app.storage.save_task(&task).unwrap();
    let view = app.tasks_view(cx);

    // Each run shows the command and asks.
    for run in 0..2 {
        app.task_does(cx, &view, TaskAction::Run, id);
        app.wait_until(cx, "the confirmation", |app, cx| app.has(cx, "sql-preview"));
        assert!(paints(&app, cx, "kerno_ui_task_script.sql"));
        if run == 0 {
            app.click(cx, "sql-confirm");
            app.wait_until(cx, "the run", |app, _| {
                app.stored_tasks()[0].last_status == Some(HistoryStatus::Ok)
            });
        } else {
            // Declined: nothing ran, the last run stays the first.
            let before = app.stored_tasks()[0].last_run_at_ms;
            app.click(cx, "sql-cancel");
            assert_eq!(app.stored_tasks()[0].last_run_at_ms, before);
        }
        app.tap(cx, "backup-close");
    }

    // The connection is now read-only: the task is refused.
    app.step(cx, |_, cx| {
        app.connections.update(cx, |connections, cx| {
            connections.set_read_only(&connection, true, cx)
        })
    });
    app.wait_until(cx, "the connection open again", |app, cx| {
        matches!(app.status(cx, 0), Status::Connected { .. })
    });
    let before = notification_count(&app, cx);
    app.task_does(cx, &view, TaskAction::Run, id);
    app.wait_until(cx, "the refusal", |app, cx| {
        notification_count(app, cx) > before
    });
    assert!(!app.has(cx, "backup-dialog"));
    assert!(cx.update(|cx| view.read(cx).opened()).is_none());
}

#[gpui_kit::test]
fn the_tasks_view_reads_its_list_when_it_opens_not_at_each_frame(cx: &mut TestAppContext) {
    // The window renders its active tab at every frame: a read of the list
    // from there would read, redraw and read again without end.
    let app = open(cx, memory_storage());
    let options = BackupOptions::new(Engine::Postgres, "kerno");
    app.storage
        .save_task(&SavedTask::new(
            "kerno_ui_task once",
            "a-connection",
            TaskSettings::Backup(options),
        ))
        .unwrap();
    let view = app.tasks_view(cx);
    let reads = |cx: &mut TestAppContext| cx.update(|cx| view.read(cx).reads());
    assert_eq!(reads(cx), 1);
    for _ in 0..5 {
        app.step(cx, |window, _| window.refresh());
    }
    assert_eq!(reads(cx), 1, "frames read nothing");
    // Opened again (Tools ▸ Tasks): read once more, then still.
    let again = app.tasks_view(cx);
    assert_eq!(again, view);
    app.step(cx, |window, _| window.refresh());
    assert_eq!(reads(cx), 2);
}
