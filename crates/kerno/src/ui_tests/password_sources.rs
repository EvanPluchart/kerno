//! J3, passwords from a command, AWS RDS IAM, 1Password, `.pgpass` and
//! `.my.cnf` (DBeaver #3270, #9413; Beekeeper #1443, #530): the dialog's
//! password choice and the fields of each, RDS IAM moving TLS to require,
//! "Run now", the origin line of a test and of a connection, and an
//! imported command that waits for its confirmation. Every test gives the
//! app a temporary `PATH` and `HOME` ([`testing::fake_aws`]): the real
//! `aws`, `op` and the developer's password files are never reached.

use kerno_core::config::{PasswordSource, TlsMode};

use super::*;

/// The text painted in the window's last frame.
fn paints(app: &Fixture, cx: &mut TestAppContext, part: &str) -> bool {
    app.step(cx, |window, _| {
        gpui_kit::base::test_support::texts(window)
            .into_iter()
            .any(|text| text.text().contains(part))
    })
}

/// Opens the app with a fake `aws` and `dir` as `HOME`.
fn open_with_env(cx: &mut TestAppContext, storage: Arc<Storage>, dir: &Path) -> Fixture {
    let app = open(cx, storage);
    let env = testing::fake_aws(dir);
    cx.update(|cx| {
        app.connections
            .update(cx, |connections, _| connections.set_password_env(Some(env)))
    });
    app
}

/// A URL for the test server of `engine`, without a password.
fn url(engine: Engine) -> String {
    let scheme = match engine {
        Engine::Postgres => "postgres",
        Engine::MySql => "mysql",
        Engine::MariaDb => "mariadb",
    };
    format!("{scheme}://kerno@127.0.0.1:{}/kerno", testing::port(engine))
}

/// Picks where the password comes from, in the dialog's list.
fn choose(app: &Fixture, cx: &mut TestAppContext, label: &str) {
    app.click(cx, "conn-password-source");
    app.click_label(cx, label);
}

fn new_connection(app: &Fixture, cx: &mut TestAppContext, engine: Engine) {
    app.click(cx, "empty-new-connection");
    app.click(cx, "conn-url");
    app.type_text(cx, &url(engine));
    app.press(cx, &["enter"]);
}

fn test_passes(app: &Fixture, cx: &mut TestAppContext) {
    app.click(cx, "conn-test");
    app.wait_until(cx, "the passed test", |app, cx| {
        assert!(!app.has(cx, "conn-test-failed"), "{}", app.situation(cx));
        app.has(cx, "conn-test-passed")
    });
}

/// Each choice shows its own fields, and a command its line as it runs.
#[gpui_kit::test]
fn each_password_choice_shows_its_fields(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let app = open_with_env(cx, memory_storage(), dir.path());
    app.click(cx, "empty-new-connection");
    assert!(app.has(cx, "conn-password"));
    assert!(paints(&app, cx, "Left empty, Kerno reads ~/.pgpass"));

    choose(&app, cx, "Ask each time");
    assert!(app.has(cx, "conn-password"));
    assert!(paints(
        &app,
        cx,
        "Kerno will ask for the password each time"
    ));

    choose(&app, cx, "Command");
    assert!(!app.has(cx, "conn-password"), "a command needs no password");
    assert!(app.has(cx, "conn-password-program") && app.has(cx, "conn-password-timeout"));
    app.click(cx, "conn-password-program");
    app.type_text(cx, "get-pw");
    assert!(paints(&app, cx, "Runs: get-pw"));

    choose(&app, cx, "1Password");
    assert!(app.has(cx, "conn-op-reference"));
    app.click(cx, "conn-op-reference");
    app.type_text(cx, "op://Work/db/password");
    assert!(paints(&app, cx, "Runs: op read op://Work/db/password"));

    choose(&app, cx, "AWS RDS IAM");
    assert!(app.has(cx, "conn-rds-region") && app.has(cx, "conn-rds-profile"));
    app.click(cx, "conn-rds-region");
    app.type_text(cx, "eu-west-3");
    assert!(paints(
        &app,
        cx,
        "Runs: aws rds generate-db-auth-token --hostname localhost --port 5432 --username \
         postgres --region eu-west-3"
    ));
    app.click(cx, "conn-name");
    app.type_text(cx, "kerno_ui rds");
    app.click(cx, "conn-save");
    app.wait_until(cx, "the saved connection", |app, _| {
        app.storage.connections().unwrap().len() == 1
    });
    let stored = app.first_stored();
    assert_eq!(
        stored.password_source,
        PasswordSource::RdsIam {
            profile: None,
            region: "eu-west-3".into()
        }
    );
    assert_eq!(
        stored.tls.mode,
        TlsMode::Require,
        "RDS IAM moved TLS to require"
    );
    assert!(testing::fake_aws_args(dir.path()).is_empty(), "nothing ran");
}

/// GOAL-2 criterion 2: the RDS preset through a fake `aws` tests,
/// connects over TLS and says where the password came from; "Run now"
/// gives the length of the output, never the output.
#[gpui_kit::test]
fn the_rds_preset_connects_with_what_aws_prints_and_says_so(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let app = open_with_env(cx, memory_storage(), dir.path());
    new_connection(&app, cx, Engine::Postgres);
    choose(&app, cx, "AWS RDS IAM");
    app.click(cx, "conn-rds-region");
    app.type_text(cx, "us-east-1");

    app.click(cx, "conn-password-run");
    app.wait_until(cx, "the command's run", |app, cx| {
        assert!(
            !app.has(cx, "conn-password-run-failed"),
            "the command failed"
        );
        app.has(cx, "conn-password-run-result")
    });
    assert!(paints(
        &app,
        cx,
        &format!(
            "printed {} characters (not shown)",
            testing::DB_PASSWORD.len()
        )
    ));
    assert!(!paints(&app, cx, testing::DB_PASSWORD));

    test_passes(&app, cx);
    assert!(paints(&app, cx, "Password from the command `aws`"));
    assert_eq!(
        testing::fake_aws_args(dir.path())[..4],
        ["rds", "generate-db-auth-token", "--hostname", "127.0.0.1"]
    );

    app.click(cx, "conn-save-connect");
    app.wait_until(cx, "the connection", |app, cx| {
        // Saved on a storage thread before it is listed.
        if cx.update(|cx| app.connections.read(cx).saved().is_empty()) {
            return false;
        }
        assert!(
            !matches!(app.status(cx, 0), Status::Failed { .. }),
            "{:?}",
            app.status(cx, 0)
        );
        matches!(app.status(cx, 0), Status::Connected { .. })
    });
    let id = app.storage.connections().unwrap()[0].id.clone();
    assert_eq!(
        cx.update(|cx| app.connections.read(cx).password_origin(&id)),
        Some("Password from the command `aws`".to_string())
    );
    // The token is in no store.
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    let kept = kerno_core::secrets::load_secrets(secrets.as_ref(), &id).unwrap();
    assert!(kept.password.is_none(), "the command's output was stored");
    app.run_in_new_tab(
        cx,
        "select ssl::text from pg_stat_ssl where pid = pg_backend_pid()",
        "secondary-enter",
    );
    app.wait_until(cx, "the result", |app, cx| app.outcomes(cx) == ["1 rows"]);
    assert_eq!(app.result(cx, 0, 1).1, vec![vec![Some("true".to_string())]]);
}

/// With nothing stored, the engines' own password files are read from
/// HOME, and the test says which one gave the password.
#[gpui_kit::test]
fn pgpass_and_my_cnf_give_the_password_and_the_test_says_so(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    testing::password_file(
        dir.path(),
        ".pgpass",
        &format!("127.0.0.1:*:*:kerno:{}\n", testing::DB_PASSWORD),
        0o600,
    );
    testing::password_file(
        dir.path(),
        ".my.cnf",
        &format!("[client]\npassword='{}'\n", testing::DB_PASSWORD),
        0o600,
    );
    let app = open_with_env(cx, memory_storage(), dir.path());
    for (engine, file) in [
        (Engine::Postgres, "~/.pgpass"),
        (Engine::MySql, "~/.my.cnf"),
        (Engine::MariaDb, "~/.my.cnf"),
    ] {
        if !testing::available(engine) {
            continue;
        }
        new_connection(&app, cx, engine);
        test_passes(&app, cx);
        assert!(
            paints(&app, cx, &format!("Password from {file}")),
            "{engine:?}"
        );
        app.click(cx, "conn-cancel");
    }
}

/// An imported command is shown in full and runs only once confirmed:
/// connecting before says why, and the fake `aws` never ran.
#[gpui_kit::test]
fn an_imported_command_runs_only_once_confirmed(cx: &mut TestAppContext) {
    if !testing::available(Engine::MySql) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mut config = testing::config(Engine::MySql);
    config.name = "kerno_ui imported".into();
    config.password_source = PasswordSource::Command {
        program: "aws".into(),
        args: vec!["--user=${user}".into()],
        timeout_secs: 10,
    };
    let file = kerno_core::config::export_connections(std::slice::from_ref(&config)).unwrap();
    let imported = kerno_core::config::import_connections(&file).unwrap();
    assert!(imported[0].password_command_unconfirmed);
    let storage = memory_storage();
    let id = storage.save_connection(None, &imported[0]).unwrap();
    let app = open_with_env(cx, storage, dir.path());

    app.open_connection_row(cx, 0);
    app.wait_until(cx, "the refused connection", |app, cx| {
        matches!(app.status(cx, 0), Status::Failed { .. })
    });
    let Status::Failed { message } = app.status(cx, 0) else {
        unreachable!()
    };
    let explained = kerno_core::error::explain(&message, &imported[0]);
    assert!(
        explained.headline.contains("imported file"),
        "{explained:?}"
    );
    assert!(testing::fake_aws_args(dir.path()).is_empty(), "aws ran");

    app.sidebar_does(cx, Subject::Connection(id), SidebarAction::Edit);
    app.wait_until(cx, "the connection dialog", |app, cx| {
        app.has(cx, "conn-password-unconfirmed")
    });
    assert!(paints(&app, cx, "Runs: aws --user=kerno"));
    // "Run now" waits for the confirmation too.
    app.click(cx, "conn-password-run");
    assert!(!app.has(cx, "conn-password-run-result"));
    assert!(testing::fake_aws_args(dir.path()).is_empty(), "aws ran");

    app.click(cx, "conn-password-confirm");
    assert!(!app.has(cx, "conn-password-unconfirmed"));
    app.click(cx, "conn-save-connect");
    app.wait_until(cx, "the connection", |app, cx| {
        matches!(app.status(cx, 0), Status::Connected { .. })
    });
    assert!(!app.first_stored().password_command_unconfirmed);
    assert_eq!(testing::fake_aws_args(dir.path()), ["--user=kerno"]);
}
