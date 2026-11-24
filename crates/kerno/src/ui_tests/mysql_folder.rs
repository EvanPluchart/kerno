//! K63: the faster backup of MySQL and MariaDB, several mysqldump at once
//! into a folder that the restore loads on several mysql clients; the
//! consistency choice, with and without the RELOAD privilege; an old server
//! that needs an old client (docs/plans/M12.md, D-135).

use kerno_core::tools::{BackupFormat, ClientFamily, DumpMethod, EngineBackup, mysql_folder};

use super::tool_wizards::{fake_client, paints};
use super::*;
use crate::backup_dialog::BackupDialog;

fn method(cx: &mut TestAppContext, dialog: &Entity<BackupDialog>) -> (DumpMethod, Option<u32>) {
    let task = cx
        .update(|cx| dialog.read(cx).task_backup(cx))
        .expect("the settings");
    match task.engine {
        EngineBackup::MySql(my) => (my.method, my.jobs),
        other => panic!("{other:?}"),
    }
}

/// The round trip from the simple views, with the real tools on MySQL 8.4:
/// "Faster" backs a scratch database up into a folder with its manifest;
/// the simple restore offers that folder (the open panel takes folders),
/// reads it as Kerno's folder backup, and loads it on several clients; a
/// folder without the manifest is not offered as a backup.
#[gpui_kit::test]
fn a_faster_mysql_backup_is_a_folder_the_restore_loads_on_several_clients(cx: &mut TestAppContext) {
    let engine = Engine::MySql;
    if !testing::available(engine) {
        return;
    }
    let (source, target) = ("kerno_test_k63_ui_src", "kerno_test_k63_ui_dst");
    let drop = |name: &str| format!("DROP DATABASE IF EXISTS {name}");
    let _scratch = Scratch::new(
        engine,
        &[&drop(source), &drop(target)],
        &[
            &format!("CREATE DATABASE {source}"),
            &format!("CREATE DATABASE {target}"),
            &format!("CREATE TABLE {source}.one (id INT PRIMARY KEY, label VARCHAR(20))"),
            &format!("CREATE TABLE {source}.two (label VARCHAR(20)) ENGINE=MyISAM"),
            &format!("CREATE VIEW {source}.one_view AS SELECT id FROM {source}.one"),
            &format!("INSERT INTO {source}.one VALUES (1, 'a'), (2, 'b'), (3, 'c')"),
            &format!("INSERT INTO {source}.two VALUES ('x'), ('y')"),
        ],
    );
    let app = connected(cx, engine);
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let jobs = crate::backup_dialog::faster_jobs();

    let dialog = app.simple_dialog(cx, source, false).expect("the backup");
    let state = app.wait_backup(cx, &dialog, &["ready", "no tool", "old client"]);
    if !state.starts_with("ready") {
        eprintln!("SKIPPED: no usable client tool: {state}");
        return;
    }
    // Off by default: one SQL file; no consistency row.
    assert!(app.has(cx, "simple-faster") && !app.has(cx, "simple-block-writes"));
    app.tap(cx, "simple-faster");
    assert_eq!(
        method(cx, &dialog),
        (DumpMethod::SingleTransaction, Some(jobs))
    );
    assert!(paints(&app, cx, "A folder of SQL scripts, one per table"));
    assert!(app.has(cx, "simple-block-writes"));
    assert!(paints(&app, cx, "Each table is read at its own moment"));
    let folder = root.join("k63");
    app.save_as(cx, "simple-choose-file", &folder);
    assert!(paints(&app, cx, &format!("{}/", folder.display())));
    let state = app.wizard_state(cx, &dialog);
    assert!(
        state.contains("# one mysqldump per table") && !state.contains(testing::DB_PASSWORD),
        "{state}"
    );
    app.tap(cx, "backup-start");
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    assert!(state.starts_with("done: mysqldump finished"), "{state}");
    let manifest = mysql_folder::read_manifest(&folder).expect("the manifest");
    assert_eq!(manifest.tables.len(), 2);
    assert!(state.ends_with(").") && state.contains(&folder.display().to_string()));
    assert!(
        cx.update(|cx| dialog.read(cx).log())
            .iter()
            .any(|line| line.starts_with("[one] ")),
        "the log names each table"
    );
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
        "Kerno's folder backup, one SQL file per table, run by the mysql client (made from the \
         database “kerno_test_k63_ui_src”)"
    ));
    app.tap(cx, "simple-parallel");
    assert!(
        cx.update(|cx| dialog.read(cx).task_restore(cx))
            .is_some_and(|restore| { restore.jobs == Some(jobs) && !restore.single_transaction })
    );
    app.tap(cx, "restore-start");
    app.wait_until(cx, "the confirmation", |app, cx| app.has(cx, "sql-preview"));
    app.tap(cx, "sql-confirm");
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    assert!(state.starts_with("done: mysql finished"), "{state}");
    assert_eq!(
        on_server(
            engine,
            &format!(
                "SELECT CONCAT((SELECT COUNT(*) FROM {target}.one), '/', \
                 (SELECT COUNT(*) FROM {target}.two), '/', (SELECT COUNT(*) FROM {target}.one_view))"
            )
        ),
        "3/2/3"
    );
    app.tap(cx, "backup-close");

    // A folder without the manifest is not a Kerno backup.
    let other = root.join("other");
    std::fs::create_dir(&other).unwrap();
    std::fs::write(other.join("a.sql"), "SELECT 1;\n").unwrap();
    let dialog = app.simple_dialog(cx, target, true).expect("the restore");
    app.wait_backup(cx, &dialog, &["choose a file"]);
    app.tap(cx, "restore-choose-file");
    app.answer_open_prompt(cx, &other);
    let state = app.wait_backup(cx, &dialog, &["ready", "file"]);
    assert!(
        state.contains("is a folder without kerno-backup.json: not a backup Kerno made"),
        "{state}"
    );
}

/// "Block writes": shown with "Faster", off by default, said in plain words
/// either way; ticked, it is the folder backup's read lock, kept by the
/// wizard and a task, not a hidden change, and Start asks first, showing
/// the statement. An account without RELOAD: said, not offered, and a
/// lock already chosen refuses Start until it is unticked.
#[gpui_kit::test]
fn the_consistency_choice_is_said_and_needs_reload(cx: &mut TestAppContext) {
    let engine = Engine::MySql;
    if !testing::available(engine) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    // A stand-in that never runs here: Start stops at the confirmation.
    let client = fake_client(
        &root,
        "mysql",
        ClientFamily::MySql,
        "mysqldump  Ver 8.4.6 for macos15 on arm64",
        "exit 0",
    );
    let app = connected(cx, engine);
    app.use_client(cx, ClientFamily::MySql, &client);
    let dialog = app
        .simple_dialog(cx, "kerno_other", false)
        .expect("the backup");
    app.wait_backup(cx, &dialog, &["ready"]);
    app.wait_until(cx, "the objects", |_, cx| {
        cx.update(|cx| dialog.read(cx).objects_loaded())
    });
    let jobs = crate::backup_dialog::faster_jobs();
    app.tap(cx, "simple-faster");
    app.save_as(cx, "simple-choose-file", &root.join("locked"));
    app.tap(cx, "simple-block-writes");
    assert_eq!(method(cx, &dialog), (DumpMethod::LockAllTables, Some(jobs)));
    assert!(paints(
        &app,
        cx,
        "Kerno holds a read lock on the whole server (FLUSH TABLES WITH READ LOCK)"
    ));
    // The wizard shows it as its own consistency and format; back without
    // a question.
    app.tap(cx, "backup-advanced");
    app.to_settings(cx, &dialog);
    assert!(app.has(cx, "backup-jobs"));
    let settings = cx.update(|cx| dialog.read(cx).settings(cx)).unwrap();
    assert_eq!(settings.format, BackupFormat::Directory);
    app.tap(cx, "wizard-simple");
    assert!(!app.has(cx, "simple-reset"));
    assert_eq!(method(cx, &dialog), (DumpMethod::LockAllTables, Some(jobs)));
    // Start asks first, with the statement.
    app.tap(cx, "backup-start");
    app.wait_until(cx, "the confirmation", |app, cx| app.has(cx, "sql-preview"));
    assert!(paints(&app, cx, "FLUSH TABLES WITH READ LOCK"));
    app.tap(cx, "sql-cancel");
    assert!(app.wizard_state(cx, &dialog).starts_with("ready: "));

    // Without RELOAD: said; the lock chosen refuses Start until unticked,
    // then the choice cannot be ticked again.
    app.step(cx, |_, cx| {
        dialog.update(cx, |dialog, cx| dialog.set_can_lock(Some(false), cx))
    });
    assert!(paints(&app, cx, "This account lacks the RELOAD privilege"));
    assert!(!cx.update(|cx| dialog.read(cx).ready_to_start(cx)));
    app.tap(cx, "simple-block-writes");
    assert_eq!(method(cx, &dialog).0, DumpMethod::SingleTransaction);
    assert!(cx.update(|cx| dialog.read(cx).ready_to_start(cx)));
    app.tap(cx, "simple-block-writes");
    assert_eq!(method(cx, &dialog).0, DumpMethod::SingleTransaction);
    // Unticking "Faster" gives the plain script back.
    app.tap(cx, "simple-faster");
    assert_eq!(method(cx, &dialog), (DumpMethod::SingleTransaction, None));
    assert!(!app.has(cx, "simple-block-writes"));
}

/// An old server with a client known not to read it (mysqldump 26.10 on
/// MySQL 5.1): said before the run with the client that works, the plain
/// and the faster backup refused; the tools go without TLS there, as the
/// connection with 'prefer' does.
#[gpui_kit::test]
fn an_old_server_needs_an_old_client(cx: &mut TestAppContext) {
    let Some((_, params)) = testing::legacy_mysql()
        .into_iter()
        .find(|(name, _)| *name == "kerno-mysql51")
    else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let old = fake_client(
        &root,
        "old",
        ClientFamily::MySql,
        "mysqldump  Ver 10.13 Distrib 5.7.99, for osx10.19 (x86_64)",
        "exit 0",
    );
    let new = fake_client(
        &root,
        "new",
        ClientFamily::MySql,
        "mysqldump  Ver 26.10.0 for macos27.0 on arm64 (Homebrew)",
        "exit 0",
    );
    // TLS "prefer", as a new connection is: MySQL 5.1 offers TLS 1.0 only,
    // so the connection goes without it, and so do the tools.
    let mut config = testing::config(Engine::MySql);
    config.port = params.port;
    config.tls.mode = kerno_core::config::TlsMode::Prefer;
    let app = connected_as(cx, config);
    app.use_client(cx, ClientFamily::MySql, &old);
    app.use_client(cx, ClientFamily::MySql, &new);
    let dialog = app.simple_dialog(cx, "kerno", false).expect("the backup");
    let state = app.wait_backup(cx, &dialog, &["ready", "old client"]);
    let said = format!(
        "This mysqldump (26.10.0) reads no column of this server's tables: a backup of MySQL \
         5.1.73 with it would hold no table, or tables without their rows. Choose a MySQL 5.7 \
         client (5.7.11 or later) with “Local client…”: the one in {} (5.7.99) works.",
        old.display()
    );
    assert_eq!(state, format!("old client: {said}"));
    assert!(paints(&app, cx, &said));
    assert!(!cx.update(|cx| dialog.read(cx).ready_to_start(cx)));
    // The faster backup too.
    app.tap(cx, "simple-faster");
    assert!(app.wizard_state(cx, &dialog).starts_with("old client: "));
    // The client that works: ready.
    app.use_client(cx, ClientFamily::MySql, &old);
    app.tap(cx, "backup-close");
    let dialog = app.simple_dialog(cx, "kerno", false).expect("the backup");
    let state = app.wait_backup(cx, &dialog, &["ready", "old client"]);
    assert!(
        state.starts_with("ready: ") && state.contains(" --ssl-mode=DISABLED "),
        "{state}"
    );
}
