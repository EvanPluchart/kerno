//! A PostgreSQL session: one server connection that runs statements and
//! streams their results.
//!
//! Statements run through the simple query protocol, so every value arrives
//! in the server's own text form, for every type including extension types.
//! That protocol does not report column types, so statements that return
//! rows are described first (`Parse`/`Describe`) to learn them.

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use async_trait::async_trait;
use futures_util::StreamExt;
use kerno_core::config::ConnectParams;
use kerno_core::dialect::Dialect;
use kerno_core::exec::ExecSink;
use kerno_core::schema::{
    Change, DatabaseInfo, Dependent, ExtensionInfo, ObjectRef, SchemaInfo, SchemaObjects,
    ServerInfo, TableRef, TableStructure,
};
use kerno_core::sql::{Catalog, TokenKind, lex};
use kerno_core::{
    AdminView, Column, ColumnOrigin, Engine, Error, Result, Session, TxState, TypeKind,
};
use tokio::sync::{Mutex, mpsc};
use tokio_postgres::{Client, SimpleQueryMessage, SimpleQueryRow};

use crate::connect::{self, CancelRoute, ConnectionEvent};
use crate::dialect::PgDialect;
use crate::{admin, introspect, types};

/// What the session lock protects: the client and the connection's events.
struct Link {
    /// `None` once the session is closed.
    client: Option<Client>,
    events: mpsc::UnboundedReceiver<ConnectionEvent>,
}

/// One connection to a PostgreSQL server.
pub struct PgSession {
    link: Mutex<Link>,
    cancel: CancelRoute,
    tx_state: AtomicU8,
    /// How many times [`Session::cancel`] was called. A cancel that comes
    /// after a statement was asked for and before it is sent (while it waits
    /// for its turn on the session, or is being described) is for that
    /// statement: it is not sent at all (the server, idle then, would
    /// ignore the cancel request).
    cancels: AtomicU64,
    server: ServerInfo,
    database: String,
    /// `current_schema()`, as last read (C6).
    schema: std::sync::Mutex<Option<String>>,
    dialect: PgDialect,
}

const TX_IDLE: u8 = 0;
const TX_OPEN: u8 = 1;
const TX_FAILED: u8 = 2;

/// SQLSTATE: current transaction is aborted.
const IN_FAILED_TRANSACTION: &str = "25P02";

impl PgSession {
    /// Connects and reads the server's identity.
    pub(crate) async fn connect(params: &ConnectParams) -> Result<Self> {
        let (client, events, cancel) = connect::open(params).await?;
        let rows = simple_rows(
            &client,
            "SELECT current_setting('server_version_num'), version(), current_database(), \
             current_schema()",
        )
        .await?;
        let row = rows
            .first()
            .ok_or_else(|| Error::Protocol("the server did not report its version".into()))?;
        let number: u32 = row.get(0).and_then(|v| v.parse().ok()).unwrap_or(0);
        let server = ServerInfo {
            version: row.get(1).unwrap_or_default().to_string(),
            // server_version_num is MMmmpp up to 9.6 and MM00mm from 10 on.
            version_number: if number >= 100_000 {
                (number / 10_000, number % 10_000, 0)
            } else {
                (number / 10_000, number / 100 % 100, number % 100)
            },
        };
        let database = row.get(2).unwrap_or_default().to_string();
        let schema = row.get(3).map(str::to_string);
        Ok(Self {
            link: Mutex::new(Link {
                client: Some(client),
                events,
            }),
            cancel,
            tx_state: AtomicU8::new(TX_IDLE),
            cancels: AtomicU64::new(0),
            server,
            database,
            schema: std::sync::Mutex::new(schema),
            dialect: PgDialect,
        })
    }

    /// Runs a metadata query and returns its rows as text.
    pub(crate) async fn query(&self, sql: &str) -> Result<Vec<SimpleQueryRow>> {
        let link = self.link.lock().await;
        let client = link.client.as_ref().ok_or_else(closed)?;
        simple_rows(client, sql).await
    }

    fn set_tx_state(&self, state: TxState) {
        self.tx_state.store(
            match state {
                TxState::Idle => TX_IDLE,
                TxState::Open => TX_OPEN,
                TxState::Failed => TX_FAILED,
            },
            Ordering::Release,
        );
    }

    /// Runs one statement, writing to the sink. Returns `Err(Cancelled)`
    /// when the sink reported that the consumer left, or when the session
    /// was cancelled since `cancels` was read (the count of cancels when
    /// the statement was asked for): `sent` then stays false, the
    /// statement never reached the server.
    async fn run(
        &self,
        link: &mut Link,
        sql: &str,
        sink: &mut ExecSink,
        cancels: u64,
        sent: &mut bool,
    ) -> Result<()> {
        let Link { client, events } = link;
        let client = client.as_ref().ok_or_else(closed)?;

        // Leftovers from before this statement: stale notices are dropped,
        // a lost connection is reported.
        while let Ok(event) = events.try_recv() {
            if let ConnectionEvent::Closed(why) = event {
                return Err(Error::Closed(why));
            }
        }

        let mut described = None;
        if self.tx_state() != TxState::Failed && may_return_rows(sql) {
            // A failure here is not reported: running the statement gives
            // the authoritative error.
            if let Ok(statement) = client.prepare(sql).await {
                described = Some(
                    statement
                        .columns()
                        .iter()
                        .map(|column| {
                            let (name, kind) = types::describe(column.type_());
                            // The table column it shows, when it shows one.
                            let origin = column
                                .table_oid()
                                .zip(column.column_id())
                                .map(|(table, column)| ColumnOrigin::Oid { table, column });
                            (name, kind, origin)
                        })
                        .collect::<Vec<_>>(),
                );
            }
        }

        // A cancel that came while this statement waited for the session
        // (behind a result being let go, say) or was described would find
        // the server idle and be lost: the statement is not sent.
        if self.cancels.load(Ordering::Acquire) != cancels {
            return Err(Error::Cancelled);
        }
        *sent = true;
        let stream = client
            .simple_query_raw(sql)
            .await
            .map_err(connect::statement_error)?;
        tokio::pin!(stream);

        let mut binary: Vec<bool> = Vec::new();
        loop {
            tokio::select! {
                biased;
                () = sink.stopped() => return Err(Error::Cancelled),
                Some(event) = events.recv() => match event {
                    ConnectionEvent::Notice(notice) => {
                        sink.notice(notice).await?;
                    }
                    ConnectionEvent::Closed(why) => return Err(Error::Closed(why)),
                },
                message = stream.next() => match message {
                    None => break,
                    Some(Err(error)) => return Err(connect::statement_error(error)),
                    Some(Ok(SimpleQueryMessage::RowDescription(names))) => {
                        let types = described.take().filter(|types| types.len() == names.len());
                        let columns: Vec<Column> = names
                            .iter()
                            .enumerate()
                            .map(|(index, name)| match &types {
                                Some(types) => Column::new(name.name(), &types[index].0, types[index].1)
                                    .with_origin(types[index].2.clone()),
                                None => Column::new(name.name(), "", TypeKind::Other),
                            })
                            .collect();
                        binary = columns.iter().map(|c| c.kind == TypeKind::Binary).collect();
                        sink.columns(columns).await?;
                    }
                    Some(Ok(SimpleQueryMessage::Row(row))) => {
                        let builder = sink.row();
                        for index in 0..row.len() {
                            match row.get(index) {
                                None => builder.push_null(),
                                Some(text) if binary.get(index).copied().unwrap_or(false) => {
                                    builder.push_bytes(&decode_bytea(text));
                                }
                                Some(text) => builder.push_text(text),
                            }
                        }
                        sink.end_row().await?;
                    }
                    Some(Ok(SimpleQueryMessage::CommandComplete(rows))) => {
                        sink.complete(Some(rows)).await?;
                    }
                    Some(Ok(_)) => {}
                },
            }
        }

        // Notices raised by the last statement may still be queued.
        while let Ok(event) = events.try_recv() {
            match event {
                ConnectionEvent::Notice(notice) => {
                    sink.notice(notice).await?;
                }
                ConnectionEvent::Closed(why) => return Err(Error::Closed(why)),
            }
        }
        Ok(())
    }
}

fn closed() -> Error {
    Error::Closed("the session was closed".into())
}

async fn simple_rows(client: &Client, sql: &str) -> Result<Vec<SimpleQueryRow>> {
    let messages = client
        .simple_query(sql)
        .await
        .map_err(connect::statement_error)?;
    Ok(messages
        .into_iter()
        .filter_map(|message| match message {
            SimpleQueryMessage::Row(row) => Some(row),
            _ => None,
        })
        .collect())
}

/// The first words of a statement, upper-cased, skipping comments.
fn leading_words(sql: &str, count: usize) -> Vec<String> {
    lex(sql, kerno_core::sql::LexRules::POSTGRES)
        .into_iter()
        .filter(|token| !token.kind.is_trivia())
        .take(count)
        .map(|token| match token.kind {
            TokenKind::Word => sql[token.range].to_ascii_uppercase(),
            _ => String::new(),
        })
        .collect()
}

/// Whether a statement can return rows, and so is worth describing first.
/// A wrong "no" only costs the column types (shown as plain text).
fn may_return_rows(sql: &str) -> bool {
    let rules = kerno_core::sql::LexRules::POSTGRES;
    let mut words = lex(sql, rules)
        .into_iter()
        .filter(|token| token.kind == TokenKind::Word)
        .map(|token| sql[token.range].to_ascii_uppercase());
    match words.next().as_deref() {
        Some(
            "SELECT" | "WITH" | "VALUES" | "TABLE" | "SHOW" | "EXPLAIN" | "FETCH" | "EXECUTE"
            | "CALL",
        ) => true,
        Some("INSERT" | "UPDATE" | "DELETE" | "MERGE") => words.any(|word| word == "RETURNING"),
        _ => false,
    }
}

/// Whether a request may change the current schema (C6), so that it is read
/// again after it: one of its statements starts with `SET` (search path,
/// schema, role: `$user` follows the role) or `RESET`, `DISCARD`, `DO` or
/// `CALL` (whose body may set it), or ends a transaction (`COMMIT`,
/// `ROLLBACK`, `END`, `ABORT`, `PREPARE TRANSACTION`: a `SET` made inside
/// the block, `LOCAL` or not, is undone by a rollback, a `SET LOCAL` by
/// any end); or it calls `set_config` anywhere. A function called from a
/// `SELECT` that sets it is not seen.
fn touches_session_state(sql: &str) -> bool {
    let mut at_start = true;
    for token in lex(sql, kerno_core::sql::LexRules::POSTGRES) {
        match token.kind {
            kind if kind.is_trivia() => {}
            TokenKind::Semicolon => at_start = true,
            TokenKind::Word => {
                let word = sql[token.range].to_ascii_uppercase();
                let starts = matches!(
                    word.as_str(),
                    "SET"
                        | "RESET"
                        | "DISCARD"
                        | "DO"
                        | "CALL"
                        | "COMMIT"
                        | "ROLLBACK"
                        | "END"
                        | "ABORT"
                        | "PREPARE"
                );
                if (at_start && starts) || word == "SET_CONFIG" {
                    return true;
                }
                at_start = false;
            }
            _ => at_start = false,
        }
    }
    false
}

/// How a statement moves the transaction state when it succeeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TxEffect {
    /// Opens a transaction block.
    Begin,
    /// Ends the transaction block.
    End,
    /// `ROLLBACK TO SAVEPOINT`: the block is usable again.
    RollbackTo,
    /// No effect on the block.
    None,
}

fn tx_effect(sql: &str) -> TxEffect {
    let words = leading_words(sql, 5);
    let word = |index: usize| words.get(index).map_or("", String::as_str);
    // `COMMIT AND CHAIN` / `ROLLBACK AND CHAIN` start a new block at once.
    let chains = words
        .windows(2)
        .any(|pair| pair[0] == "AND" && pair[1] == "CHAIN");
    match word(0) {
        "BEGIN" => TxEffect::Begin,
        "START" if word(1) == "TRANSACTION" => TxEffect::Begin,
        "COMMIT" | "END" | "ABORT" if chains => TxEffect::Begin,
        "ROLLBACK" if word(1) == "TO" => TxEffect::RollbackTo,
        "ROLLBACK" if chains => TxEffect::Begin,
        "COMMIT" | "END" | "ABORT" | "ROLLBACK" => TxEffect::End,
        "PREPARE" if word(1) == "TRANSACTION" => TxEffect::End,
        _ => TxEffect::None,
    }
}

/// The transaction state after a statement with `effect` ran from `before`.
fn next_tx_state(before: TxState, effect: TxEffect, outcome: &Result<()>) -> TxState {
    match outcome {
        Ok(()) => match effect {
            TxEffect::Begin | TxEffect::RollbackTo => TxState::Open,
            TxEffect::End => TxState::Idle,
            TxEffect::None => before,
        },
        Err(Error::Closed(_)) => TxState::Idle,
        Err(error) => {
            let aborted = error
                .db()
                .is_some_and(|db| db.code.as_deref() == Some(IN_FAILED_TRANSACTION));
            match (before, effect) {
                _ if aborted => TxState::Failed,
                // A failed COMMIT (a deferred constraint, say) still ends the block.
                (_, TxEffect::End) => TxState::Idle,
                (TxState::Idle, _) => TxState::Idle,
                // Any error inside a block aborts it until ROLLBACK.
                (TxState::Open | TxState::Failed, _) => TxState::Failed,
            }
        }
    }
}

/// Decodes the text form of a `bytea` value: `\x` followed by hex digits
/// (`bytea_output = hex`, the default), or the older escape format.
fn decode_bytea(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    if let Some(hex) = bytes.strip_prefix(b"\\x") {
        let digit = |byte: u8| (byte as char).to_digit(16).unwrap_or(0) as u8;
        return hex
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| digit(pair[0]) << 4 | digit(pair[1]))
            .collect();
    }
    // Escape format: printable bytes as themselves, `\\` for a backslash,
    // `\ooo` (three octal digits) for the rest.
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            if bytes.get(index + 1) == Some(&b'\\') {
                out.push(b'\\');
                index += 2;
                continue;
            }
            if let Some(octal) = bytes.get(index + 1..index + 4)
                && let Some(value) = std::str::from_utf8(octal)
                    .ok()
                    .and_then(|digits| u8::from_str_radix(digits, 8).ok())
            {
                out.push(value);
                index += 4;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    out
}

#[async_trait]
impl Session for PgSession {
    fn engine(&self) -> Engine {
        Engine::Postgres
    }

    fn dialect(&self) -> &dyn Dialect {
        &self.dialect
    }

    fn server(&self) -> &ServerInfo {
        &self.server
    }

    fn database(&self) -> Option<String> {
        Some(self.database.clone())
    }

    fn schema(&self) -> Option<String> {
        self.schema
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn tx_state(&self) -> TxState {
        match self.tx_state.load(Ordering::Acquire) {
            TX_OPEN => TxState::Open,
            TX_FAILED => TxState::Failed,
            _ => TxState::Idle,
        }
    }

    async fn execute(&self, sql: &str, sink: &mut ExecSink) -> Result<()> {
        let cancels = self.cancels.load(Ordering::Acquire);
        let mut link = self.link.lock().await;
        let effect = tx_effect(sql);
        let mut sent = false;
        let outcome = self.run(&mut link, sql, sink, cancels, &mut sent).await;
        if matches!(outcome, Err(Error::Cancelled)) {
            // Cancelled before it was sent: nothing to stop.
            if !sent {
                return outcome;
            }
            // The consumer left. The statement's remaining output is
            // discarded by the connection, in order, before the next
            // statement's answer is read.
            if self.tx_state() == TxState::Open {
                // Inside a transaction block a cancel request would abort
                // the block, and with it what the user did in it: letting a
                // paged result go must not cost that. The server is left to
                // finish the statement; the next one waits for it. (An
                // explicit `cancel` still stops it, block and all.)
                return outcome;
            }
            // Outside a block, stop the server too.
            if let Err(error) = self.cancel.cancel().await {
                tracing::debug!(%error, "could not cancel an abandoned statement");
            }
            return outcome;
        }
        self.set_tx_state(next_tx_state(self.tx_state(), effect, &outcome));
        // A statement let go has returned above: it may still be running on
        // the server, and a read would wait behind it.
        if !matches!(outcome, Err(Error::Closed(_)))
            && touches_session_state(sql)
            && let Some(client) = link.client.as_ref()
        {
            // In a failed block the read fails too: the last value stays.
            match simple_rows(client, "SELECT current_schema()").await {
                Ok(rows) => {
                    *self.schema.lock().unwrap_or_else(|e| e.into_inner()) =
                        rows.first().and_then(|row| row.get(0)).map(str::to_string);
                }
                Err(error) => tracing::debug!(%error, "could not read the current schema"),
            }
        }
        outcome
    }

    async fn cancel(&self) -> Result<()> {
        // First: a statement about to be sent sees it.
        self.cancels.fetch_add(1, Ordering::AcqRel);
        self.cancel.cancel().await
    }

    async fn ping(&self) -> Result<()> {
        self.query("SELECT 1").await.map(|_| ())
    }

    async fn close(&self) {
        // Stop whatever is running so the lock is released promptly.
        let _ = self.cancel.cancel().await;
        // Dropping the client sends Terminate and ends the connection task.
        self.link.lock().await.client = None;
    }

    async fn databases(&self) -> Result<Vec<DatabaseInfo>> {
        introspect::databases(self).await
    }

    async fn schemas(&self) -> Result<Vec<SchemaInfo>> {
        introspect::schemas(self).await
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

    async fn extensions(&self) -> Result<Vec<ExtensionInfo>> {
        introspect::extensions(self).await
    }

    async fn completion_catalog(&self) -> Result<Catalog> {
        introspect::completion_catalog(self).await
    }

    async fn dependents(&self, change: &Change) -> Result<Vec<Dependent>> {
        introspect::dependents(self, change).await
    }

    fn admin_query(&self, view: AdminView) -> Option<String> {
        Some(admin::query(view).to_string())
    }

    async fn cancel_session(&self, id: &str) -> Result<()> {
        admin::signal(self, self.dialect.cancel_session_sql(id)?, id).await
    }

    async fn terminate_session(&self, id: &str) -> Result<()> {
        admin::signal(self, self.dialect.terminate_session_sql(id)?, id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kerno_core::DbError;

    #[test]
    fn bytea_hex_and_escape_formats() {
        assert_eq!(decode_bytea("\\x"), Vec::<u8>::new());
        assert_eq!(
            decode_bytea("\\xdeadBEEF00ff"),
            vec![0xde, 0xad, 0xbe, 0xef, 0x00, 0xff]
        );
        // bytea_output = escape
        assert_eq!(decode_bytea("abc"), b"abc".to_vec());
        assert_eq!(decode_bytea("a\\\\b"), b"a\\b".to_vec());
        assert_eq!(decode_bytea("\\000\\377x\\012"), vec![0, 255, b'x', 10]);
        assert_eq!(decode_bytea(""), Vec::<u8>::new());
    }

    #[test]
    fn statements_worth_describing() {
        for sql in [
            "select 1",
            "  -- c\n WITH x AS (select 1) select * from x",
            "VALUES (1)",
            "table t",
            "show all",
            "explain select 1",
            "insert into t values (1) returning id",
            "/* c */ update t set a = 1 RETURNING *",
            "call p()",
        ] {
            assert!(may_return_rows(sql), "{sql}");
        }
        for sql in [
            "insert into t values ('returning')",
            "update t set a = 1",
            "create table t (returning_id int)",
            "begin",
            "",
            "vacuum",
        ] {
            assert!(!may_return_rows(sql), "{sql}");
        }
    }

    #[test]
    fn transaction_effects_of_statements() {
        use TxEffect::*;
        for (sql, expected) in [
            ("BEGIN", Begin),
            ("begin isolation level serializable", Begin),
            ("-- c\nstart transaction read only", Begin),
            ("COMMIT", End),
            ("end", End),
            ("ROLLBACK", End),
            ("abort", End),
            ("rollback to savepoint s", RollbackTo),
            ("ROLLBACK TO s", RollbackTo),
            ("commit and chain", Begin),
            ("rollback and chain", Begin),
            ("commit and no chain", End),
            ("prepare transaction 'x'", End),
            ("prepare p as select 1", None),
            ("savepoint s", None),
            ("select 'begin'", None),
            ("start_something()", None),
            ("", None),
        ] {
            assert_eq!(tx_effect(sql), expected, "{sql}");
        }
    }

    /// C6: the statements after which the current schema is read again.
    #[test]
    fn statements_that_may_change_the_current_schema() {
        for sql in [
            "SET search_path TO other",
            "set local search_path = a, b",
            "SET SESSION search_path TO DEFAULT",
            "set schema 'other'",
            "SET ROLE someone",
            "RESET search_path",
            "reset all",
            "DISCARD ALL",
            "-- c\n/* c */ set search_path to x",
            "SELECT 1; SET search_path TO x",
            "COMMIT",
            "rollback",
            "ROLLBACK TO SAVEPOINT s",
            "end",
            "abort",
            "PREPARE TRANSACTION 'x'",
            "DO $$ BEGIN PERFORM 1; END $$",
            "CALL p()",
            "SELECT set_config('search_path', 'x', false)",
            "select 1 from t where pg_catalog.set_config('a.b', '1', true) <> ''",
        ] {
            assert!(touches_session_state(sql), "{sql}");
        }
        for sql in [
            "SELECT 'SET search_path TO x'",
            "SELECT \"set\" FROM t",
            "UPDATE t SET a = 1",
            "INSERT INTO t VALUES ('x; SET search_path TO y')",
            "ALTER ROLE r SET search_path TO x",
            "CREATE FUNCTION f() RETURNS int SET search_path = x AS 'select 1' LANGUAGE sql",
            "BEGIN",
            "SELECT 'set_config'",
            "",
            ";;",
        ] {
            assert!(!touches_session_state(sql), "{sql}");
        }
    }

    #[test]
    fn transaction_state_transitions() {
        use TxEffect as E;
        use TxState::*;
        let ok = Ok(());
        let failed: Result<()> = Err(DbError::message("boom").into());
        let aborted: Result<()> = Err(DbError {
            code: Some(IN_FAILED_TRANSACTION.into()),
            ..DbError::message("aborted")
        }
        .into());

        assert_eq!(next_tx_state(Idle, E::Begin, &ok), Open);
        assert_eq!(next_tx_state(Open, E::None, &ok), Open);
        assert_eq!(next_tx_state(Open, E::End, &ok), Idle);
        assert_eq!(next_tx_state(Idle, E::None, &ok), Idle);
        // An error outside a block changes nothing; inside, it aborts the block.
        assert_eq!(next_tx_state(Idle, E::None, &failed), Idle);
        assert_eq!(next_tx_state(Open, E::None, &failed), Failed);
        assert_eq!(next_tx_state(Failed, E::None, &aborted), Failed);
        // Recovery.
        assert_eq!(next_tx_state(Failed, E::End, &ok), Idle);
        assert_eq!(next_tx_state(Failed, E::RollbackTo, &ok), Open);
        // A COMMIT that fails still ends the block.
        assert_eq!(next_tx_state(Open, E::End, &failed), Idle);
        // A BEGIN that fails outside a block leaves it outside.
        assert_eq!(next_tx_state(Idle, E::Begin, &failed), Idle);
        // The server says the block is aborted: believe it.
        assert_eq!(next_tx_state(Idle, E::None, &aborted), Failed);
        assert_eq!(
            next_tx_state(Open, E::None, &Err(Error::Closed("gone".into()))),
            Idle
        );
    }
}
