//! A MySQL / MariaDB session: one server connection that runs statements
//! and streams their results.
//!
//! Statements run through the text protocol, so every value arrives in the
//! server's own text form (see `types` for the few that are presented
//! differently). Cancelling goes through a second, short-lived connection
//! that sends `KILL QUERY`, the only way the protocol offers.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use kerno_core::config::ConnectParams;
use kerno_core::dialect::Dialect;
use kerno_core::exec::{ExecSink, Stopped};
use kerno_core::schema::{
    DatabaseInfo, ExtensionInfo, ObjectRef, SchemaInfo, SchemaObjects, ServerInfo, TableRef,
    TableStructure,
};
use kerno_core::sql::params::is_number;
use kerno_core::sql::{LexRules, TokenKind, lex};
use kerno_core::{AdminView, Column, Engine, Error, Notice, Result, Session, TxState, TypeKind};
use mysql_async::consts::StatusFlags;
use mysql_async::prelude::Queryable;
use mysql_async::{Conn, QueryResult, Row, TextProtocol, Value};
use tokio::sync::Mutex;

use crate::connect::{self, statement_error, text_rows};
use crate::dialect::MysqlDialect;
use crate::{admin, introspect, types};

/// What the session lock protects.
struct Link {
    /// `None` once the session is closed.
    conn: Option<Conn>,
    /// A request was sent and its answer is not read to the end yet.
    in_flight: bool,
}

impl Link {
    /// The connection, for one exchange with the server; [`Link::end`]
    /// must follow once the answer was read.
    ///
    /// When the caller of an earlier exchange dropped its future midway,
    /// that answer is still on the wire and the next statement would read
    /// it as its own. Such a connection is given up: the session reports
    /// itself closed instead of returning another statement's rows.
    fn begin(&mut self) -> Result<&mut Conn> {
        if self.in_flight {
            self.conn = None;
            self.in_flight = false;
            return Err(Error::Closed(
                "an earlier statement on this session was interrupted before its answer was read"
                    .into(),
            ));
        }
        let conn = self.conn.as_mut().ok_or_else(closed)?;
        self.in_flight = true;
        Ok(conn)
    }

    fn end(&mut self) {
        self.in_flight = false;
    }
}

/// One connection to a MySQL or MariaDB server.
pub struct MysqlSession {
    link: Mutex<Link>,
    /// The route to the server, for the side connections that cancel. It
    /// holds the password in memory, as the connector that opened the
    /// session does.
    params: ConnectParams,
    timeout: Duration,
    /// The server's id of this connection, as `KILL` takes it.
    connection_id: u64,
    engine: Engine,
    server: ServerInfo,
    dialect: MysqlDialect,
    database: std::sync::Mutex<Option<String>>,
    /// The server's "in a transaction" status flag after the last statement.
    in_transaction: AtomicBool,
}

/// Why a statement stopped before its results were all delivered.
enum Abort {
    /// The consumer left; the statement still runs on the server.
    Cancelled,
    /// The consumer left and the statement was killed already.
    Killed,
    /// The server or the connection failed.
    Failed(mysql_async::Error),
}

impl From<Stopped> for Abort {
    fn from(_: Stopped) -> Self {
        Self::Cancelled
    }
}

impl From<mysql_async::Error> for Abort {
    fn from(error: mysql_async::Error) -> Self {
        Self::Failed(error)
    }
}

type TextResult<'a> = QueryResult<'a, 'static, TextProtocol>;

/// `ER_EMPTY_QUERY`: the request held no statement.
const EMPTY_QUERY: u16 = 1065;

impl MysqlSession {
    /// Connects, reads the server's identity and applies the session
    /// settings of `params`.
    pub(crate) async fn connect(params: &ConnectParams) -> Result<Self> {
        let settings = connect::settings(params)?;
        let mut conn = connect::open(params, settings.timeout, params.database.as_deref()).await?;

        // The handshake's connection id is 32 bits wide and its version is
        // not the whole string; ask.
        let identity = "SELECT VERSION(), CONNECTION_ID()";
        let rows = text_rows(&mut conn, identity)
            .await
            .map_err(|error| statement_error(error, identity))?;
        let row = rows
            .into_iter()
            .next()
            .ok_or_else(|| Error::Protocol("the server did not report its version".into()))?;
        let version = row.first().cloned().flatten().unwrap_or_default();
        let connection_id = row
            .get(1)
            .and_then(|id| id.as_deref()?.parse().ok())
            .ok_or_else(|| Error::Protocol("the server did not report a connection id".into()))?;
        let engine = engine_of(&version);

        let session = Self {
            link: Mutex::new(Link {
                conn: None,
                in_flight: false,
            }),
            params: params.clone(),
            timeout: settings.timeout,
            connection_id,
            engine,
            server: ServerInfo {
                version_number: version_number(&version),
                version,
            },
            dialect: MysqlDialect::new(engine),
            database: std::sync::Mutex::new(None),
            in_transaction: AtomicBool::new(false),
        };

        // The literals below must be written for the mode the session is in.
        session.read_session_state(&mut conn).await?;
        let mut setup = Vec::new();
        for (name, value) in &settings.variables {
            let value = if is_number(value) {
                value.trim().to_string()
            } else {
                session.dialect.quote_string(value)
            };
            setup.push(format!("SET SESSION {name} = {value}"));
        }
        if params.read_only {
            // Every transaction of the session, implicit ones included.
            setup.push("SET SESSION TRANSACTION READ ONLY".into());
        }
        for sql in &setup {
            text_rows(&mut conn, sql).await.map_err(|error| {
                Error::Connect(format!(
                    "the server refused a session setting: {}",
                    statement_error(error, sql)
                ))
            })?;
        }
        if !setup.is_empty() {
            session.read_session_state(&mut conn).await?;
        }
        session.note_status(&conn);
        session.link.lock().await.conn = Some(conn);
        Ok(session)
    }

    /// Runs a metadata query and returns its rows as text.
    pub(crate) async fn query(&self, sql: &str) -> Result<Vec<Vec<Option<String>>>> {
        let mut link = self.link.lock().await;
        let rows = text_rows(link.begin()?, sql)
            .await
            .map_err(|error| statement_error(error, sql));
        link.end();
        rows
    }

    /// Reads what statements can change behind the driver's back: the
    /// current database and whether a backslash escapes in string literals.
    async fn read_session_state(&self, conn: &mut Conn) -> Result<()> {
        let sql = "SELECT DATABASE(), @@session.sql_mode";
        let rows = text_rows(conn, sql)
            .await
            .map_err(|error| statement_error(error, sql))?;
        let mut row = rows.into_iter().next().unwrap_or_default().into_iter();
        let database = row.next().flatten();
        let sql_mode = row.next().flatten().unwrap_or_default();
        *self.database.lock().unwrap_or_else(|e| e.into_inner()) = database;
        self.dialect.set_no_backslash_escapes(
            sql_mode
                .split(',')
                .any(|mode| mode.trim().eq_ignore_ascii_case("NO_BACKSLASH_ESCAPES")),
        );
        Ok(())
    }

    /// Records the transaction flag of the server's last status report.
    fn note_status(&self, conn: &Conn) {
        if let Some(ok) = conn.last_ok_packet() {
            self.in_transaction.store(
                ok.status_flags()
                    .contains(StatusFlags::SERVER_STATUS_IN_TRANS),
                Ordering::Release,
            );
        }
    }

    /// Sends `KILL QUERY` for this session's connection over a connection
    /// of its own, which takes the same route (a new tunnel entrance, the
    /// same TLS settings).
    async fn kill_query(&self) -> Result<()> {
        let cancel_error =
            |error: Error| Error::Connect(format!("could not send the cancel request: {error}"));
        let mut side = connect::open(&self.params, self.timeout, None)
            .await
            .map_err(cancel_error)?;
        let sql = format!("KILL QUERY {}", self.connection_id);
        let outcome = side.query_drop(&sql).await;
        if let Err(error) = side.disconnect().await {
            tracing::debug!(%error, "the cancel connection did not close cleanly");
        }
        outcome.map_err(|error| cancel_error(statement_error(error, &sql)))
    }

    /// Stops the statement the consumer walked away from.
    async fn kill_abandoned(&self) {
        if let Err(error) = self.kill_query().await {
            tracing::debug!(%error, "could not cancel an abandoned statement");
        }
    }

    /// The next row of the current result, `None` at its end. Returns
    /// [`Abort::Killed`] when the consumer leaves while the server is
    /// awaited, after killing the statement.
    ///
    /// The read in flight is always awaited to its end: the client library
    /// marks a connection broken when one of its futures is dropped midway.
    async fn next_row(
        &self,
        result: &mut TextResult<'_>,
        sink: &ExecSink,
    ) -> std::result::Result<Option<Row>, Abort> {
        let next = result.next();
        tokio::pin!(next);
        tokio::select! {
            biased;
            row = &mut next => Ok(row?),
            () = sink.stopped() => {
                self.kill_abandoned().await;
                let _ = next.await;
                Err(Abort::Killed)
            }
        }
    }

    /// Writes every result of a request to the sink.
    async fn stream(
        &self,
        result: &mut TextResult<'_>,
        sink: &mut ExecSink,
    ) -> std::result::Result<(), Abort> {
        loop {
            let Some(columns) = result.columns() else {
                // Nothing is pending: the request is over, unless a later
                // statement of it failed, which this reports.
                self.next_row(result, sink).await?;
                return Ok(());
            };
            if columns.is_empty() {
                // A statement without rows.
                sink.complete(Some(result.affected_rows())).await?;
                // Steps to the next result of the request, if any.
                self.next_row(result, sink).await?;
                continue;
            }

            let mut described = Vec::with_capacity(columns.len());
            // The width of each BIT column, 0 for the others.
            let mut bit_widths = Vec::with_capacity(columns.len());
            for column in columns.iter() {
                let (type_name, kind) = types::describe(
                    column.column_type(),
                    column.flags(),
                    column.character_set(),
                    column.column_length(),
                );
                bit_widths.push(if kind == TypeKind::Bit {
                    column.column_length().max(1)
                } else {
                    0
                });
                described.push(Column::new(column.name_str(), type_name, kind));
            }
            sink.columns(described).await?;

            // ponytail: the client library reads the header of the next
            // result together with the end of this one, so in a request
            // with several results a result is reported complete when the
            // next one starts. Needs a lower-level read loop to fix.
            let mut rows = 0u64;
            while let Some(row) = self.next_row(result, sink).await? {
                let builder = sink.row();
                for (index, value) in row.unwrap().into_iter().enumerate() {
                    match value {
                        Value::NULL => builder.push_null(),
                        Value::Bytes(bytes) => match bit_widths.get(index) {
                            Some(&width) if width > 0 => {
                                builder.push_text(&types::bit_string(&bytes, width));
                            }
                            _ => builder.push_bytes(&bytes),
                        },
                        // The text protocol sends nothing else.
                        other => builder.push_text(&other.as_sql(true)),
                    }
                }
                sink.end_row().await?;
                rows += 1;
            }
            sink.complete(Some(rows)).await?;
        }
    }

    /// Runs one request, writing to the sink. When the consumer leaves, the
    /// statement is killed and what the server had already sent is read
    /// off, so the connection is ready for the next statement.
    async fn run(
        &self,
        conn: &mut Conn,
        sql: &str,
        sink: &mut ExecSink,
    ) -> std::result::Result<(), Abort> {
        let mut result = {
            let query = conn.query_iter(sql);
            tokio::pin!(query);
            tokio::select! {
                biased;
                result = &mut query => match result {
                    Ok(result) => result,
                    // Nothing to run is not an error (PostgreSQL does not
                    // make it one either): a statement that changed nothing.
                    Err(mysql_async::Error::Server(error)) if error.code == EMPTY_QUERY => {
                        sink.complete(Some(0)).await?;
                        return Ok(());
                    }
                    Err(error) => return Err(Abort::Failed(error)),
                },
                () = sink.stopped() => {
                    self.kill_abandoned().await;
                    if let Ok(result) = query.await {
                        let _ = result.drop_result().await;
                    }
                    return Err(Abort::Killed);
                }
            }
        };
        match self.stream(&mut result, sink).await {
            Ok(()) => Ok(()),
            Err(Abort::Failed(error)) => Err(Abort::Failed(error)),
            Err(abort) => {
                if matches!(abort, Abort::Cancelled) {
                    self.kill_abandoned().await;
                }
                // Ends at once with the server's "interrupted" error, or
                // after the rows that were already on their way.
                let _ = result.drop_result().await;
                Err(Abort::Killed)
            }
        }
    }

    /// Runs one request and brings the session's view of the server (open
    /// transaction, current database, `sql_mode`) up to date.
    async fn execute_on(&self, conn: &mut Conn, sql: &str, sink: &mut ExecSink) -> Result<()> {
        let outcome = match self.run(conn, sql, sink).await {
            Ok(()) => {
                self.forward_warnings(conn, sink).await;
                Ok(())
            }
            Err(Abort::Cancelled | Abort::Killed) => Err(Error::Cancelled),
            Err(Abort::Failed(error)) => Err(statement_error(error, sql)),
        };

        if matches!(outcome, Err(Error::Closed(_))) {
            self.in_transaction.store(false, Ordering::Release);
            return outcome;
        }
        // An error packet carries no status; a ping's answer does. It also
        // tells a statement that failed from a connection that was killed
        // under it, which some servers report with the same error.
        if outcome.is_err() && conn.ping().await.is_err() {
            self.in_transaction.store(false, Ordering::Release);
            return match outcome {
                Err(Error::Db(error)) => Err(Error::Closed(error.to_string())),
                other => other,
            };
        }
        if touches_session_state(sql, self.dialect.lex_rules())
            && let Err(error) = self.read_session_state(conn).await
        {
            tracing::debug!(%error, "could not read the session state after a statement");
        }
        self.note_status(conn);
        outcome
    }

    /// Forwards the warnings the last statement left, if any. Only the last
    /// statement of a request can be asked about.
    async fn forward_warnings(&self, conn: &mut Conn, sink: &mut ExecSink) {
        if conn.get_warnings() == 0 {
            return;
        }
        let warnings = match text_rows(conn, "SHOW WARNINGS").await {
            Ok(warnings) => warnings,
            Err(error) => {
                tracing::debug!(%error, "could not read the warnings of a statement");
                return;
            }
        };
        for warning in warnings {
            let mut fields = warning.into_iter();
            let notice = Notice {
                severity: fields.next().flatten().unwrap_or_else(|| "Warning".into()),
                code: fields.next().flatten(),
                message: fields.next().flatten().unwrap_or_default(),
                detail: None,
            };
            if sink.notice(notice).await.is_err() {
                return;
            }
        }
    }
}

fn closed() -> Error {
    Error::Closed("the session was closed".into())
}

/// MariaDB names itself in its version string; nothing else does.
fn engine_of(version: &str) -> Engine {
    if version.to_ascii_lowercase().contains("mariadb") {
        Engine::MariaDb
    } else {
        Engine::MySql
    }
}

/// `(major, minor, patch)` of a version string such as `8.4.13` or
/// `11.8.9-MariaDB-ubu2404`.
fn version_number(version: &str) -> (u32, u32, u32) {
    // Old MariaDB servers prefix their real version for replication's sake.
    let version = version.strip_prefix("5.5.5-").unwrap_or(version);
    let mut parts = version.split('.').map(|part| {
        let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().unwrap_or(0)
    });
    let mut next = || parts.next().unwrap_or(0);
    (next(), next(), next())
}

/// Whether a request may change the current database or `sql_mode`: one of
/// its statements starts with `USE`, `SET` or `DROP` (dropping the current
/// database leaves the session in none). Stored routines cannot change
/// either for their caller.
fn touches_session_state(sql: &str, rules: LexRules) -> bool {
    let mut at_start = true;
    for token in lex(sql, rules) {
        match token.kind {
            kind if kind.is_trivia() => {}
            TokenKind::Semicolon => at_start = true,
            TokenKind::Word if at_start => {
                let word = sql[token.range].to_ascii_uppercase();
                if matches!(word.as_str(), "USE" | "SET" | "DROP") {
                    return true;
                }
                at_start = false;
            }
            _ => at_start = false,
        }
    }
    false
}

#[async_trait]
impl Session for MysqlSession {
    fn engine(&self) -> Engine {
        self.engine
    }

    fn dialect(&self) -> &dyn Dialect {
        &self.dialect
    }

    fn server(&self) -> &ServerInfo {
        &self.server
    }

    fn database(&self) -> Option<String> {
        self.database
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// MySQL has no failed-transaction state: after an error the statement
    /// is undone and the transaction stays open, so this is `Idle` or `Open`.
    fn tx_state(&self) -> TxState {
        if self.in_transaction.load(Ordering::Acquire) {
            TxState::Open
        } else {
            TxState::Idle
        }
    }

    async fn execute(&self, sql: &str, sink: &mut ExecSink) -> Result<()> {
        let mut link = self.link.lock().await;
        let outcome = self.execute_on(link.begin()?, sql, sink).await;
        link.end();
        outcome
    }

    async fn cancel(&self) -> Result<()> {
        self.kill_query().await
    }

    async fn ping(&self) -> Result<()> {
        let mut link = self.link.lock().await;
        let outcome = link
            .begin()?
            .ping()
            .await
            .map_err(|error| statement_error(error, ""));
        link.end();
        outcome
    }

    async fn close(&self) {
        let conn = match self.link.try_lock() {
            Ok(mut link) => link.conn.take(),
            Err(_) => {
                // A statement holds the connection: stop it first.
                self.kill_abandoned().await;
                self.link.lock().await.conn.take()
            }
        };
        if let Some(conn) = conn
            && let Err(error) = conn.disconnect().await
        {
            tracing::debug!(%error, "the connection did not close cleanly");
        }
    }

    async fn databases(&self) -> Result<Vec<DatabaseInfo>> {
        introspect::databases(self).await
    }

    /// Always empty: a MySQL database contains tables directly.
    async fn schemas(&self) -> Result<Vec<SchemaInfo>> {
        Ok(Vec::new())
    }

    async fn objects(&self, schema: &str) -> Result<SchemaObjects> {
        introspect::objects(self, schema).await
    }

    async fn table_structure(&self, table: &TableRef) -> Result<TableStructure> {
        introspect::table_structure(self, table).await
    }

    async fn ddl(&self, object: &ObjectRef) -> Result<String> {
        crate::ddl::ddl(self, object).await
    }

    /// Always empty: neither engine has extensions in PostgreSQL's sense.
    async fn extensions(&self) -> Result<Vec<ExtensionInfo>> {
        Ok(Vec::new())
    }

    fn admin_query(&self, view: AdminView) -> Option<String> {
        Some(admin::query(view, self.engine, self.server.version_number))
    }

    async fn cancel_session(&self, id: &str) -> Result<()> {
        admin::kill(self, "QUERY", id).await
    }

    async fn terminate_session(&self, id: &str) -> Result<()> {
        admin::kill(self, "CONNECTION", id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_engine_is_read_from_the_version_string() {
        assert_eq!(engine_of("8.4.13"), Engine::MySql);
        assert_eq!(engine_of("8.0.36-28 Percona Server"), Engine::MySql);
        assert_eq!(engine_of("11.8.9-MariaDB-ubu2404"), Engine::MariaDb);
        assert_eq!(engine_of("5.5.5-10.6.4-MariaDB-log"), Engine::MariaDb);
        assert_eq!(engine_of(""), Engine::MySql);
    }

    #[test]
    fn version_numbers() {
        assert_eq!(version_number("8.4.13"), (8, 4, 13));
        assert_eq!(version_number("11.8.9-MariaDB-ubu2404"), (11, 8, 9));
        assert_eq!(version_number("5.5.5-10.6.4-MariaDB-log"), (10, 6, 4));
        assert_eq!(version_number("8.0.36-0ubuntu0.22.04.1"), (8, 0, 36));
        assert_eq!(version_number("10.11"), (10, 11, 0));
        assert_eq!(version_number(""), (0, 0, 0));
        assert_eq!(version_number("garbage"), (0, 0, 0));
    }

    #[test]
    fn statements_that_may_change_the_session_state() {
        let rules = LexRules::MYSQL;
        for sql in [
            "USE kerno_other",
            "  use `x`",
            "-- c\n/* c */ USE x",
            "SELECT 1; USE x",
            "SELECT 1;\n# c\nuse x;",
            "SET sql_mode = ''",
            "set names utf8mb4",
            "DROP DATABASE kerno_test_x",
            "drop schema x",
        ] {
            assert!(touches_session_state(sql, rules), "{sql}");
        }
        for sql in [
            "SELECT 'USE x'",
            "SELECT `use` FROM t",
            "SELECT 1; SELECT 'a;USE x'",
            "UPDATE t SET a = 1",
            "INSERT INTO t VALUES ('x; DROP TABLE t')",
            "CALL use_something()",
            "CREATE DATABASE x",
            "",
            ";;",
        ] {
            assert!(!touches_session_state(sql, rules), "{sql}");
        }
    }
}
