//! Kerno's PostgreSQL driver.

mod admin;
mod connect;
mod ddl;
mod dialect;
mod introspect;
mod plan;
mod session;
mod types;

use std::sync::Arc;

use async_trait::async_trait;
use kerno_core::config::ConnectParams;
use kerno_core::{Driver, Engine, Result, Session};

pub use dialect::PgDialect;
pub use session::PgSession;

/// The database a session opens in when the connection names none: libpq
/// falls back to the user name; "postgres" exists on almost every server
/// and is what people expect to land in.
pub const DEFAULT_DATABASE: &str = "postgres";

/// Opens PostgreSQL sessions.
#[derive(Debug, Clone, Copy, Default)]
pub struct PostgresDriver;

#[async_trait]
impl Driver for PostgresDriver {
    fn engines(&self) -> &'static [Engine] {
        &[Engine::Postgres]
    }

    async fn connect(&self, params: &ConnectParams) -> Result<Arc<dyn Session>> {
        Ok(Arc::new(PgSession::connect(params).await?))
    }
}
