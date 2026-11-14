//! J2, SSH through the system's OpenSSH (DBeaver #5845, #14749, #4516,
//! #17976; Beekeeper #1240, #1344, #940): the SSH tab offers the aliases of
//! the SSH configuration, a connection saved with an alias and a jump host
//! connects and runs a query, and the built-in client still logs in with a
//! password. Every test passes its own `ssh_config`: the developer's SSH
//! files are never read.

use kerno_core::config::{SshAuth, SshClient, SshConfig};
use kerno_core::openssh::OpenSshEnv;

use super::*;

/// Gives the app the OpenSSH of `config`, as `main` gives it the user's.
/// `ssh` never gets an answer from the test binary: `false` refuses its
/// questions.
fn use_openssh(app: &Fixture, cx: &mut TestAppContext, config: &Path) {
    let env = OpenSshEnv {
        ssh: kerno_core::openssh::find_ssh(std::env::var_os("PATH").as_deref())
            .unwrap_or_else(|| "/usr/bin/ssh".into()),
        config: Some(config.to_path_buf()),
        aliases_from: Some(config.to_path_buf()),
        askpass: "/usr/bin/false".into(),
    };
    cx.update(|cx| {
        app.connections
            .update(cx, |connections, _| connections.set_openssh(Some(env)))
    });
}

/// Beekeeper #1344, DBeaver #4516: the host field lists the `Host` aliases
/// (its `Include`s too), and a new connection uses OpenSSH.
#[gpui_kit::test]
fn the_ssh_tab_offers_the_aliases_of_the_ssh_configuration(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("ssh_config");
    std::fs::write(
        &config,
        "Host kerno-ui-bastion\n  HostName 10.0.0.1\nInclude more.conf\nHost *\n  User x\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("more.conf"), "Host kerno-ui-other\n").unwrap();
    let app = open(cx, memory_storage());
    use_openssh(&app, cx, &config);

    app.click(cx, "empty-new-connection");
    app.click_label(cx, "SSH tunnel");
    app.click(cx, "conn-ssh-enabled");
    app.wait_until(cx, "the aliases", |app, cx| {
        app.has(cx, "conn-ssh-host-list")
    });
    // OpenSSH's own fields are shown, the built-in client's password is not.
    assert!(app.has(cx, "conn-ssh-jump"));
    assert!(!app.shows_label(cx, "Password"));
    app.click(cx, "conn-ssh-host-list");
    assert!(app.shows_label(cx, "kerno-ui-other"));
    app.click_label(cx, "kerno-ui-bastion");

    app.click_label(cx, "General");
    app.click(cx, "conn-name");
    app.type_text(cx, "through an alias");
    app.click(cx, "conn-save");
    app.wait_until(cx, "the saved connection", |app, _| {
        app.storage.connections().unwrap().len() == 1
    });
    let ssh = app.first_stored().ssh.expect("the tunnel");
    assert_eq!(
        (ssh.client, ssh.host.as_str(), ssh.port, ssh.user.as_str()),
        (SshClient::OpenSsh, "kerno-ui-bastion", 0, ""),
        "the port and the user are left to the SSH configuration"
    );
}

/// Without OpenSSH the dialog says so, and offers the built-in client.
#[gpui_kit::test]
fn without_openssh_new_connections_use_the_built_in_client(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    app.click(cx, "empty-new-connection");
    app.click_label(cx, "SSH tunnel");
    app.click(cx, "conn-ssh-enabled");
    assert!(!app.has(cx, "conn-ssh-jump"));
    assert!(app.shows_label(cx, "Password"));
    app.click_label(cx, "OpenSSH (your ~/.ssh/config)");
    assert!(app.has(cx, "conn-ssh-openssh-missing"));
    assert!(app.has(cx, "conn-ssh-jump"));
}

/// DBeaver #14749, Beekeeper #940: an alias of the SSH configuration plus
/// the Jump host field, nothing else, and the connection works end to end.
#[gpui_kit::test]
fn a_connection_through_an_alias_and_a_jump_host_runs_a_query(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) || !testing::ssh_jump_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let fixture = testing::openssh(dir.path(), "/usr/bin/false".into(), true);
    // The bastion as the jump host sees it, under an alias of its own.
    let mut text = std::fs::read_to_string(&fixture.config).unwrap();
    text.insert_str(
        0,
        &format!(
            "Host kerno-ui-target\n  HostName kerno-ssh\n  Port 22\n  User kerno\n  \
             IdentityFile {}\n",
            testing::generated("ssh/id_ed25519").display()
        ),
    );
    std::fs::write(&fixture.config, text).unwrap();

    let mut config = testing::config(Engine::Postgres);
    config.name = "kerno_ui openssh".into();
    config.host = testing::service_name(Engine::Postgres).into();
    config.port = 5432;
    config.ssh = Some(SshConfig {
        host: "kerno-ui-target".into(),
        port: 0,
        user: String::new(),
        auth: SshAuth::Agent { socket: None },
        keepalive_secs: 30,
        client: SshClient::OpenSsh,
        jump: Some("kerno-jump".into()),
    });
    let storage = memory_storage();
    let id = storage.save_connection(None, &config).unwrap();
    let app = open(cx, storage);
    use_openssh(&app, cx, &fixture.config);
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    kerno_core::secrets::save_secrets(secrets.as_ref(), &id, &testing::secrets()).unwrap();

    app.open_connection_row(cx, 0);
    app.wait_until(cx, "the connection through the jump host", |app, cx| {
        assert!(
            !matches!(app.status(cx, 0), Status::Failed { .. }),
            "{:?}",
            app.status(cx, 0)
        );
        matches!(app.status(cx, 0), Status::Connected { .. })
    });
    app.run_in_new_tab(cx, "select 40 + 2 as answer", "secondary-enter");
    app.wait_until(cx, "the result", |app, cx| app.outcomes(cx) == ["1 rows"]);
    assert_eq!(
        app.result(cx, 0, 1),
        (
            vec!["answer".to_string()],
            vec![vec![Some("42".to_string())]]
        )
    );
}

/// The built-in client still logs in with a password, chosen in the dialog
/// beside OpenSSH.
#[gpui_kit::test]
fn the_built_in_client_still_logs_in_with_a_password(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) || !testing::ssh_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let fixture = testing::openssh(dir.path(), "/usr/bin/false".into(), true);
    let app = open(cx, memory_storage());
    use_openssh(&app, cx, &fixture.config);

    app.click(cx, "empty-new-connection");
    app.click(cx, "conn-url");
    app.type_text(
        cx,
        &format!(
            "postgres://kerno:{}@{}:5432/kerno?sslmode=disable",
            testing::DB_PASSWORD,
            testing::service_name(Engine::Postgres)
        ),
    );
    app.press(cx, &["enter"]);
    app.click_label(cx, "SSH tunnel");
    app.click(cx, "conn-ssh-enabled");
    app.click_label(cx, "Built-in");
    for (id, text) in [
        ("conn-ssh-host", "127.0.0.1".to_string()),
        ("conn-ssh-port", testing::SSH_PORT.to_string()),
        ("conn-ssh-user", "kerno".to_string()),
    ] {
        app.click(cx, id);
        app.press(cx, &["secondary-a", "backspace"]);
        app.type_text(cx, &text);
    }
    app.click_label(cx, "Password");
    app.click(cx, "conn-ssh-password");
    app.type_text(cx, testing::SSH_PASSWORD);

    // The built-in client asks about the host key first.
    app.click(cx, "conn-test");
    app.wait_until(cx, "the host key question", |app, cx| {
        app.has(cx, "conn-trust-host")
    });
    app.click(cx, "conn-trust-host");
    app.wait_until(cx, "the passed test", |app, cx| {
        assert!(!app.has(cx, "conn-test-failed"), "{}", app.situation(cx));
        app.has(cx, "conn-test-passed")
    });
}
