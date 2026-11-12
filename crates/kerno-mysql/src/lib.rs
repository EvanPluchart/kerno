//! Kerno's MySQL and MariaDB driver.

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

pub use dialect::MysqlDialect;
pub use session::MysqlSession;

/// Opens MySQL and MariaDB sessions. The two engines share one wire
/// protocol; a session reports which one it really reached.
#[derive(Debug, Clone, Copy, Default)]
pub struct MysqlDriver;

#[async_trait]
impl Driver for MysqlDriver {
    fn engines(&self) -> &'static [Engine] {
        &[Engine::MySql, Engine::MariaDb]
    }

    async fn connect(&self, params: &ConnectParams) -> Result<Arc<dyn Session>> {
        Ok(Arc::new(MysqlSession::connect(params).await?))
    }
}
