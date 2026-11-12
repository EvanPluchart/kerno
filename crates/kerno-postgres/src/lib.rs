//! Kerno's PostgreSQL driver.

mod admin;
mod connect;
mod ddl;
mod dialect;
mod introspect;
mod session;
mod types;

use std::sync::Arc;

use async_trait::async_trait;
use kerno_core::config::ConnectParams;
use kerno_core::{Driver, Engine, Result, Session};

pub use dialect::PgDialect;
pub use session::PgSession;

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
