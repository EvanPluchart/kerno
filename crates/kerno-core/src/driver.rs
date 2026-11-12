//! The driver abstraction: what an engine implements to plug into Kerno.
//!
//! A [`Driver`] opens [`Session`]s. A session is one server connection: it
//! runs one statement at a time, can be cancelled from another task, and
//! answers introspection questions with the engine-neutral types of
//! [`crate::schema`].

use std::sync::Arc;

use async_trait::async_trait;

use crate::config::{ConnectParams, Engine};
use crate::dialect::Dialect;
use crate::error::Result;
use crate::exec::ExecSink;
use crate::schema::{
    DatabaseInfo, ExtensionInfo, ObjectRef, SchemaInfo, SchemaObjects, ServerInfo, TableRef,
    TableStructure,
};
use crate::sql::Catalog;

/// Transaction state of a session, as last reported by the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TxState {
    /// No transaction is open: statements commit on their own.
    #[default]
    Idle,
    /// A transaction is open.
    Open,
    /// A transaction is open and failed: only `ROLLBACK` is accepted.
    Failed,
}

/// A table-shaped administrative view every engine can produce. Each is a
/// canned query whose result the app shows in the ordinary results grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AdminView {
    /// Server sessions / processes. Column 0 is the session id accepted by
    /// [`Session::cancel_session`] and [`Session::terminate_session`].
    Sessions,
    /// Users and roles with their attributes and memberships.
    Roles,
    /// Privileges granted on objects.
    Grants,
    /// Databases with their sizes.
    DatabaseSizes,
    /// Tables of the current database with row estimates and sizes.
    TableSizes,
    /// Server configuration settings.
    Settings,
}

/// Opens sessions for one or more engines.
#[async_trait]
pub trait Driver: Send + Sync + 'static {
    /// Engines this driver speaks.
    fn engines(&self) -> &'static [Engine];

    /// Opens a session. The TCP endpoint in `params` may be a tunnel
    /// entrance; TLS names are checked against `params.host`.
    async fn connect(&self, params: &ConnectParams) -> Result<Arc<dyn Session>>;
}

/// One connection to a server.
///
/// Methods take `&self` and serialize internally: a session runs one
/// statement at a time, and a second caller waits. [`Session::cancel`] is
/// the exception and never waits.
#[async_trait]
pub trait Session: Send + Sync + 'static {
    /// Engine of the connected server.
    fn engine(&self) -> Engine;

    /// SQL dialect of the connected server.
    fn dialect(&self) -> &dyn Dialect;

    /// Version of the connected server.
    fn server(&self) -> &ServerInfo;

    /// Database this session is in. On MySQL it follows `USE`.
    fn database(&self) -> Option<String>;

    /// Transaction state after the last statement.
    fn tx_state(&self) -> TxState;

    /// Runs `sql` (one statement, or a script the server accepts as one
    /// request) and writes what it produces to `sink`.
    ///
    /// Contract for implementors:
    /// - values are written in the server's text form, raw bytes for
    ///   [`crate::TypeKind::Binary`] columns;
    /// - never read the next row from the server before
    ///   [`ExecSink::end_row`] returned;
    /// - if the sink reports [`crate::exec::Stopped`], or
    ///   [`ExecSink::stopped`] resolves while waiting for the server, cancel
    ///   the statement on the server and leave the connection ready for the
    ///   next statement before returning [`crate::Error::Cancelled`].
    async fn execute(&self, sql: &str, sink: &mut ExecSink) -> Result<()>;

    /// Asks the server to stop the statement this session is running.
    /// Returns once the request was delivered. The running
    /// [`Session::execute`] then ends as the server decides: usually with
    /// its cancellation error, but some statements simply return early
    /// (MySQL's `SLEEP()` returns 1 when interrupted). Callers that need to
    /// know a statement was cancelled remember that they asked.
    /// Does nothing harmful when no statement is running.
    async fn cancel(&self) -> Result<()>;

    /// Checks the connection is alive with a round trip.
    async fn ping(&self) -> Result<()>;

    /// Closes the connection. The session is unusable afterwards.
    async fn close(&self);

    // Introspection -------------------------------------------------------

    /// Databases on the server.
    async fn databases(&self) -> Result<Vec<DatabaseInfo>>;

    /// Schemas of the connected database. Engines without schemas inside
    /// databases return an empty list (see [`Dialect::has_schemas`]).
    async fn schemas(&self) -> Result<Vec<SchemaInfo>>;

    /// Everything in a schema (PostgreSQL schema or MySQL database).
    async fn objects(&self, schema: &str) -> Result<SchemaObjects>;

    /// Columns, indexes, constraints, foreign keys and triggers of a table
    /// or view.
    async fn table_structure(&self, table: &TableRef) -> Result<TableStructure>;

    /// The `CREATE` statement(s) that would recreate an object.
    async fn ddl(&self, object: &ObjectRef) -> Result<String>;

    /// Installed and available extensions. Empty on engines without them.
    async fn extensions(&self) -> Result<Vec<ExtensionInfo>>;

    /// What SQL completion needs, in one call: every non-system schema
    /// (database, on MySQL) with its relations, their columns and foreign
    /// keys, the routine names, and the search path.
    async fn completion_catalog(&self) -> Result<Catalog>;

    // Administration ------------------------------------------------------

    /// The query behind an administrative view, or `None` when the engine
    /// has no such view.
    fn admin_query(&self, view: AdminView) -> Option<String>;

    /// Cancels the statement another session is running.
    async fn cancel_session(&self, id: &str) -> Result<()>;

    /// Disconnects another session.
    async fn terminate_session(&self, id: &str) -> Result<()>;
}
