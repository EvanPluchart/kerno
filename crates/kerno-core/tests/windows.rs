//! K67 (D-136), Windows step 3a, against a real Windows: the home folder, libpq's password file
//! and the local client tools, read from the runner's own folders. The probe workflow
//! (`.github/workflows/windows-probe.yml`, branch `windows`) writes `~\.ssh\config`, a `.my.cnf`
//! and `%APPDATA%\postgresql\pgpass.conf` first; the image carries PostgreSQL 17 (EDB's
//! installer) and MySQL Server 8.4. Skipped everywhere else ([`testing::k67_runner`]).

use std::path::PathBuf;

use kerno_core::config::{ConnectionConfig, Engine};
use kerno_core::home;
use kerno_core::password::{self, PasswordEnv, PasswordOrigin};
use kerno_core::testing;
use kerno_core::tools::{self, ClientFamily, ClientRoots, ClientSource, Tool};

fn runner_var(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("{name} is set on Windows")))
}

#[test]
fn the_home_is_the_profile_folder_and_a_tilde_goes_under_it() {
    if !testing::k67_runner() {
        return;
    }
    let home = home::dir().expect("a home folder");
    assert_eq!(home, runner_var("USERPROFILE"));
    let config = home.join(".ssh").join("config");
    assert!(config.is_file(), "the workflow wrote {}", config.display());
    assert_eq!(home::expand(r"~\.ssh\config", Some(&home)), config);
    assert_eq!(home::expand("~/.ssh/config", Some(&home)), config);
    assert_eq!(
        home::known_hosts(),
        Some(home.join(".ssh").join("known_hosts"))
    );
    // Kerno's reader of OpenSSH's file finds its host there.
    let aliases = kerno_core::openssh::host_aliases(&config, Some(&home));
    assert_eq!(aliases, ["k67-probe"]);
    eprintln!(
        "K67 home: {} (USERPROFILE); ~\\.ssh\\config and ~/.ssh/config are {}; its hosts: \
         {aliases:?}",
        home.display(),
        config.display()
    );
}

#[tokio::test]
async fn libpq_s_password_file_is_in_appdata_and_no_my_cnf_is_read() {
    if !testing::k67_runner() {
        return;
    }
    let env = PasswordEnv::detect(None, home::dir(), None);
    let pgpass = runner_var("APPDATA").join("postgresql").join("pgpass.conf");
    assert_eq!(env.pgpassfile.as_ref(), Some(&pgpass));
    let config = |engine| ConnectionConfig {
        host: "127.0.0.1".into(),
        port: testing::port(engine),
        user: "kerno".into(),
        database: Some("kerno_test_k67".into()),
        ..ConnectionConfig::new(engine)
    };
    let postgres = password::resolve(&config(Engine::Postgres), None, None, Some(&env))
        .await
        .unwrap();
    assert_eq!(
        postgres.password.as_ref().map(|p| p.expose()),
        Some(testing::DB_PASSWORD)
    );
    let shown = r"~\AppData\Roaming\postgresql\pgpass.conf";
    assert_eq!(postgres.origin, Some(PasswordOrigin::File(shown.into())));
    // `~\.my.cnf` is there, and MySQL has no such file on Windows: not read.
    assert!(home::dir().unwrap().join(".my.cnf").is_file());
    let mysql = password::resolve(&config(Engine::MySql), None, None, Some(&env))
        .await
        .unwrap();
    assert!(mysql.password.is_none() && mysql.warning.is_none());
    eprintln!(
        "K67 password files: PostgreSQL's from {shown} ({}); MySQL's ~\\.my.cnf not read",
        pgpass.display()
    );
}

#[test]
fn the_local_clients_are_found_with_their_versions() {
    if !testing::k67_runner() {
        return;
    }
    let found = tools::detect_clients(&ClientRoots::system(Vec::new()));
    for install in &found {
        eprintln!(
            "K67 client: {} in {} ({})",
            install.label(),
            install.dir.display(),
            install
                .tools
                .iter()
                .map(|(_, path)| path.file_name().unwrap().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    for install in &found {
        assert!(
            !install.dir.to_string_lossy().starts_with(r"\\?\"),
            "{}",
            install.dir.display()
        );
    }
    let program_files = runner_var("ProgramFiles");
    let postgres = found
        .iter()
        .find(|install| install.family == ClientFamily::Postgres)
        .expect("PostgreSQL's client");
    assert_eq!(postgres.source, ClientSource::Edb);
    assert!(postgres.dir.starts_with(program_files.join("PostgreSQL")));
    assert_eq!(postgres.version.as_ref().map(|v| v.number.0), Some(17));
    for tool in ClientFamily::Postgres.tools() {
        let path = postgres.path(*tool).expect("every PostgreSQL tool");
        assert_eq!(path.extension().unwrap(), "exe", "{}", path.display());
    }
    let mysql: Vec<_> = found
        .iter()
        .filter(|install| install.family == ClientFamily::MySql)
        .collect();
    // The image puts MySQL's folder in PATH too: one install, the installer's.
    assert_eq!(mysql.len(), 1, "{mysql:#?}");
    let mysql = mysql[0];
    assert_eq!(mysql.source, ClientSource::ProgramFiles);
    assert!(mysql.dir.starts_with(program_files.join("MySQL")));
    let version = mysql.version.as_ref().expect("mysqldump's version");
    assert_eq!(
        (version.number.0, version.number.1, version.mariadb),
        (8, 4, false)
    );
    assert!(mysql.path(Tool::MysqlDump).is_some() && mysql.path(Tool::Mysql).is_some());
    // PATH alone, as `find` looks, finds the same mysqldump.exe.
    assert_eq!(
        tools::find(Tool::MysqlDump).map(|path| tools::real_path(&path)),
        mysql.path(Tool::MysqlDump).map(PathBuf::from)
    );
}
