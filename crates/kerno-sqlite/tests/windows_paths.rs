//! A SQLite database named by a share's network name is refused before
//! anything touches it, on Windows (K69, D-138): opening it would log in to
//! that machine with the user's account. A mapped drive letter is taken.

#![cfg(windows)]

use kerno_core::config::Engine;
use kerno_core::{Driver, Error, testing};
use kerno_sqlite::SqliteDriver;

/// Mutation seen: the check in the driver's connect removed.
#[tokio::test]
async fn a_database_on_a_share_is_refused_by_its_network_name() {
    for file in [
        r"\\k69-nowhere\share\shop.db",
        "//k69-nowhere/share/shop.db",
    ] {
        // Built by hand, as an imported connection: no form on the way.
        let mut params = testing::params(Engine::Postgres);
        params.engine = Engine::Sqlite;
        params.file = Some(file.into());
        let Err(error) = SqliteDriver.connect(&params).await else {
            panic!("{file}: refused");
        };
        let Error::Config(message) = &error else {
            panic!("{file}: {error:?}");
        };
        assert!(
            message.contains("Map the share to a drive letter"),
            "{message}"
        );
        eprintln!("K69 PROVED: {message}");
    }
}
