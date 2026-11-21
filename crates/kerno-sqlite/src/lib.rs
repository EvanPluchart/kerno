//! Kerno's SQLite driver: a database file opened in the app itself, no
//! server. Each session owns one connection on a thread of its own (D-102).

mod connect;
mod dialect;
mod introspect;
mod plan;
mod session;
mod types;

use std::sync::Arc;

use async_trait::async_trait;
use kerno_core::config::ConnectParams;
use kerno_core::{Driver, Engine, Result, Session};

pub use dialect::SqliteDialect;
pub use session::SqliteSession;

/// Makes an empty database at `path` (an empty file is one to SQLite):
/// `true` when the file was made, `false` when one was there already, which
/// is left exactly as it is, never emptied. Blocking.
pub fn new_database_file(path: &std::path::Path) -> std::io::Result<bool> {
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error),
    }
}

/// Opens SQLite database files.
#[derive(Debug, Clone, Copy, Default)]
pub struct SqliteDriver;

#[async_trait]
impl Driver for SqliteDriver {
    fn engines(&self) -> &'static [Engine] {
        &[Engine::Sqlite]
    }

    async fn connect(&self, params: &ConnectParams) -> Result<Arc<dyn Session>> {
        Ok(Arc::new(SqliteSession::connect(params).await?))
    }
}
