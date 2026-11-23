//! K35: a restore whose backup names a role the server does not have says
//! so in plain words and runs again without the backup's owners and
//! privileges (docs/plans/M12.md, D-108).

use std::path::Path;

use kerno_core::tools::ClientFamily;

use super::tool_wizards::{fake_client, paints};
use super::*;
use crate::backup_dialog::BackupDialog;

/// The first outside user's pg_restore output (2026-11-23), verbatim.
const FRENCH: &str = "\
pg_restore: connexion à la base de données pour la restauration
pg_restore: exécution SELECT pg_catalog.set_config('search_path', '', false);
pg_restore: création de SCHEMA « archive »
pg_restore: pendant le traitement de la TOC (« PROCESSING TOC ») :
pg_restore: de l'entrée TOC 12 ; 2615 117450 SCHEMA archive app_owner
pg_restore: erreur : could not execute query: ERROR:  role \"app_owner\" does not exist
La commande était : ALTER SCHEMA archive OWNER TO app_owner;";

/// The arguments of the command the dialog would run now.
fn args(cx: &mut TestAppContext, dialog: &Entity<BackupDialog>) -> Vec<String> {
    cx.update(|cx| dialog.read(cx).pending_command(cx))
        .expect("a command")
        .args
}

/// Restores `file` through the simple view: Restore…, then the
/// confirmation (twice on production).
fn restore(app: &Fixture, cx: &mut TestAppContext, dialog: &Entity<BackupDialog>, twice: bool) {
    app.tap(cx, "restore-start");
    app.wait_until(cx, "the confirmation", |app, cx| app.has(cx, "sql-preview"));
    app.tap(cx, "sql-confirm");
    if twice {
        assert!(app.has(cx, "sql-preview"), "production asks a second time");
        app.tap(cx, "sql-confirm");
    }
    app.wait_backup(cx, dialog, &["done", "failed"]);
}

fn open_file(app: &Fixture, cx: &mut TestAppContext, dialog: &Entity<BackupDialog>, file: &Path) {
    app.wait_backup(cx, dialog, &["choose a file"]);
    app.tap(cx, "restore-choose-file");
    app.answer_open_prompt(cx, file);
}

/// The user's case with a stand-in pg_restore that speaks French: the plain
/// words first (the role, what to do), the tool's output below as before;
/// "Restore without owners and privileges…" runs the same restore with
/// `--no-owner --no-privileges`, through the same confirmations, and the
/// simple view and the wizard show what it set.
#[gpui_kit::test]
fn a_missing_owner_is_said_and_the_restore_runs_again_without_owners(cx: &mut TestAppContext) {
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
        "pg_restore (PostgreSQL) 18.7",
        &format!(
            "case \" $* \" in *\" --list \"*) exit 0;; esac\n\
             case \" $* \" in *\" --no-owner \"*) echo \"$*\" > {ran}; echo restored; exit 0;; esac\n\
             cat >&2 <<'END'\n{FRENCH}\n\nEND\nexit 1",
            ran = ran.display()
        ),
    );
    let file = root.join("prod_dump");
    std::fs::write(&file, b"PGDMP\x01\x0e\x00 an archive").unwrap();

    let mut config = testing::config(Engine::Postgres);
    config.environment = kerno_core::environment::PRODUCTION.into();
    let app = connected_as(cx, config);
    app.use_client(cx, ClientFamily::Postgres, &client);
    let dialog = app
        .simple_dialog(cx, "kerno_ui_restore_never", true)
        .expect("the restore");
    open_file(&app, cx, &dialog, &file);
    let state = app.wait_backup(cx, &dialog, &["ready", "file", "no tool"]);
    assert!(state.starts_with("ready: "), "{state}");
    // The simple view offers it before the run too, off by default.
    assert!(app.has(cx, "simple-no-owner") && paints(&app, cx, "belongs to “kerno”"));
    let first = args(cx, &dialog);
    assert!(
        !first.iter().any(|arg| arg == "--no-owner")
            && first.contains(&"--single-transaction".into()),
        "{first:?}"
    );

    restore(&app, cx, &dialog, true);
    app.wait_until(cx, "the roles named", |app, cx| {
        app.has(cx, "restore-roles")
    });
    assert!(paints(
        &app,
        cx,
        "The backup names the role “app_owner” as the owner of its objects, and this server has \
         no such role. Restore again without the backup's owners and privileges: what it \
         restores then belongs to “kerno”, and the backup's grants are left out. Or create the \
         role on this server, then restore again."
    ));
    // The tool's output and the failure line, below, as before.
    assert!(paints(
        &app,
        cx,
        "pg_restore failed (exit status 1). It ran in one transaction"
    ));
    assert!(
        cx.update(|cx| dialog.read(cx).log())
            .iter()
            .any(|line| line == "La commande était : ALTER SCHEMA archive OWNER TO app_owner;")
    );
    assert!(!ran.exists());

    app.tap(cx, "restore-without-owners");
    app.wait_until(cx, "the confirmation", |app, cx| app.has(cx, "sql-preview"));
    let second = args(cx, &dialog);
    assert_eq!(
        second
            .iter()
            .filter(|arg| arg.starts_with("--no-") && *arg != "--no-password")
            .collect::<Vec<_>>(),
        ["--no-owner", "--no-privileges"],
        "{second:?}"
    );
    assert!(
        second.contains(&"--single-transaction".into()),
        "{second:?}"
    );
    app.tap(cx, "sql-confirm");
    assert!(
        app.has(cx, "sql-preview") && !ran.exists(),
        "production asks twice"
    );
    app.tap(cx, "sql-confirm");
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    assert!(state.starts_with("done: pg_restore finished"), "{state}");
    let ran = std::fs::read_to_string(&ran).unwrap();
    assert!(
        ran.contains("--no-owner --no-privileges --single-transaction"),
        "{ran}"
    );
    assert!(!app.has(cx, "restore-roles") && !app.has(cx, "restore-without-owners"));

    // Back in the simple view the checkbox shows what the retry set, and
    // the wizard keeps it with nothing to reset.
    app.tap(cx, "backup-back");
    assert!(app.has(cx, "simple-no-owner"));
    assert!(args(cx, &dialog).contains(&"--no-owner".into()));
    app.tap(cx, "backup-advanced");
    app.tap(cx, "wizard-simple");
    assert!(!app.has(cx, "simple-reset-text"), "nothing hidden to reset");
    // The checkbox clears both.
    app.tap(cx, "simple-no-owner");
    let cleared = args(cx, &dialog);
    assert!(
        !cleared
            .iter()
            .any(|arg| arg == "--no-owner" || arg == "--no-privileges"),
        "{cleared:?}"
    );
    app.tap(cx, "backup-close");
}

/// A plain SQL script run by psql: the role found from the script's line
/// psql points at (its message in French), what to do said, and no button,
/// since psql cannot leave out what the script's text holds.
#[gpui_kit::test]
fn a_plain_script_says_what_to_do_without_a_retry(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let client = fake_client(
        &root,
        "client",
        ClientFamily::Postgres,
        "psql (PostgreSQL) 99.0",
        "cat > /dev/null\necho 'CREATE SCHEMA'\n\
         echo 'psql:<stdin>:3: ERREUR:  le rôle « app_owner » n'\"'\"'existe pas' >&2\nexit 3",
    );
    let file = root.join("prod.sql");
    std::fs::write(
        &file,
        "SET client_encoding = 'UTF8';\nCREATE SCHEMA archive;\nALTER SCHEMA archive OWNER TO \
         app_owner;\n",
    )
    .unwrap();

    let app = connected(cx, Engine::Postgres);
    app.use_client(cx, ClientFamily::Postgres, &client);
    let dialog = app
        .simple_dialog(cx, "kerno_ui_restore_never", true)
        .expect("the restore");
    // The checkbox ticked for an archive does not follow to a script, which
    // runs as it is (pg_restore's options on it would be refused).
    let archive = root.join("prod_dump");
    std::fs::write(&archive, b"PGDMP\x01\x0e\x00 an archive").unwrap();
    open_file(&app, cx, &dialog, &archive);
    app.wait_backup(cx, &dialog, &["ready"]);
    app.tap(cx, "simple-no-owner");
    assert!(args(cx, &dialog).contains(&"--no-owner".into()));
    app.tap(cx, "restore-choose-file");
    app.answer_open_prompt(cx, &file);
    let state = app.wait_backup(cx, &dialog, &["ready", "not ready", "file"]);
    assert!(
        state.starts_with("ready: ") && state.contains("psql"),
        "{state}"
    );
    assert!(
        !app.has(cx, "simple-no-owner"),
        "a script cannot leave them out"
    );
    restore(&app, cx, &dialog, false);
    app.wait_until(cx, "the roles named", |app, cx| {
        app.has(cx, "restore-roles")
    });
    assert!(paints(
        &app,
        cx,
        "The backup names the role “app_owner” as the owner of its objects, and this server has \
         no such role. A plain SQL script sets its owners and rights in its own text, so psql \
         cannot leave them out. Create the role on this server, then restore again; or back up \
         again as a compressed archive, which a restore can run without them."
    ));
    assert!(paints(&app, cx, "psql failed (exit status 3)"));
    assert!(!app.has(cx, "restore-without-owners"));
}

/// The real tools on the test server: a schema owned by a scratch role
/// whose name needs quotes, backed up as an archive; the role dropped; the
/// restore into another database fails and says so; the retry restores it,
/// owned by the user the restore connects as.
#[gpui_kit::test]
fn an_archive_from_another_server_restores_without_its_owners(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let (source, target) = ("kerno_test_k35_src", "kerno_test_k35_dst");
    let role = format!("kerno_test_k35_{}_Rôle \"Été\"", std::process::id());
    let quoted = format!("\"{}\"", role.replace('"', "\"\""));
    let _scratch = Scratch::new(
        Engine::Postgres,
        &[
            &format!("DROP DATABASE IF EXISTS {source} WITH (FORCE)"),
            &format!("DROP DATABASE IF EXISTS {target} WITH (FORCE)"),
            &format!("DROP ROLE IF EXISTS {quoted}"),
        ],
        &[
            &format!("CREATE ROLE {quoted}"),
            &format!("CREATE DATABASE {source}"),
            &format!("CREATE DATABASE {target}"),
        ],
    );
    on_database(
        Engine::Postgres,
        Some(source),
        &format!(
            "CREATE SCHEMA kerno_test_k35_archive AUTHORIZATION {quoted}; \
             CREATE TABLE kerno_test_k35_archive.kept (id int); \
             INSERT INTO kerno_test_k35_archive.kept VALUES (1), (2); \
             ALTER TABLE kerno_test_k35_archive.kept OWNER TO {quoted}"
        ),
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
    let path = root.join("k35");
    cx.simulate_new_path_selection(move |_| Some(path));
    cx.run_until_parked();
    app.tap(cx, "backup-start");
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    assert!(state.starts_with("done: "), "{state}");
    let file = cx.update(|cx| dialog.read(cx).written()).expect("a file");
    app.tap(cx, "backup-close");
    // The role exists on the server the backup came from, not on this one.
    on_server(
        Engine::Postgres,
        &format!("DROP DATABASE {source} WITH (FORCE)"),
    );
    on_server(Engine::Postgres, &format!("DROP ROLE {quoted}"));

    let dialog = app.simple_dialog(cx, target, true).expect("the restore");
    open_file(&app, cx, &dialog, &file);
    let state = app.wait_backup(cx, &dialog, &["ready", "file", "no tool"]);
    assert!(state.starts_with("ready: "), "{state}");
    // Since K39 the archive's list says so before the run and ticks the
    // box; the user unticks it, and the restore fails as before.
    app.wait_until(cx, "the preflight", |app, cx| {
        app.has(cx, "restore-preflight")
    });
    assert!(args(cx, &dialog).contains(&"--no-owner".into()));
    app.tap(cx, "simple-no-owner");
    restore(&app, cx, &dialog, false);
    assert!(
        app.wizard_state(cx, &dialog)
            .starts_with("failed: pg_restore failed"),
        "{}",
        app.wizard_state(cx, &dialog)
    );
    app.wait_until(cx, "the roles named", |app, cx| {
        app.has(cx, "restore-roles")
    });
    assert!(paints(
        &app,
        cx,
        &format!(
            "The backup names the role “{role}” as the owner of its objects, and this server has \
             no such role."
        )
    ));
    app.tap(cx, "restore-without-owners");
    app.wait_until(cx, "the confirmation", |app, cx| app.has(cx, "sql-preview"));
    app.tap(cx, "sql-confirm");
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    assert!(state.starts_with("done: "), "{state}");
    assert_eq!(
        on_database(
            Engine::Postgres,
            Some(target),
            "SELECT nspowner::regrole::text || ' ' || (SELECT count(*) FROM \
             kerno_test_k35_archive.kept) FROM pg_namespace WHERE nspname = \
             'kerno_test_k35_archive'"
        ),
        "kerno 2"
    );
    app.tap(cx, "backup-close");
}
