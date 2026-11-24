//! J3, passwords from a command and from the engines' password files
//! (DBeaver #3270, #9413; Beekeeper #1443, #530), through the public API
//! the connector uses: a fake `aws` on a temporary `PATH`, a `.pgpass` and
//! a `.my.cnf` in a temporary `HOME`. The drivers' `password_sources` tests
//! log in with them.

// The fake `aws` is a `/bin/sh` script, the files have modes: Unix only (K64).
#![cfg(unix)]

use kerno_core::config::{ConnectionConfig, Engine, PasswordSource, Secret, TlsMode};
use kerno_core::password::{self, PasswordOrigin};
use kerno_core::testing;

fn rds(engine: Engine) -> ConnectionConfig {
    let mut config = testing::config(engine);
    config.tls.mode = TlsMode::Require;
    config.password_source = PasswordSource::RdsIam {
        profile: Some("kerno-profile".into()),
        region: "eu-west-3".into(),
    };
    config
}

#[tokio::test]
async fn the_rds_preset_runs_aws_with_the_connection_s_values() {
    let dir = tempfile::tempdir().unwrap();
    let env = testing::fake_aws(dir.path());
    let config = rds(Engine::Postgres);
    config.validate().unwrap();
    let resolved = password::resolve(&config, Some(&Secret::new("stale")), None, Some(&env))
        .await
        .unwrap();
    assert_eq!(
        resolved.password.unwrap().expose(),
        testing::DB_PASSWORD,
        "one trailing newline trimmed, the stored password not used"
    );
    assert_eq!(resolved.origin, Some(PasswordOrigin::Command("aws".into())));
    assert_eq!(
        testing::fake_aws_args(dir.path()),
        [
            "rds",
            "generate-db-auth-token",
            "--hostname",
            "127.0.0.1",
            "--port",
            "55432",
            "--username",
            "kerno",
            "--region",
            "eu-west-3",
            "--profile",
            "kerno-profile",
        ]
    );
}

#[tokio::test]
async fn password_files_are_read_from_the_given_home_only() {
    let dir = tempfile::tempdir().unwrap();
    let env = testing::fake_aws(dir.path());
    testing::password_file(
        dir.path(),
        ".pgpass",
        "127.0.0.1:55432:other:kerno:wrong\n127.0.0.1:55432:kerno:kerno:from-pgpass\n",
        0o600,
    );
    testing::password_file(
        dir.path(),
        ".my.cnf",
        "[client]\npassword = \"from-my-cnf\"\n",
        0o644,
    );
    let postgres = testing::config(Engine::Postgres);
    let resolved = password::resolve(&postgres, None, None, Some(&env))
        .await
        .unwrap();
    assert_eq!(resolved.password.unwrap().expose(), "from-pgpass");
    assert_eq!(
        resolved.origin,
        Some(PasswordOrigin::File("~/.pgpass".into()))
    );
    // Another database matches another line, or none.
    let other = password::resolve(&postgres, None, Some("other"), Some(&env))
        .await
        .unwrap();
    assert_eq!(other.password.unwrap().expose(), "wrong");
    for engine in [Engine::MySql, Engine::MariaDb] {
        let resolved = password::resolve(&testing::config(engine), None, None, Some(&env))
            .await
            .unwrap();
        assert_eq!(resolved.password.unwrap().expose(), "from-my-cnf");
        assert_eq!(
            resolved.origin,
            Some(PasswordOrigin::File("~/.my.cnf".into()))
        );
    }
    // MySQL ignores a world-writable option file.
    testing::password_file(dir.path(), ".my.cnf", "[client]\npassword=x\n", 0o666);
    let ignored = password::resolve(&testing::config(Engine::MySql), None, None, Some(&env))
        .await
        .unwrap();
    assert!(ignored.password.is_none());
    assert!(
        ignored
            .warning
            .unwrap()
            .starts_with("~/.my.cnf was ignored: anyone can change it")
    );
}
