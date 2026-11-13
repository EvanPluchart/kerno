//! Kerno's engine-neutral core: the driver trait, the value and type model,
//! connection settings, SSH tunnels and SQL helpers. No UI dependency.

pub mod aggregate;
pub mod browse;
pub mod config;
pub mod connector;
pub mod dialect;
pub mod driver;
pub mod editable;
pub mod environment;
pub mod error;
pub mod exec;
pub mod export;
pub mod export_file;
pub mod format;
pub mod import;
pub mod maintenance;
pub mod pending;
pub mod plan;
pub mod result;
pub mod schema;
pub mod secrets;
pub mod sort;
pub mod sql;
pub mod ssh;
pub mod storage;
pub mod table_draft;
#[cfg(feature = "test-support")]
pub mod testing;
pub mod tools;

pub use config::{
    ConnectParams, ConnectionConfig, Engine, Secret, Secrets, SshAuth, SshConfig, TlsConfig,
    TlsMode,
};
pub use connector::{Connector, HostKeyStore};
pub use dialect::Dialect;
pub use driver::{AdminView, Driver, Session, TxState};
pub use error::{DbError, Error, Result};
pub use exec::{ExecEvent, ExecSink, Execution, Notice, RowDemand, RowLimit};
pub use plan::{Plan, PlanNode};
pub use result::{BatchBuilder, Column, ColumnOrigin, ResultStore, RowBatch, TypeKind};
