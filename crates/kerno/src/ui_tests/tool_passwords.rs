//! The tools get the password a connection gets (follow-up of J3): a
//! backup of a connection whose password comes from a command runs the
//! command and the tool logs in with what it printed.

use kerno_core::config::PasswordSource;

use super::*;

#[gpui_kit::test]
fn a_backup_logs_in_with_a_command_sourced_password(cx: &mut TestAppContext) {
    for engine in [Engine::Postgres, Engine::MySql] {
        if !testing::available(engine) {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut config = testing::config(engine);
        config.password_source = PasswordSource::Command {
            program: "aws".into(),
            args: vec!["--for=${database}".into()],
            timeout_secs: 10,
        };
        // No password in the secret store: only the command gives one.
        let storage = memory_storage();
        storage.save_connection(None, &config).unwrap();
        let app = open(cx, storage);
        let env = testing::fake_aws(dir.path());
        cx.update(|cx| {
            app.connections
                .update(cx, |connections, _| connections.set_password_env(Some(env)))
        });
        app.open_connection_row(cx, 0);
        app.wait_until(cx, "the connection", |app, cx| {
            assert!(!matches!(app.status(cx, 0), Status::Failed { .. }));
            matches!(app.status(cx, 0), Status::Connected { .. })
        });
        std::fs::remove_file(dir.path().join("aws-args")).unwrap();

        let dialog = app
            .backup_dialog(cx, "kerno_other", false)
            .expect("the backup dialog");
        let state = app.wait_backup(cx, &dialog, &["no tool", "ready", "old client", "blocked"]);
        if state.starts_with("no tool") || state.starts_with("old client") {
            eprintln!("SKIPPED ({engine:?}): no usable client tool: {state}");
            continue;
        }
        assert!(state.starts_with("ready"), "{engine:?}: {state}");
        // The command ran for the tool, for its database.
        assert_eq!(
            testing::fake_aws_args(dir.path()),
            ["--for=kerno_other"],
            "{engine:?}"
        );
        // Never on the command line.
        assert!(!state.contains(testing::DB_PASSWORD), "{engine:?}");
        app.to_settings(cx, &dialog);
        let out = std::fs::canonicalize(dir.path()).unwrap();
        app.write_to(cx, &out, Some("by-command"));
        app.tap(cx, "backup-start");
        let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
        assert!(state.starts_with("done: "), "{engine:?}: {state}");
        let written = std::fs::read_dir(&out)
            .unwrap()
            .flatten()
            .find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("by-command")
            })
            .expect("the backup file");
        assert!(written.metadata().unwrap().len() > 0, "{engine:?}");
        app.tap(cx, "backup-close");
    }
}
