//! K39: a restore whose archive needs an extension the server does not
//! have says so before it runs and leaves it out through pg_restore's
//! `--use-list`; a failed `CREATE EXTENSION` is said after the run, with a
//! retry that leaves it out (docs/plans/M12.md, D-112).
//!
//! The test server offers `pg_trgm` and not `pgaudit`, and has no role
//! `app_owner`: the stand-in pg_restore's lists name them, and the real
//! server answers which are missing.

use std::path::{Path, PathBuf};

use kerno_core::tools::ClientFamily;

use super::tool_wizards::{fake_client, paints};
use super::*;
use crate::backup_dialog::BackupDialog;

/// An archive's list as `pg_restore --list` prints it: an extension the
/// server offers, one it does not (with its comment), a table owned by a
/// role the server lacks.
const LIST: &str = concat!(
    ";\n;     dbname: prod\n;\n; Selected TOC Entries:\n;\n",
    "2; 3079 16385 EXTENSION - pg_trgm \n",
    "3; 3079 16390 EXTENSION - pgaudit \n",
    "3485; 0 0 COMMENT - EXTENSION pgaudit \n",
    "219; 1259 16400 TABLE public t app_owner\n",
    "220; 1259 16401 TABLE public u kerno\n",
);

/// What a backup of this server lists: nothing it lacks.
const HOME: &str = concat!(
    ";\n;     dbname: here\n;\n",
    "2; 3079 16385 EXTENSION - pg_trgm \n",
    "220; 1259 16401 TABLE public u kerno\n",
);

/// The report of 2026-11-23, its failing part verbatim (pg_restore 18 in
/// French), then K35's refused owner.
const FAILED: &str = "\
pg_restore: création de EXTENSION « pgaudit »
pg_restore: pendant le traitement de la TOC (« PROCESSING TOC ») :
pg_restore: de l'entrée TOC 2 ; 3079 16390 EXTENSION pgaudit (no owner)
pg_restore: erreur : could not execute query: ERROR:  extension \"pgaudit\" is not available
ASTUCE : The extension must first be installed on the system where PostgreSQL is running.
La commande était : CREATE EXTENSION IF NOT EXISTS pgaudit WITH SCHEMA public;
pg_restore: de l'entrée TOC 219 ; 1259 16400 TABLE t app_owner
pg_restore: erreur : could not execute query: ERROR:  role \"app_owner\" does not exist
La commande était : ALTER TABLE public.t OWNER TO app_owner;";

fn args(cx: &mut TestAppContext, dialog: &Entity<BackupDialog>) -> Vec<String> {
    cx.update(|cx| dialog.read(cx).pending_command(cx))
        .expect("a command")
        .args
}

fn use_list(args: &[String]) -> Option<PathBuf> {
    args.iter()
        .find_map(|arg| arg.strip_prefix("--use-list="))
        .map(PathBuf::from)
}

fn open_file(app: &Fixture, cx: &mut TestAppContext, dialog: &Entity<BackupDialog>, file: &Path) {
    app.tap(cx, "restore-choose-file");
    app.answer_open_prompt(cx, file);
    let state = app.wait_backup(cx, dialog, &["ready", "file", "no tool"]);
    assert!(state.starts_with("ready: "), "{state}");
}

fn restore(app: &Fixture, cx: &mut TestAppContext, dialog: &Entity<BackupDialog>) -> String {
    app.tap(cx, "restore-start");
    app.wait_until(cx, "the confirmation", |app, cx| app.has(cx, "sql-preview"));
    app.tap(cx, "sql-confirm");
    app.wait_backup(cx, dialog, &["done", "failed"])
}

/// A stand-in client whose pg_restore lists `prod_dump` as [`LIST`] and
/// any other archive as [`HOME`]; a restore copies the list it is given
/// to `ran.list` and its arguments to `ran`, and fails with [`FAILED`]
/// unless it leaves out both the owners and the extension.
fn client(root: &Path) -> PathBuf {
    std::fs::write(root.join("prod.list"), LIST).unwrap();
    std::fs::write(root.join("home.list"), HOME).unwrap();
    std::fs::write(root.join("failed.log"), FAILED).unwrap();
    let r = root.display();
    fake_client(
        root,
        "client",
        ClientFamily::Postgres,
        "pg_restore (PostgreSQL) 18.7",
        &format!(
            "case \" $* \" in *\" --list \"*prod_dump*) cat {r}/prod.list; exit 0;; \
             *\" --list \"*) cat {r}/home.list; exit 0;; esac\n\
             echo \"$*\" > {r}/ran\n\
             for a; do case $a in --use-list=*) cp \"${{a#--use-list=}}\" {r}/ran.list;; esac; done\n\
             case \" $* \" in *\" --use-list=\"*\" --no-owner \"*) echo restored; exit 0;; esac\n\
             cat {r}/failed.log >&2; exit 1"
        ),
    )
}

/// Before the run: the extension and the owner the server lacks are said,
/// the extension left out (`--use-list` with its entries commented out,
/// said in words in the command), the owners left out; unticked, the
/// command is as before. A backup that needs nothing missing says
/// nothing. The list file is removed after the run.
#[gpui_kit::test]
fn what_the_server_lacks_is_said_and_left_out_before_the_run(cx: &mut TestAppContext) {
    if !super::tool_wizards::fake_tools_run() {
        return;
    }
    if !testing::available(Engine::Postgres) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let client = client(&root);
    let home = root.join("home_dump");
    let prod = root.join("prod_dump");
    for file in [&home, &prod] {
        std::fs::write(file, b"PGDMP\x01\x0e\x00 an archive").unwrap();
    }
    let app = connected(cx, Engine::Postgres);
    app.use_client(cx, ClientFamily::Postgres, &client);
    let dialog = app
        .simple_dialog(cx, "kerno_ui_restore_never", true)
        .expect("the restore");
    app.wait_backup(cx, &dialog, &["choose a file"]);

    // Everything this backup names is there: nothing asked, while the
    // server is asked and answers.
    open_file(&app, cx, &dialog, &home);
    for _ in 0..40 {
        cx.run_until_parked();
        assert!(!app.has(cx, "restore-preflight"));
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let plain = args(cx, &dialog);
    assert!(
        use_list(&plain).is_none() && !plain.contains(&"--no-owner".into()),
        "{plain:?}"
    );

    open_file(&app, cx, &dialog, &prod);
    app.wait_until(cx, "the preflight", |app, cx| {
        app.has(cx, "restore-preflight")
    });
    assert!(paints(
        &app,
        cx,
        "This backup needs the extension “pgaudit”, which this server does not have."
    ));
    assert!(!paints(&app, cx, "“pg_trgm”"));
    assert!(paints(
        &app,
        cx,
        "This backup gives its objects to the role “app_owner”, which this server does not have: \
         the backup's owners and privileges are left out, and what it restores belongs to \
         “kerno”."
    ));
    let ticked = args(cx, &dialog);
    let list = use_list(&ticked).expect("--use-list");
    assert!(
        ticked.contains(&"--no-owner".into()) && ticked.contains(&"--no-privileges".into()),
        "{ticked:?}"
    );
    let state = app.wizard_state(cx, &dialog);
    assert!(
        state.contains(
            "# leaves out of the archive the extension “pgaudit” (the entries that create it and \
             comment on it)"
        ),
        "{state}"
    );
    // The wizard says the same.
    app.tap(cx, "backup-advanced");
    assert!(app.has(cx, "restore-preflight"));
    app.tap(cx, "wizard-simple");

    // Unticked, the extension is restored as the backup has it.
    app.tap(cx, "restore-leave-out");
    assert!(use_list(&args(cx, &dialog)).is_none());
    app.tap(cx, "restore-leave-out");

    // A backup of this server chosen next: what the preflight ticked for
    // the other file is unticked.
    open_file(&app, cx, &dialog, &home);
    let next = args(cx, &dialog);
    assert!(
        use_list(&next).is_none() && !next.contains(&"--no-owner".into()),
        "{next:?}"
    );
    open_file(&app, cx, &dialog, &prod);
    app.wait_until(cx, "the preflight", |app, cx| {
        app.has(cx, "restore-preflight")
    });

    let state = restore(&app, cx, &dialog);
    assert!(state.starts_with("done: pg_restore finished"), "{state}");
    let given = std::fs::read_to_string(root.join("ran.list")).unwrap();
    assert_eq!(
        given
            .lines()
            .filter(|line| line.starts_with(";3"))
            .collect::<Vec<_>>(),
        [
            ";3; 3079 16390 EXTENSION - pgaudit ",
            ";3485; 0 0 COMMENT - EXTENSION pgaudit "
        ],
        "{given}"
    );
    assert!(given.contains("\n2; 3079 16385 EXTENSION - pg_trgm \n"));
    assert!(!list.exists(), "the list file is removed after the run");
    app.tap(cx, "backup-close");
}

/// After a failure: the user unticked both, the restore failed on the
/// extension and the owner; one view says both, the plain words first,
/// and one retry leaves out both, through the same confirmation.
#[gpui_kit::test]
fn a_failed_extension_and_owner_are_said_and_one_retry_leaves_out_both(cx: &mut TestAppContext) {
    if !super::tool_wizards::fake_tools_run() {
        return;
    }
    if !testing::available(Engine::Postgres) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let client = client(&root);
    let prod = root.join("prod_dump");
    std::fs::write(&prod, b"PGDMP\x01\x0e\x00 an archive").unwrap();
    let app = connected(cx, Engine::Postgres);
    app.use_client(cx, ClientFamily::Postgres, &client);
    let dialog = app
        .simple_dialog(cx, "kerno_ui_restore_never", true)
        .expect("the restore");
    app.wait_backup(cx, &dialog, &["choose a file"]);
    open_file(&app, cx, &dialog, &prod);
    app.wait_until(cx, "the preflight", |app, cx| {
        app.has(cx, "restore-preflight")
    });
    app.tap(cx, "restore-leave-out");
    app.tap(cx, "simple-no-owner");
    let unticked = args(cx, &dialog);
    assert!(
        use_list(&unticked).is_none() && !unticked.contains(&"--no-owner".into()),
        "{unticked:?}"
    );

    let state = restore(&app, cx, &dialog);
    assert!(state.starts_with("failed: pg_restore failed"), "{state}");
    app.wait_until(cx, "the failure said", |app, cx| {
        app.has(cx, "restore-roles")
    });
    assert!(paints(
        &app,
        cx,
        "The backup needs the extension “pgaudit”, which this server does not have. A restore \
         cannot install an extension: its files are installed on the machine PostgreSQL runs on. \
         Restore again without it: what in the backup uses it may then fail, and is said here. \
         Or install it on that machine, then restore again."
    ));
    assert!(paints(
        &app,
        cx,
        "The backup names the role “app_owner” as the owner of its objects, and this server has \
         no such role."
    ));

    app.tap(cx, "restore-without-owners");
    app.wait_until(cx, "the confirmation", |app, cx| app.has(cx, "sql-preview"));
    let again = args(cx, &dialog);
    assert!(
        use_list(&again).is_some()
            && again.contains(&"--no-owner".into())
            && again.contains(&"--no-privileges".into())
            && again.contains(&"--single-transaction".into()),
        "{again:?}"
    );
    app.tap(cx, "sql-confirm");
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    assert!(state.starts_with("done: pg_restore finished"), "{state}");
    assert!(!app.has(cx, "restore-roles") && !app.has(cx, "restore-without-owners"));
    app.tap(cx, "backup-close");
}

/// The real tools on the test server, scratch databases only: an archive of
/// a database with an extension the server offers (whose name needs
/// quotes); the preflight reads the real list, finds the extension and the
/// owner, and asks nothing since the server has both; the restore with
/// the extension left out through `--use-list` restores the rest, without
/// the extension.
#[gpui_kit::test]
fn a_real_archive_restores_without_a_left_out_extension(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let (source, target) = ("kerno_test_k39_src", "kerno_test_k39_dst");
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
        "CREATE EXTENSION \"uuid-ossp\"; CREATE TABLE kerno_test_k39_kept (id int); \
         INSERT INTO kerno_test_k39_kept VALUES (1), (2), (3)",
    );
    let app = connected(cx, Engine::Postgres);
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();

    let dialog = app.simple_dialog(cx, source, false).expect("the backup");
    let state = app.wait_backup(cx, &dialog, &["ready", "no tool", "old client"]);
    if !state.starts_with("ready") {
        eprintln!("SKIPPED: no usable pg_dump / pg_restore: {state}");
        return;
    }
    app.tap(cx, "simple-choose-file");
    let path = root.join("k39");
    cx.simulate_new_path_selection(move |_| Some(path));
    cx.run_until_parked();
    app.tap(cx, "backup-start");
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    assert!(state.starts_with("done: "), "{state}");
    let file = cx.update(|cx| dialog.read(cx).written()).expect("a file");
    app.tap(cx, "backup-close");

    let dialog = app.simple_dialog(cx, target, true).expect("the restore");
    app.wait_backup(cx, &dialog, &["choose a file"]);
    open_file(&app, cx, &dialog, &file);
    cx.run_until_parked();
    let list = cx
        .update(|cx| dialog.read(cx).archive_list())
        .expect("the real list");
    assert_eq!(kerno_core::tools::list_extensions(&list), ["uuid-ossp"]);
    assert_eq!(
        kerno_core::tools::missing_owners(&list, &[]),
        ["kerno"],
        "{list}"
    );
    for _ in 0..40 {
        cx.run_until_parked();
        assert!(!app.has(cx, "restore-preflight"), "the server has both");
        std::thread::sleep(std::time::Duration::from_millis(25));
    }

    cx.update(|cx| {
        dialog.update(cx, |dialog, _| {
            dialog.set_leave_out(vec!["uuid-ossp".into()])
        })
    });
    assert!(use_list(&args(cx, &dialog)).is_some());
    let state = restore(&app, cx, &dialog);
    assert!(state.starts_with("done: "), "{state}");
    assert_eq!(
        on_database(
            Engine::Postgres,
            Some(target),
            "SELECT (SELECT count(*) FROM kerno_test_k39_kept) || ' ' || (SELECT count(*) FROM \
             pg_extension WHERE extname = 'uuid-ossp')"
        ),
        "3 0"
    );
    app.tap(cx, "backup-close");
}
