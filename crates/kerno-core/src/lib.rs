//! Kerno's engine-neutral core: the driver trait, the value and type model,
//! connection settings, SSH tunnels and SQL helpers. No UI dependency.

pub mod config;
pub mod connector;
pub mod dialect;
pub mod driver;
pub mod error;
pub mod exec;
pub mod result;
pub mod schema;
pub mod sql;
pub mod ssh;
#[cfg(feature = "test-support")]
pub mod testing;

pub use config::{
    ConnectParams, ConnectionConfig, Engine, Environment, Secret, Secrets, SshAuth, SshConfig,
    TlsConfig, TlsMode,
};
pub use connector::{Connector, HostKeyStore};
pub use dialect::Dialect;
pub use driver::{AdminView, Driver, Session, TxState};
pub use error::{DbError, Error, Result};
pub use exec::{ExecEvent, ExecSink, Execution, Notice, RowLimit};
pub use result::{BatchBuilder, Column, ResultStore, RowBatch, TypeKind};
