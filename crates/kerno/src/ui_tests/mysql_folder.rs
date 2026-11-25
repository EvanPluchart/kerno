//! K63: the faster backup of MySQL and MariaDB, several mysqldump at once
//! into a folder that the restore loads on several mysql clients; the
//! consistency choice, with and without the RELOAD privilege; an old server
//! that needs an old client (docs/plans/M12.md, D-135).

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
            "“kerno_test_k63_ui_dst” already holds “one”, which the backup has too: each is \
             dropped and created again from the backup; the database's other tables stay.",
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
    let held = format!("“{target}” already holds “one”");
    let cases = [
        (
            "data",
            &["one", "two"][..],
            BackupContent::DataOnly,
            true,
            Some(
                "which the backup has rows for: the backup holds rows only, so they are added to \
                 the tables already there; a table that already holds some of them stops the \
                 restore on a duplicate key, or ends with rows twice when it has no key.",
            ),
        ),
        (
            "all-drop",
            &["one", "two"][..],
            BackupContent::All,
            true,
            Some(
                "which the backup has too: each is dropped and created again from the backup; the \
                 database's other tables stay.",
            ),
        ),
        (
            "all-no-drop",
            &["one", "two"][..],
            BackupContent::All,
            false,
            Some(
                "which the backup has too, and the backup was made without DROP TABLE \
                 statements: the restore stops at the first of them.",
            ),
        ),
        (
            "schema-drop",
            &["one", "two"][..],
            BackupContent::SchemaOnly,
            true,
            Some(
                "which the backup has too: the backup holds definitions only, so each is dropped \
                 and created again empty: the rows it holds now are gone; the database's other \
                 tables stay.",
            ),
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
        let said = words.map(|words| format!("{held}, {words}"));
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
