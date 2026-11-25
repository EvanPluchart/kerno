//! K63: the faster backup of MySQL and MariaDB, several mysqldump at once
//! into a folder that the restore loads on several mysql clients; the
//! consistency choice, with and without the RELOAD privilege; an old server
//! that needs an old client (docs/plans/M12.md, D-135). K74: a restore
//! again into a database that holds the backup's tables, said before and
//! after (D-143).

use kerno_core::tools::{
    BackupContent, BackupFormat, ClientFamily, DumpMethod, EngineBackup, MySqlBackup, mysql_folder,
};

use super::tool_wizards::{fake_client, fake_tools_run, paints};
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
            // A database that is not empty: `one` is the backup's too.
            &format!("CREATE TABLE {target}.one (x INT)"),
            &format!("CREATE TABLE {target}.extra (id INT)"),
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
    assert!(paints(
        &app,
        cx,
        "Each table is read at one moment, all its parts included"
    ));
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
    app.wait_until(cx, "the database's tables", |app, cx| {
        paints(
            app,
            cx,
            "“kerno_test_k63_ui_dst” already holds 1 of the backup's 2 tables (“one”): each is \
             dropped and replaced with the backup's table and rows; the tables only in \
             “kerno_test_k63_ui_dst” stay.",
        )
    });
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
                 (SELECT COUNT(*) FROM {target}.two), '/', (SELECT COUNT(*) FROM {target}.one_view), \
                 '/', (SELECT COUNT(*) FROM {target}.extra))"
            )
        ),
        "3/2/3/0"
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
    // The Consistency row says what each choice does to the folder (G7).
    for (at, said) in [
        (
            2usize,
            "In the folder, with no lock at all, no table is cut",
        ),
        (0, "In the folder, each table is read at one moment"),
        (
            1,
            "In the folder, Kerno holds the server's read lock for the whole backup",
        ),
    ] {
        app.tap(cx, ("backup-method", at));
        assert!(paints(&app, cx, said), "{said}");
    }
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

/// A plain backup whose mysqldump reads no column of a table ("it has no
/// fields") still exits 0: a failure, its file removed (K63, F6).
#[gpui_kit::test]
fn a_plain_backup_that_read_no_column_fails_and_removes_its_file(cx: &mut TestAppContext) {
    // The stand-in mysqldump must really run: it is what reads no column.
    if !testing::available(Engine::MySql) || !fake_tools_run() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let client = fake_client(
        &root,
        "nofields",
        ClientFamily::MySql,
        "mysqldump  Ver 8.4.6 for macos15 on arm64",
        r#"for arg in "$@"; do case $arg in --result-file=*) echo '-- MySQL dump' > "${arg#--result-file=}";; esac; done
echo "-- Skipping dump data for table 't', it has no fields" >&2
exit 0"#,
    );
    let app = connected(cx, Engine::MySql);
    app.use_client(cx, ClientFamily::MySql, &client);
    let dialog = app
        .simple_dialog(cx, "kerno_other", false)
        .expect("the backup");
    app.wait_backup(cx, &dialog, &["ready"]);
    let file = root.join("empty.sql");
    app.save_as(cx, "simple-choose-file", &file);
    app.tap(cx, "backup-start");
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    assert!(
        state.starts_with("failed: mysqldump found no columns in a table")
            && state.ends_with("The incomplete file was removed."),
        "{state}"
    );
    assert!(!file.exists());
}

/// K74, with the real tools on MySQL 8.4: a folder backup of a parent and
/// a child linked by a foreign key, restored with "Faster" into an empty
/// database, then again from the same wizard (Back): the second time the
/// wizard and the confirmation say that the backup's tables there are
/// dropped and replaced, how many, and what a stopped restore leaves. A
/// file that fails after its table's drop: the failure says which tables
/// are empty or missing, as the database holds them, and what to do; run
/// again, everything is in.
#[gpui_kit::test]
fn a_restore_again_says_what_it_replaces_and_a_failure_what_it_left(cx: &mut TestAppContext) {
    let engine = Engine::MySql;
    if !testing::available(engine) {
        return;
    }
    let (source, target) = ("kerno_test_k74_ui_src", "kerno_test_k74_ui_dst");
    let drop = |name: &str| format!("DROP DATABASE IF EXISTS {name}");
    let _scratch = Scratch::new(
        engine,
        &[&drop(source), &drop(target)],
        &[
            &format!("CREATE DATABASE {source}"),
            &format!("CREATE DATABASE {target}"),
            &format!("CREATE TABLE {target}.extra (id INT)"),
            &format!("CREATE TABLE {source}.parent (id INT PRIMARY KEY, code VARCHAR(9) UNIQUE)"),
            &format!(
                "CREATE TABLE {source}.child (id INT PRIMARY KEY, parent_id INT, FOREIGN KEY \
                 (parent_id) REFERENCES {source}.parent (id))"
            ),
            &format!("INSERT INTO {source}.parent VALUES (1, 'a'), (2, 'b'), (3, 'c')"),
            &format!("INSERT INTO {source}.child VALUES (1, 3), (2, 2)"),
        ],
    );
    let app = connected(cx, engine);
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();

    let dialog = app.simple_dialog(cx, source, false).expect("the backup");
    let state = app.wait_backup(cx, &dialog, &["ready", "no tool", "old client"]);
    if !state.starts_with("ready") {
        eprintln!("SKIPPED: no usable client tool: {state}");
        return;
    }
    app.tap(cx, "simple-faster");
    let folder = root.join("k74");
    app.save_as(cx, "simple-choose-file", &folder);
    app.tap(cx, "backup-start");
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    assert!(state.starts_with("done: "), "{state}");
    app.tap(cx, "backup-close");

    let counts = || {
        on_server(
            engine,
            &format!(
                "SELECT CONCAT((SELECT COUNT(*) FROM {target}.parent), '/', \
                 (SELECT COUNT(*) FROM {target}.child), '/', (SELECT COUNT(*) FROM {target}.extra))"
            ),
        )
    };
    let replaced = format!(
        "“{target}” already holds all 2 of the backup's tables (“child” and “parent”): each is \
         dropped and replaced with the backup's table and rows; the tables only in “{target}” \
         stay. Kerno creates the backup's tables first, one after the other, then loads their \
         rows: if the restore stops, a table it has not finished is left empty or missing until \
         you run it again."
    );
    let dialog = app.simple_dialog(cx, target, true).expect("the restore");
    app.wait_backup(cx, &dialog, &["choose a file"]);
    app.tap(cx, "restore-choose-file");
    app.answer_open_prompt(cx, &folder);
    let state = app.wait_backup(cx, &dialog, &["ready", "refused", "no tool", "file"]);
    assert!(state.starts_with("ready: "), "{state}");
    app.wait_until(cx, "the database's tables", |_, cx| {
        cx.update(|cx| dialog.read(cx).target_loaded())
    });
    // Into a database that holds none of the backup's tables: nothing is
    // replaced.
    assert_eq!(painted(&app, cx, "already holds"), 0);
    assert!(paints(
        &app,
        cx,
        &format!("A restore writes into “{target}” on top of what it holds")
    ));
    app.tap(cx, "simple-parallel");
    let restore = |app: &Fixture, cx: &mut TestAppContext| {
        app.tap(cx, "restore-start");
        app.wait_until(cx, "the confirmation", |app, cx| app.has(cx, "sql-preview"));
    };
    restore(&app, cx);
    app.tap(cx, "sql-confirm");
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    assert!(state.starts_with("done: mysql finished"), "{state}");
    assert_eq!(counts(), "3/2/0");

    // Again, from the same wizard: what it holds now is read again and said,
    // in the wizard and in the confirmation.
    app.tap(cx, "backup-back");
    app.wait_until(cx, "the tables read again", |app, cx| {
        painted(app, cx, &replaced) == 1
    });
    assert!(paints(
        &app,
        cx,
        &format!("A restore drops and replaces the backup's tables that “{target}” holds")
    ));
    restore(&app, cx);
    assert_eq!(painted(&app, cx, &replaced), 2);
    assert!(paints(
        &app,
        cx,
        "dropping and replacing the backup's tables that the database holds"
    ));
    app.tap(cx, "sql-confirm");
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    assert!(state.starts_with("done: mysql finished"), "{state}");
    assert_eq!(counts(), "3/2/0");

    // The child's file fails after its DROP: the failure says it is missing
    // and the parent as it was, as the database holds them.
    let manifest = mysql_folder::read_manifest(&folder).unwrap();
    let child = folder.join(&manifest.tables[0].files[0].file);
    let original = std::fs::read_to_string(&child).unwrap();
    let at = original.find("CREATE TABLE").unwrap();
    std::fs::write(
        &child,
        format!("{}THIS IS NOT SQL;\n{}", &original[..at], &original[at..]),
    )
    .unwrap();
    app.tap(cx, "backup-back");
    app.wait_until(cx, "the tables read again", |_, cx| {
        cx.update(|cx| dialog.read(cx).target_loaded())
    });
    restore(&app, cx);
    app.tap(cx, "sql-confirm");
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    // The child's definition runs first (the manifest's order): the
    // parent's never ran, and it still holds its rows.
    let said = "Loaded: none. Missing now: “child”. Not loaded, as they were before: “parent”. The \
                restore drops and creates each of the backup's tables in “kerno_test_k74_ui_dst” \
                first, then loads their rows: a table it did not finish stays empty or missing.";
    let next = "Run the restore again: it drops each of the backup's tables and loads it from \
                the start.";
    assert!(
        state.starts_with("failed: mysql failed on child: ERROR 1064")
            && state.contains(" of 0001-child.sql: ")
            && state.contains(said)
            && state.ends_with(next),
        "{state}"
    );
    assert!(paints(&app, cx, said));
    assert_eq!(
        on_server(
            engine,
            &format!(
                "SELECT CONCAT((SELECT COUNT(*) FROM {target}.parent), '/', \
                 (SELECT COUNT(*) FROM information_schema.TABLES WHERE TABLE_SCHEMA = '{target}' \
                 AND TABLE_NAME = 'child'))"
            )
        ),
        "3/0"
    );
    // Run again with the file mended: everything is in.
    std::fs::write(&child, original).unwrap();
    app.tap(cx, "backup-back");
    app.wait_until(cx, "the tables read again", |_, cx| {
        cx.update(|cx| dialog.read(cx).target_loaded())
    });
    restore(&app, cx);
    app.tap(cx, "sql-confirm");
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    assert!(state.starts_with("done: mysql finished"), "{state}");
    assert_eq!(counts(), "3/2/0");
}

/// K74: the Speed row's hint names "Block writes" as offered once
/// "Faster" is ticked while that row is hidden, and as below once it shows;
/// on a server before 5.5.3 too.
#[gpui_kit::test]
fn the_speed_hint_points_only_at_a_row_that_shows(cx: &mut TestAppContext) {
    let engine = Engine::MySql;
    if !testing::available(engine) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
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
    for version in [(8, 4, 6), (5, 1, 73)] {
        app.step(cx, |_, cx| {
            dialog.update(cx, |dialog, cx| dialog.set_server_version(version, cx))
        });
        assert!(!app.has(cx, "simple-block-writes"), "{version:?}");
        assert!(
            paints(&app, cx, "“Block writes”, offered once this is ticked")
                && !paints(&app, cx, "“Block writes” below"),
            "{version:?}"
        );
        app.tap(cx, "simple-faster");
        assert!(app.has(cx, "simple-block-writes"), "{version:?}");
        assert!(
            paints(&app, cx, "“Block writes” below")
                && !paints(&app, cx, "offered once this is ticked"),
            "{version:?}"
        );
        app.tap(cx, "simple-faster");
    }
}

/// K78: a folder backup of `kerno_test_k78_ui_src` whose `routines.sql`
/// holds a trigger written for that database (as MariaDB keeps one created
/// with its database's name) and a procedure whose body names it, restored
/// with the real mysql client on 8.4 into `kerno_test_k78_ui_dst`. Said
/// before Start in the simple view, the wizard and the confirmation; by
/// default the trigger is left out (nothing in the source) and named at the
/// end; "Load them as written" creates it in the source, as written; run so
/// again, the pass's failure (ERROR 1359) says what is in place, a rerun's
/// fate and the views already there left as they were, not "Run the restore
/// again". A plain dump: what its first lines name.
#[gpui_kit::test]
fn a_backup_that_names_its_database_is_said_and_left_out_by_default(cx: &mut TestAppContext) {
    let engine = Engine::MySql;
    if !testing::available(engine) {
        return;
    }
    let (source, target) = ("kerno_test_k78_ui_src", "kerno_test_k78_ui_dst");
    let drop = |name: &str| format!("DROP DATABASE IF EXISTS {name}");
    let _scratch = Scratch::new(
        engine,
        &[&drop(source), &drop(target)],
        &[
            &format!("CREATE DATABASE {source}"),
            &format!("CREATE DATABASE {target}"),
            &format!("CREATE TABLE {source}.big (id INT PRIMARY KEY, n INT)"),
        ],
    );
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let folder = names_its_database(&root, source);
    let triggers = |db: &str| {
        on_server(
            engine,
            &format!(
                "SELECT COUNT(*) FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA = '{db}'"
            ),
        )
    };

    let app = connected(cx, engine);
    let dialog = app.simple_dialog(cx, target, true).expect("the restore");
    app.wait_backup(cx, &dialog, &["choose a file"]);
    app.tap(cx, "restore-choose-file");
    app.answer_open_prompt(cx, &folder);
    let state = app.wait_backup(cx, &dialog, &["ready", "refused", "no tool", "file"]);
    if !state.starts_with("ready") {
        eprintln!("SKIPPED: no usable client tool: {state}");
        return;
    }
    app.wait_until(cx, "the database's tables", |_, cx| {
        cx.update(|cx| dialog.read(cx).target_loaded())
    });
    let before = format!(
        "1 trigger of this backup (“big_bi”) is written for the database “{source}”: restored \
         into “{target}” as it is, it would be created in “{source}”, not in “{target}”. Kerno \
         leaves it out unless you tick “Load them as written”; the tables, their rows and \
         everything else are loaded. To have it in “{target}”, edit routines.sql and run it \
         there; or restore into a database named “{source}”. 1 procedure (“count_big”) reads or \
         writes “{source}” by name: restored into “{target}”, it acts on “{source}” each time it \
         runs."
    );
    app.wait_until(cx, "the sentence", |app, cx| paints(app, cx, &before));
    assert!(app.has(cx, "restore-as-written"));
    // The wizard says it too, with the same choice.
    app.tap(cx, "backup-advanced");
    app.to_settings(cx, &dialog);
    assert!(paints(&app, cx, &before) && app.has(cx, "restore-as-written"));
    app.tap(cx, "restore-start");
    app.wait_until(cx, "the confirmation", |app, cx| app.has(cx, "sql-preview"));
    assert_eq!(painted(&app, cx, &before), 2);
    app.tap(cx, "sql-confirm");
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    assert!(
        state.contains(&format!(
            "Left out: 1 trigger (“big_bi”), written for the database “{source}”"
        )),
        "{state}"
    );
    assert_eq!(
        (triggers(source), triggers(target)),
        ("0".into(), "0".into())
    );
    assert_eq!(
        on_server(
            engine,
            &format!(
                "SELECT CONCAT((SELECT COUNT(*) FROM {target}.big), '/', (SELECT COUNT(*) FROM \
                 information_schema.ROUTINES WHERE ROUTINE_SCHEMA = '{target}'), '/', (SELECT \
                 COUNT(*) FROM {target}.big_v))"
            )
        ),
        "3/1/3"
    );

    // Loaded as written: asked again, shown in the confirmation, and the
    // trigger lands where its statement says.
    app.tap(cx, "backup-back");
    app.wait_until(cx, "the tables read again", |_, cx| {
        cx.update(|cx| dialog.read(cx).target_loaded())
    });
    app.tap(cx, "restore-as-written");
    let chosen = "you chose to load it as written, so it is created in “kerno_test_k78_ui_src”";
    assert!(paints(&app, cx, chosen));
    app.tap(cx, "restore-start");
    app.wait_until(cx, "the confirmation", |app, cx| app.has(cx, "sql-preview"));
    assert_eq!(painted(&app, cx, chosen), 2);
    app.tap(cx, "sql-confirm");
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    assert!(
        state.contains(&format!(
            "Loaded as written, into “{source}”: 1 trigger (“big_bi”)."
        )),
        "{state}"
    );
    assert_eq!(
        (triggers(source), triggers(target)),
        ("1".into(), "0".into())
    );

    // Again as written: the pass's failure, said for what it is.
    app.tap(cx, "backup-back");
    app.wait_until(cx, "the tables read again", |_, cx| {
        cx.update(|cx| dialog.read(cx).target_loaded())
    });
    assert!(
        paints(&app, cx, chosen),
        "the choice stays for the same file"
    );
    app.tap(cx, "restore-start");
    app.wait_until(cx, "the confirmation", |app, cx| app.has(cx, "sql-preview"));
    app.tap(cx, "sql-confirm");
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    assert!(
        state.starts_with(
            "failed: mysql failed on routines, triggers and events: ERROR 1359 (HY000) at line 3 \
             of routines.sql: "
        ),
        "{state}"
    );
    let said = format!(
        "Loaded: “big”. Not loaded either: the routines, triggers and events and the views \
         (“{target}” already held 1 of them, “big_v”: left as it was). Every table and its rows \
         are in place. The statement that failed, the trigger “big_bi”, is written for \
         “{source}”, the database the backup was taken from, not “{target}”: as written it acts \
         there. What it creates already exists where it names: running the restore again stops \
         at the same statement."
    );
    assert!(
        state.contains(&said) && !state.contains("Run the restore again"),
        "{state}"
    );
    assert!(paints(&app, cx, &said));
    app.tap(cx, "backup-close");

    // A plain dump: its header names its database; one that selects a
    // database itself says so.
    for (name, head, words) in [
        (
            "plain.sql",
            format!("-- MySQL dump 10.13\n--\n-- Host: 127.0.0.1    Database: {source}\n"),
            format!(
                "This dump was taken from the database “{source}”: its triggers, routines, \
                 events or views that name “{source}” would act on “{source}”, not on “{target}”."
            ),
        ),
        (
            "uses.sql",
            format!(
                "-- MySQL dump 10.13\n--\n-- Host: 127.0.0.1    Database: {source}\n\
                 CREATE DATABASE /*!32312 IF NOT EXISTS*/ `{source}`;\n\nUSE `{source}`;\n"
            ),
            format!(
                "This dump selects the database “{source}” itself (USE `{source}`): restored, \
                 what it holds goes into “{source}”, not into “{target}”."
            ),
        ),
    ] {
        let file = root.join(name);
        std::fs::write(&file, format!("{head}SELECT 1;\n")).unwrap();
        let dialog = app.simple_dialog(cx, target, true).expect("the restore");
        app.wait_backup(cx, &dialog, &["choose a file"]);
        app.tap(cx, "restore-choose-file");
        app.answer_open_prompt(cx, &file);
        app.wait_backup(cx, &dialog, &["ready", "refused", "no tool", "file"]);
        assert!(paints(&app, cx, &words), "{name}");
        assert!(!app.has(cx, "restore-as-written"), "{name}");
        app.tap(cx, "backup-close");
    }
}

/// K78: a folder backup of `source`, one table `big`, whose `routines.sql`
/// holds a trigger written for `source` and a procedure whose body names it,
/// and a view.
pub(super) fn names_its_database(root: &std::path::Path, source: &str) -> std::path::PathBuf {
    let folder = folder_of(root, "k78", &["big"], BackupContent::All, true);
    std::fs::write(
        folder.join("0001-big.sql"),
        "/*!40014 SET FOREIGN_KEY_CHECKS=0 */;\nDROP TABLE IF EXISTS `big`;\n\
         /*!40101 SET @saved_cs_client     = @@character_set_client */;\n\
         CREATE TABLE `big` (\n  `id` int NOT NULL,\n  `n` int DEFAULT NULL,\n  PRIMARY KEY \
         (`id`)\n) ENGINE=InnoDB;\n/*!40101 SET character_set_client = @saved_cs_client */;\n\
         INSERT INTO `big` VALUES (1,2),(2,4),(3,6);\n",
    )
    .unwrap();
    std::fs::write(
        folder.join("routines.sql"),
        format!(
            "/*!40101 SET @OLD_CHARACTER_SET_CLIENT=@@CHARACTER_SET_CLIENT */;\nDELIMITER ;;\n\
             /*!50003 CREATE*/ /*!50003 trigger {source}.big_bi before insert on {source}.big \
             for each row set new.n = new.id * 2 */;;\nDELIMITER ;\n\
             /*!50003 DROP PROCEDURE IF EXISTS `count_big` */;\nDELIMITER ;;\n\
             CREATE PROCEDURE `count_big`(OUT c INT)\n    READS SQL DATA\n\
             SELECT COUNT(*) INTO c FROM {source}.big ;;\nDELIMITER ;\n"
        ),
    )
    .unwrap();
    std::fs::write(
        folder.join("views.sql"),
        "/*!50001 DROP VIEW IF EXISTS `big_v`*/;\n\
         /*!50001 CREATE VIEW `big_v` AS SELECT `id` FROM `big` */;\n",
    )
    .unwrap();
    let mut manifest = mysql_folder::read_manifest(&folder).unwrap();
    manifest.database = source.into();
    manifest.after = vec![
        mysql_folder::AfterFile {
            file: "routines.sql".into(),
            kind: mysql_folder::AfterKind::Routines,
            objects: Vec::new(),
        },
        mysql_folder::AfterFile {
            file: "views.sql".into(),
            kind: mysql_folder::AfterKind::Views,
            objects: vec!["big_v".into()],
        },
    ];
    std::fs::write(
        folder.join(mysql_folder::MANIFEST),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    folder
}

/// How many painted texts hold `part`.
fn painted(app: &Fixture, cx: &mut TestAppContext, part: &str) -> usize {
    app.step(cx, |window, _| {
        gpui_kit::base::test_support::texts(window)
            .into_iter()
            .filter(|text| text.text().contains(part))
            .count()
    })
}

/// A hand-made folder backup of `tables` with `content` and DROP TABLE or
/// not; its files only say `SELECT 1;`.
fn folder_of(
    root: &std::path::Path,
    name: &str,
    tables: &[&str],
    content: BackupContent,
    add_drop_table: bool,
) -> std::path::PathBuf {
    let folder = root.join(name);
    std::fs::create_dir(&folder).unwrap();
    let tables: Vec<mysql_folder::TableFiles> = tables
        .iter()
        .enumerate()
        .map(|(at, table)| mysql_folder::TableFiles {
            table: table.to_string(),
            engine: Some("InnoDB".into()),
            files: vec![mysql_folder::Piece {
                file: format!("{:04}-{table}.sql", at + 1),
                condition: None,
            }],
        })
        .collect();
    let manifest = mysql_folder::Manifest {
        kerno_backup: 1,
        engine: Engine::MySql,
        database: "kerno_test_k63_ui_made".into(),
        server_version: "8.4.6".into(),
        client: "mysqldump  Ver 8.4.6 for macos15 on arm64".into(),
        content,
        options: MySqlBackup {
            add_drop_table,
            jobs: Some(2),
            ..MySqlBackup::default()
        },
        tables,
        after: Vec::new(),
    };
    for file in manifest.files() {
        std::fs::write(folder.join(file), "SELECT 1;\n").unwrap();
    }
    std::fs::write(
        folder.join(mysql_folder::MANIFEST),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    folder
}

/// The restore of a folder backup into a database that already holds some
/// of its tables says what happens to them, as the backup was made: rows
/// added (data only), dropped and created again (with or without their
/// rows), or a stop at the first one (no DROP TABLE); nothing when none is
/// there. The confirmation says it too (G3).
#[gpui_kit::test]
fn the_restore_says_what_happens_to_the_tables_already_there(cx: &mut TestAppContext) {
    let engine = Engine::MySql;
    if !testing::available(engine) {
        return;
    }
    let target = "kerno_test_k63_ui_there";
    let _scratch = Scratch::new(
        engine,
        &[&format!("DROP DATABASE IF EXISTS {target}")],
        &[
            &format!("CREATE DATABASE {target}"),
            &format!("CREATE TABLE {target}.one (id INT PRIMARY KEY)"),
        ],
    );
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    // A stand-in that never runs here: each restore stops at the
    // confirmation.
    let client = fake_client(
        &root,
        "mysql",
        ClientFamily::MySql,
        "mysqldump  Ver 8.4.6 for macos15 on arm64",
        "exit 0",
    );
    let app = connected(cx, engine);
    app.use_client(cx, ClientFamily::MySql, &client);
    let held = format!("“{target}” already holds");
    let cases = [
        (
            "data",
            &["one", "two"][..],
            BackupContent::DataOnly,
            true,
            Some(
                " “one”, which the backup has rows for: the backup holds rows only, so they are \
                 added to the tables already there; a table that already holds some of them stops \
                 the restore on a duplicate key, or ends with rows twice when it has no key."
                    .to_string(),
            ),
        ),
        (
            "all-drop",
            &["one", "two"][..],
            BackupContent::All,
            true,
            Some(format!(
                " 1 of the backup's 2 tables (“one”): each is dropped and replaced with the \
                 backup's table and rows; the tables only in “{target}” stay. Kerno creates the \
                 backup's tables first, one after the other, then loads their rows: if the \
                 restore stops, a table it has not finished is left empty or missing until you \
                 run it again."
            )),
        ),
        (
            "all-no-drop",
            &["one", "two"][..],
            BackupContent::All,
            false,
            Some(
                " “one”, which the backup has too, and the backup was made without DROP TABLE \
                 statements: the restore stops at the first of them."
                    .to_string(),
            ),
        ),
        (
            "schema-drop",
            &["one", "two"][..],
            BackupContent::SchemaOnly,
            true,
            Some(format!(
                " 1 of the backup's 2 tables (“one”): the backup holds definitions only, so each \
                 is dropped and created again empty, and the rows it holds now are gone; the \
                 tables only in “{target}” stay."
            )),
        ),
        ("none", &["two"][..], BackupContent::All, true, None),
    ];
    for (name, tables, content, drop, words) in cases {
        let folder = folder_of(&root, name, tables, content, drop);
        let dialog = app.simple_dialog(cx, target, true).expect("the restore");
        app.wait_backup(cx, &dialog, &["choose a file"]);
        app.tap(cx, "restore-choose-file");
        app.answer_open_prompt(cx, &folder);
        let state = app.wait_backup(cx, &dialog, &["ready", "refused", "no tool", "file"]);
        assert!(state.starts_with("ready: "), "{name}: {state}");
        app.wait_until(cx, "the database's tables", |_, cx| {
            cx.update(|cx| dialog.read(cx).target_loaded())
        });
        let said = words.map(|words| format!("{held}{words}"));
        match &said {
            Some(said) => assert_eq!(painted(&app, cx, said), 1, "{name}"),
            None => assert_eq!(painted(&app, cx, &held), 0, "{name}"),
        }
        app.tap(cx, "restore-start");
        app.wait_until(cx, "the confirmation", |app, cx| app.has(cx, "sql-preview"));
        // The confirmation says it too: the words twice now.
        match &said {
            Some(said) => assert_eq!(painted(&app, cx, said), 2, "{name}"),
            None => assert_eq!(painted(&app, cx, &held), 0, "{name}"),
        }
        app.tap(cx, "sql-cancel");
        app.tap(cx, "backup-close");
    }
    // A plain dump into that database: its DROP TABLE statements are said,
    // the tables unnamed (K74).
    let plain = root.join("plain.sql");
    std::fs::write(&plain, "SELECT 1;\n").unwrap();
    let dialog = app.simple_dialog(cx, target, true).expect("the restore");
    app.wait_backup(cx, &dialog, &["choose a file"]);
    app.tap(cx, "restore-choose-file");
    app.answer_open_prompt(cx, &plain);
    let state = app.wait_backup(cx, &dialog, &["ready", "refused", "no tool", "file"]);
    assert!(state.starts_with("ready: "), "{state}");
    app.wait_until(cx, "the database's tables", |_, cx| {
        cx.update(|cx| dialog.read(cx).target_loaded())
    });
    let said = format!(
        "“{target}” is not empty: a dump made with DROP TABLE statements, as mysqldump writes by \
         default, drops each of its tables that “{target}” holds and creates it again from the \
         dump; the other tables stay. Kerno does not read the whole file first to name them."
    );
    assert_eq!(painted(&app, cx, &said), 1);
    app.tap(cx, "restore-start");
    app.wait_until(cx, "the confirmation", |app, cx| app.has(cx, "sql-preview"));
    assert_eq!(painted(&app, cx, &said), 2);
    // One true sentence: not "on top of" beside "replaces" (L4).
    assert!(paints(
        &app,
        cx,
        "where the dump's DROP TABLE statements, if it has any, replace the tables they name"
    ));
    assert_eq!(painted(&app, cx, "on top of what"), 0);
    app.tap(cx, "sql-cancel");
    app.tap(cx, "backup-close");

    // A database whose tables cannot be read (it does not exist): every
    // table of the backup there is replaced, said once, in the wizard and
    // the confirmation (L4).
    let missing = "kerno_test_k74_ui_none";
    on_server(engine, &format!("DROP DATABASE IF EXISTS {missing}"));
    let folder = folder_of(&root, "unread", &["one"], BackupContent::All, true);
    let dialog = app.simple_dialog(cx, missing, true).expect("the restore");
    app.wait_backup(cx, &dialog, &["choose a file"]);
    app.tap(cx, "restore-choose-file");
    app.answer_open_prompt(cx, &folder);
    app.wait_backup(cx, &dialog, &["ready", "refused", "no tool", "file"]);
    app.wait_until(cx, "the read that fails", |_, cx| {
        cx.update(|cx| dialog.read(cx).target_read_ended())
    });
    let unread = format!(
        "Kerno could not read which tables “{missing}” holds: every table of the backup that \
         exists there is dropped and replaced."
    );
    assert_eq!(painted(&app, cx, &unread), 1);
    assert!(paints(
        &app,
        cx,
        &format!("A restore drops and replaces the backup's tables that “{missing}” holds")
    ));
    app.tap(cx, "restore-start");
    app.wait_until(cx, "the confirmation", |app, cx| app.has(cx, "sql-preview"));
    assert_eq!(painted(&app, cx, &unread), 2);
    assert!(paints(
        &app,
        cx,
        "dropping and replacing the backup's tables that the database holds"
    ));
    assert_eq!(painted(&app, cx, "on top of what"), 0);
    app.tap(cx, "sql-cancel");
    app.tap(cx, "backup-close");

    // A name that differs only by case is another table where the server
    // tells case apart (`lower_case_table_names` = 0) (L4).
    let folds = on_server(engine, "SELECT @@lower_case_table_names") != "0";
    on_server(engine, &format!("CREATE TABLE {target}.Two (id INT)"));
    let folder = folder_of(&root, "case", &["two"], BackupContent::All, true);
    let dialog = app.simple_dialog(cx, target, true).expect("the restore");
    app.wait_backup(cx, &dialog, &["choose a file"]);
    app.tap(cx, "restore-choose-file");
    app.answer_open_prompt(cx, &folder);
    app.wait_backup(cx, &dialog, &["ready", "refused", "no tool", "file"]);
    app.wait_until(cx, "the database's tables", |_, cx| {
        cx.update(|cx| dialog.read(cx).target_loaded())
    });
    assert_eq!(
        painted(&app, cx, "already holds the backup's table (“two”)"),
        usize::from(folds),
        "lower_case_table_names folds: {folds}"
    );
    app.tap(cx, "backup-close");
}

/// K74, F3 and F4: a folder restore cancelled once the definitions ran
/// says, in the dialog, which tables are now empty, as the database holds
/// them (not "what ran before it stopped is kept"); an extra argument that
/// changes what the mysql client prints is refused before Start. The
/// client is the real mysql, except that a file's rows wait 30 s (a
/// stand-in script in front of it; Unix only).
#[gpui_kit::test]
fn a_cancelled_restore_says_what_it_left_and_output_options_are_refused(cx: &mut TestAppContext) {
    let engine = Engine::MySql;
    if !testing::available(engine) || !fake_tools_run() {
        return;
    }
    let Some(real) = kerno_core::tools::find(kerno_core::tools::Tool::Mysql) else {
        eprintln!("SKIPPED: no mysql client");
        return;
    };
    let target = "kerno_test_k74_ui_cancel";
    let _scratch = Scratch::new(
        engine,
        &[&format!("DROP DATABASE IF EXISTS {target}")],
        &[
            &format!("CREATE DATABASE {target}"),
            &format!("CREATE TABLE {target}.one (id INT PRIMARY KEY)"),
            &format!("INSERT INTO {target}.one VALUES (1), (2)"),
            // K78: a view of an earlier restore.
            &format!("CREATE VIEW {target}.one_v AS SELECT 1 AS x"),
        ],
    );
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let real = real.display().to_string();
    let client = fake_client(
        &root,
        "slow-rows",
        ClientFamily::MySql,
        "mysql  Ver 8.4.6 for macos15 on arm64",
        &format!(
            "case \"$*\" in *--unbuffered*|*--execute=*) exec '{real}' \"$@\";; esac\nexec sleep 30"
        ),
    );
    // Real files: a table's definition, then its rows.
    let folder = folder_of(&root, "real", &["one", "two"], BackupContent::All, true);
    for (at, table) in ["one", "two"].iter().enumerate() {
        std::fs::write(
            folder.join(format!("{:04}-{table}.sql", at + 1)),
            format!(
                "/*!40014 SET FOREIGN_KEY_CHECKS=0 */;\nDROP TABLE IF EXISTS `{table}`;\n\
                 /*!40101 SET @saved_cs_client     = @@character_set_client */;\n\
                 CREATE TABLE `{table}` (\n  `id` int NOT NULL,\n  PRIMARY KEY (`id`)\n) \
                 ENGINE=InnoDB;\n/*!40101 SET character_set_client = @saved_cs_client */;\n\
                 INSERT INTO `{table}` VALUES (1),(2),(3);\n"
            ),
        )
        .unwrap();
    }
    // The backup's views, after the tables (K78).
    std::fs::write(
        folder.join("views.sql"),
        "/*!50001 CREATE OR REPLACE VIEW `one_v` AS SELECT 2 AS x */;\n",
    )
    .unwrap();
    let mut manifest = mysql_folder::read_manifest(&folder).unwrap();
    manifest.after = vec![mysql_folder::AfterFile {
        file: "views.sql".into(),
        kind: mysql_folder::AfterKind::Views,
        objects: vec!["one_v".into()],
    }];
    std::fs::write(
        folder.join(mysql_folder::MANIFEST),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let app = connected(cx, engine);
    app.use_client(cx, ClientFamily::MySql, &client);
    let dialog = app.simple_dialog(cx, target, true).expect("the restore");
    app.wait_backup(cx, &dialog, &["choose a file"]);
    app.tap(cx, "restore-choose-file");
    app.answer_open_prompt(cx, &folder);
    let state = app.wait_backup(cx, &dialog, &["ready", "refused", "no tool", "file"]);
    assert!(state.starts_with("ready: "), "{state}");
    app.wait_until(cx, "the database's tables", |_, cx| {
        cx.update(|cx| dialog.read(cx).target_loaded())
    });
    // F4: an option that changes the client's output stops Start.
    app.tap(cx, "backup-advanced");
    app.to_settings(cx, &dialog);
    app.replace_text(cx, "backup-extra-args", "--force --table");
    assert!(!cx.update(|cx| dialog.read(cx).ready_to_start(cx)));
    assert!(paints(
        &app,
        cx,
        "“--table” is not allowed for a folder restore"
    ));
    // K78: a task that could never start is not saved either.
    let saves = |app: &Fixture, cx: &mut TestAppContext| {
        app.step(cx, |window, _| {
            window.find("wizard-save-as-task").disabled() != Some(true)
        })
    };
    assert!(!saves(&app, cx));
    let refused = cx
        .update(|cx| dialog.read(cx).task_settings(cx))
        .unwrap_err();
    assert!(
        refused.contains("“--table” is not allowed for a folder restore"),
        "{refused}"
    );
    app.replace_text(cx, "backup-extra-args", "--force");
    assert!(cx.update(|cx| dialog.read(cx).ready_to_start(cx)));
    assert!(saves(&app, cx));

    // F3: cancelled while the rows wait, every definition run.
    app.tap(cx, "restore-start");
    app.wait_until(cx, "the confirmation", |app, cx| app.has(cx, "sql-preview"));
    app.tap(cx, "sql-confirm");
    app.wait_until(cx, "the rows' clients", |_, cx| {
        cx.update(|cx| dialog.read(cx).log())
            .iter()
            .any(|line| line.contains("mysql clients at once"))
    });
    app.tap(cx, "backup-cancel");
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    assert!(
        state.starts_with(
            "failed: Cancelled: mysql was stopped. Loaded: none. Empty now: “one”, \
                           “two”."
        ) && !state.contains("what ran before it stopped is kept")
            && state.contains("Run the restore again"),
        "{state}"
    );
    // K78: the view already there is said to be as it was, and it is.
    assert!(
        state.contains(
            "Not loaded either: the views (“kerno_test_k74_ui_cancel” already held 1 of them, \
             “one_v”: left as it was)."
        ),
        "{state}"
    );
    assert_eq!(
        on_server(
            engine,
            &format!(
                "SELECT CONCAT((SELECT COUNT(*) FROM {target}.one), '/', (SELECT x FROM {target}.one_v))"
            )
        ),
        "0/1"
    );
    // K78: the log's long line wraps inside its box (it ran past the right
    // edge), taller than a line of its own.
    let (log, lines) = app.step(cx, |window, _| {
        let log = window.find("backup-log").bounds();
        let lines: Vec<(String, gpui_kit::Bounds<gpui_kit::Pixels>)> =
            gpui_kit::base::test_support::texts(window)
                .into_iter()
                .filter(|text| text.text().starts_with("Kerno: "))
                .map(|text| (text.text().to_string(), text.bounds()))
                .collect();
        (log, lines)
    });
    let short = lines
        .iter()
        .find(|(text, _)| text.contains("definitions first"))
        .map(|(_, bounds)| bounds.size.height)
        .expect("a short line");
    let long = lines
        .iter()
        .find(|(text, _)| text.contains("Empty now"))
        .expect("the long line");
    assert!(
        long.1.size.height >= short * 2.,
        "{long:?} against {short:?}"
    );
    for (text, bounds) in &lines {
        assert!(
            bounds.right() <= log.right(),
            "{text}: {bounds:?} in {log:?}"
        );
    }
}

/// K74, N4: a saved folder-restore task whose extra arguments are refused
/// now (`--table`) opens from the Tasks view with Start disabled, and says
/// why where the user starts, naming the argument; it never runs.
#[gpui_kit::test]
fn a_task_with_refused_arguments_says_why_where_it_starts(cx: &mut TestAppContext) {
    let engine = Engine::MySql;
    if !testing::available(engine) || !fake_tools_run() {
        return;
    }
    let target = "kerno_test_k74_ui_task";
    let _scratch = Scratch::new(
        engine,
        &[&format!("DROP DATABASE IF EXISTS {target}")],
        &[&format!("CREATE DATABASE {target}")],
    );
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let ran = root.join("ran");
    let client = fake_client(
        &root,
        "mysql",
        ClientFamily::MySql,
        "mysqldump  Ver 8.4.6 for macos15 on arm64",
        &format!("touch '{}'", ran.display()),
    );
    let app = connected(cx, engine);
    app.use_client(cx, ClientFamily::MySql, &client);
    let folder = folder_of(&root, "task", &["one"], BackupContent::All, true);
    let connection = cx.update(|cx| app.connections.read(cx).saved()[0].id.clone());
    let options = kerno_core::tools::RestoreOptions {
        jobs: Some(2),
        extra_args: "--force --table".into(),
        ..kerno_core::tools::RestoreOptions::new(
            Engine::MySql,
            target,
            &folder,
            BackupFormat::Directory,
        )
    };
    let task = kerno_core::storage::SavedTask::new(
        "kerno_test_k74 restore",
        connection,
        kerno_core::tools::TaskSettings::Restore(options),
    );
    let id = app.storage.save_task(&task).unwrap();
    let view = app.tasks_view(cx);
    app.task_does(cx, &view, crate::tasks_view::TaskAction::Run, id);
    let dialog = app.task_wizard(cx, &view);
    app.wait_backup(cx, &dialog, &["ready", "refused", "no tool", "file"]);
    app.wait_until(cx, "the database's tables", |_, cx| {
        cx.update(|cx| dialog.read(cx).target_loaded())
    });
    assert!(!cx.update(|cx| dialog.read(cx).ready_to_start(cx)));
    assert!(paints(
        &app,
        cx,
        "Cannot start: “--table” is not allowed for a folder restore"
    ));
    assert!(!ran.exists(), "a client ran");
    assert!(
        cx.update(|cx| dialog.read(cx).state(cx))
            .starts_with("ready")
    );
    app.tap(cx, "backup-close");
}

/// A backup made in utf8mb4 (its first lines set it), restored into a
/// server older than 5.5.3: said before anything runs (G10).
#[gpui_kit::test]
fn a_utf8mb4_backup_for_an_old_server_is_said(cx: &mut TestAppContext) {
    let engine = Engine::MySql;
    if !testing::available(engine) {
        return;
    }
    let target = "kerno_test_k63_ui_old";
    let _scratch = Scratch::new(
        engine,
        &[&format!("DROP DATABASE IF EXISTS {target}")],
        &[&format!("CREATE DATABASE {target}")],
    );
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let client = fake_client(
        &root,
        "mysql",
        ClientFamily::MySql,
        "mysqldump  Ver 10.13 Distrib 5.7.99, for osx10.19 (x86_64)",
        "exit 0",
    );
    let app = connected(cx, engine);
    app.use_client(cx, ClientFamily::MySql, &client);
    let said = "This backup was made in utf8mb4, which MySQL 5.1.73 does not have: the restore \
                stops at its first statement (ERROR 1115).";
    // A folder backup: its first file is read.
    let folder = folder_of(&root, "folder", &["one"], BackupContent::All, true);
    std::fs::write(
        folder.join("0001-one.sql"),
        "-- MySQL dump 10.13\n/*!40101 SET NAMES utf8mb4 */;\nSELECT 1;\n",
    )
    .unwrap();
    for (name, head, warned) in [
        (
            "new.sql",
            "-- MySQL dump 10.13\n/*!40101 SET NAMES utf8mb4 */;\nSELECT 1;\n",
            true,
        ),
        (
            "old.sql",
            "-- MySQL dump 10.13\n/*!40101 SET NAMES utf8 */;\nSELECT 1;\n",
            false,
        ),
        ("folder", "", true),
    ] {
        let file = root.join(name);
        if !head.is_empty() {
            std::fs::write(&file, head).unwrap();
        }
        let dialog = app.simple_dialog(cx, target, true).expect("the restore");
        // As a 5.1 server would be read.
        dialog.update(cx, |dialog, cx| dialog.set_server_version((5, 1, 73), cx));
        app.wait_backup(cx, &dialog, &["choose a file"]);
        app.tap(cx, "restore-choose-file");
        app.answer_open_prompt(cx, &file);
        let state = app.wait_backup(cx, &dialog, &["ready", "refused", "no tool", "file"]);
        assert!(state.starts_with("ready: "), "{name}: {state}");
        assert_eq!(paints(&app, cx, said), warned, "{name}");
        app.tap(cx, "backup-close");
    }
}

/// mysqldump's "it has no fields" fails a MySQL backup only: a restore
/// whose client writes the same line ends well (G6).
#[gpui_kit::test]
fn only_a_mysql_backup_fails_on_no_fields(cx: &mut TestAppContext) {
    let engine = Engine::MySql;
    if !testing::available(engine) {
        return;
    }
    let target = "kerno_test_k63_ui_fields";
    let _scratch = Scratch::new(
        engine,
        &[&format!("DROP DATABASE IF EXISTS {target}")],
        &[&format!("CREATE DATABASE {target}")],
    );
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let client = fake_client(
        &root,
        "fields",
        ClientFamily::MySql,
        "mysqldump  Ver 8.4.6 for macos15 on arm64",
        r#"for arg in "$@"; do case $arg in --result-file=*) echo '-- MySQL dump' > "${arg#--result-file=}";; esac; done
echo "-- Skipping dump data for table 't', it has no fields" >&2
echo "Note: the label says it has no fields" >&2
exit 0"#,
    );
    let app = connected(cx, engine);
    app.use_client(cx, ClientFamily::MySql, &client);
    // The restore: the same client prints the same lines.
    let file = root.join("in.sql");
    std::fs::write(&file, "SELECT 1;\n").unwrap();
    let dialog = app.simple_dialog(cx, target, true).expect("the restore");
    app.wait_backup(cx, &dialog, &["choose a file"]);
    app.tap(cx, "restore-choose-file");
    app.answer_open_prompt(cx, &file);
    let state = app.wait_backup(cx, &dialog, &["ready", "refused", "no tool", "file"]);
    assert!(state.starts_with("ready: "), "{state}");
    app.tap(cx, "restore-start");
    app.wait_until(cx, "the confirmation", |app, cx| app.has(cx, "sql-preview"));
    app.tap(cx, "sql-confirm");
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    assert!(state.starts_with("done: "), "{state}");
    app.tap(cx, "backup-close");
}
