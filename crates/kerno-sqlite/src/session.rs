//! A SQLite session: one connection to the file, owned by a thread of its
//! own (D-102). rusqlite is blocking and its connection is not `Sync`, so
//! every call is a job sent to that thread, never run on a Tokio worker or
//! the UI thread. A statement's rows go to the sink from that thread, which
//! waits there when the consumer has not asked for more: nothing is read
//! ahead of demand, as the sink's contract asks.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use async_trait::async_trait;
use kerno_core::config::ConnectParams;
use kerno_core::dialect::{Dialect, hex};
use kerno_core::exec::{self, ExecSink, RowLimit};
use kerno_core::result::BatchBuilder;
use kerno_core::schema::{
    DatabaseInfo, ExtensionInfo, ObjectRef, SchemaInfo, SchemaObjects, ServerInfo, TableRef,
    TableStructure,
};
use kerno_core::sql::Catalog;
use kerno_core::{
    AdminView, Column, ColumnOrigin, DbError, Engine, Error, Result, Session, TxState, TypeKind,
};
use rusqlite::fallible_iterator::FallibleIterator;
use rusqlite::types::ValueRef;
use rusqlite::{Batch, Connection, Row, Statement};
use tokio::runtime::Handle;

use crate::connect::{self, statement_error};
use crate::dialect::{MAIN, SqliteDialect};
use crate::introspect;
use crate::types::{column_kind, column_type_name, real_text};

/// Work for the thread that owns the connection.
type Job = Box<dyn FnOnce(&mut Connection) + Send>;

/// One connection to a SQLite database file.
pub struct SqliteSession {
    /// `None` once the session is closed; dropping the sender ends the
    /// thread after its current job.
    jobs: Mutex<Option<mpsc::Sender<Job>>>,
    /// Stops the statement running on the thread, from any thread.
    interrupt: rusqlite::InterruptHandle,
    /// Where the thread waits on the sink's futures.
    runtime: Handle,
    server: ServerInfo,
    dialect: SqliteDialect,
    /// A transaction is open (the connection left auto-commit).
    in_transaction: Arc<AtomicBool>,
    /// How many times [`Session::cancel`] was called, or a consumer left: a
    /// statement still waiting for the thread when it changes does not run.
    cancels: Arc<AtomicU64>,
    file: PathBuf,
}

fn closed() -> Error {
    Error::Closed("the session was closed".into())
}

impl SqliteSession {
    /// Opens the file on a new thread, which then owns the connection.
    pub(crate) async fn connect(params: &ConnectParams) -> Result<Self> {
        // No file of another machine is opened (K69, D-138).
        kerno_core::local_path::check_params(params)?;
        let file = params
            .file
            .clone()
            .filter(|file| !file.as_os_str().is_empty())
            .ok_or_else(|| Error::Config("the database file is required".into()))?;
        let read_only = params.read_only;
        let (jobs, queue) = mpsc::channel::<Job>();
        let (ready, opened) = tokio::sync::oneshot::channel();
        let path = file.clone();
        std::thread::Builder::new()
            .name("kerno-sqlite".into())
            .spawn(move || {
                let mut conn = match connect::open(&path, read_only) {
                    Ok(conn) => conn,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                if ready.send(Ok(conn.get_interrupt_handle())).is_err() {
                    return;
                }
                while let Ok(job) = queue.recv() {
                    job(&mut conn);
                }
            })
            .map_err(|error| Error::io("could not start the thread of a SQLite session", error))?;
        let interrupt = opened.await.map_err(|_| closed())??;
        let number = rusqlite::version_number();
        let to_u32 = |part: i32| u32::try_from(part).unwrap_or(0);
        Ok(Self {
            jobs: Mutex::new(Some(jobs)),
            interrupt,
            runtime: Handle::current(),
            server: ServerInfo {
                version: format!("SQLite {}", rusqlite::version()),
                version_number: (
                    to_u32(number / 1_000_000),
                    to_u32(number / 1_000 % 1_000),
                    to_u32(number % 1_000),
                ),
            },
            dialect: SqliteDialect,
            in_transaction: Arc::new(AtomicBool::new(false)),
            cancels: Arc::new(AtomicU64::new(0)),
            file,
        })
    }

    /// The database file.
    pub fn file(&self) -> &std::path::Path {
        &self.file
    }

    /// Runs `work` on the connection's thread and returns what it gives.
    pub(crate) async fn call<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut Connection) -> T + Send + 'static,
    ) -> Result<T> {
        let (answer, answered) = tokio::sync::oneshot::channel();
        let job: Job = Box::new(move |conn| {
            let _ = answer.send(work(conn));
        });
        self.jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .ok_or_else(closed)?
            .send(job)
            .map_err(|_| closed())?;
        answered.await.map_err(|_| closed())
    }
}

/// Runs every statement of `sql`, writing what each produces to `sink`.
fn run_script(conn: &Connection, sql: &str, sink: &mut ExecSink, runtime: &Handle) -> Result<()> {
    let mut batch = Batch::new(conn, sql);
    let mut ran = false;
    while let Some(mut statement) = batch.next().map_err(|error| statement_error(error, sql))? {
        ran = true;
        run_statement(conn, &mut statement, sink, runtime, sql)?;
    }
    if !ran {
        // Nothing to run (blank, comments): a statement that changed nothing.
        runtime.block_on(sink.complete(Some(0)))?;
    }
    Ok(())
}

/// Runs one prepared statement. A result's columns are described after its
/// first row is read: the declared type alone does not say what a column
/// holds (D-102).
fn run_statement(
    conn: &Connection,
    statement: &mut Statement<'_>,
    sink: &mut ExecSink,
    runtime: &Handle,
    sql: &str,
) -> Result<()> {
    let error = |error| statement_error(error, sql);
    // SQLite would run a parameter nobody filled in as NULL, without a word:
    // the query tab fills `:name` and `?` before sending, so one left here
    // (`$id`, `@id`, `?1`) is refused instead.
    if statement.parameter_count() > 0 {
        let name = statement
            .parameter_name(1)
            .map_or_else(|| "?".to_string(), str::to_string);
        return Err(DbError {
            message: format!(
                "the statement has the parameter {name}, which has no value: Kerno asks for the \
                 values of :name and ? parameters before running; write it that way, or put the \
                 value in the statement"
            ),
            ..DbError::default()
        }
        .into());
    }
    if statement.column_count() == 0 {
        let before = conn.total_changes();
        statement.raw_execute().map_err(error)?;
        // `changes` keeps the count of the last INSERT, UPDATE or DELETE:
        // a CREATE or a BEGIN changed nothing.
        let changed = if conn.total_changes() == before {
            0
        } else {
            conn.changes()
        };
        runtime.block_on(sink.complete(Some(changed)))?;
        return Ok(());
    }
    let described: Vec<(String, Option<String>)> = statement
        .columns()
        .map(|column| {
            (
                column.name().to_string(),
                column.decl_type().map(str::to_string),
            )
        })
        .collect();
    // The table column each column shows, as the bundled SQLite says (its
    // column metadata, K59); an expression names none.
    let origins: Vec<Option<ColumnOrigin>> = statement
        .columns_with_metadata()
        .map(|column| {
            Some(ColumnOrigin::Named {
                schema: column.database_name()?.to_string(),
                table: column.table_name()?.to_string(),
                column: column.origin_name()?.to_string(),
            })
        })
        .collect();
    let describe = |first: Option<&Row<'_>>| -> Vec<Column> {
        described
            .iter()
            .enumerate()
            .map(|(index, (name, declared))| {
                let value = first.map(|row| row.get_ref_unwrap(index));
                Column::new(
                    name,
                    column_type_name(declared.as_deref(), value),
                    column_kind(declared.as_deref(), value),
                )
                .with_origin(origins[index].clone())
            })
            .collect()
    };
    let mut rows = statement.raw_query();
    let mut kinds: Option<Vec<TypeKind>> = None;
    let mut count = 0u64;
    while let Some(row) = rows.next().map_err(error)? {
        let kinds = match &kinds {
            Some(kinds) => kinds,
            None => {
                let columns = describe(Some(row));
                let found = kinds.insert(columns.iter().map(|column| column.kind).collect());
                runtime.block_on(sink.columns(columns))?;
                found
            }
        };
        push_row(sink.row(), row, kinds);
        // Waits here, the statement open, until more rows are asked for.
        runtime.block_on(sink.end_row())?;
        count += 1;
    }
    if kinds.is_none() {
        runtime.block_on(sink.columns(describe(None)))?;
    }
    runtime.block_on(sink.complete(Some(count)))?;
    Ok(())
}

/// One row's cells, each by its storage class. A blob in a column that is
/// not binary is its `X'…'` literal, so it reads as bytes, not as text.
fn push_row(builder: &mut BatchBuilder, row: &Row<'_>, kinds: &[TypeKind]) {
    for (index, kind) in kinds.iter().enumerate() {
        match row.get_ref_unwrap(index) {
            ValueRef::Null => builder.push_null(),
            ValueRef::Integer(value) => builder.push_text(&value.to_string()),
            ValueRef::Real(value) => builder.push_text(&real_text(value)),
            ValueRef::Text(text) => builder.push_bytes(text),
            ValueRef::Blob(bytes) if *kind == TypeKind::Binary => builder.push_bytes(bytes),
            ValueRef::Blob(bytes) => builder.push_text(&format!("X'{}'", hex(bytes))),
        }
    }
}

#[async_trait]
impl Session for SqliteSession {
    fn engine(&self) -> Engine {
        Engine::Sqlite
    }

    fn dialect(&self) -> &dyn Dialect {
        &self.dialect
    }

    fn server(&self) -> &ServerInfo {
        &self.server
    }

    /// Always `main`: attached databases are not opened (D-102).
    fn database(&self) -> Option<String> {
        Some(MAIN.into())
    }

    /// SQLite has no failed-transaction state: after an error the
    /// transaction stays open, so this is `Idle` or `Open`.
    fn tx_state(&self) -> TxState {
        if self.in_transaction.load(Ordering::Acquire) {
            TxState::Open
        } else {
            TxState::Idle
        }
    }

    async fn execute(&self, sql: &str, sink: &mut ExecSink) -> Result<()> {
        let asked = self.cancels.load(Ordering::Acquire);
        let cancels = self.cancels.clone();
        let in_transaction = self.in_transaction.clone();
        let runtime = self.runtime.clone();
        let stop = sink.stop_signal();
        // The thread takes the sink while it runs the statement; the
        // caller's is given back after.
        let mut owned = std::mem::replace(sink, exec::channel(RowLimit::All).0);
        let sql = sql.to_string();
        let reply = self.call(move |conn| {
            let outcome = if cancels.load(Ordering::Acquire) == asked {
                run_script(conn, &sql, &mut owned, &runtime)
            } else {
                Err(Error::Cancelled)
            };
            in_transaction.store(!conn.is_autocommit(), Ordering::Release);
            (owned, outcome)
        });
        tokio::pin!(reply, stop);
        let (owned, outcome) = tokio::select! {
            biased;
            reply = &mut reply => reply?,
            () = &mut stop => {
                // The consumer left while the statement runs or waits for
                // its turn: neither goes on.
                self.cancels.fetch_add(1, Ordering::AcqRel);
                self.interrupt.interrupt();
                reply.await?
            }
        };
        *sink = owned;
        outcome
    }

    async fn cancel(&self) -> Result<()> {
        // First: a statement waiting for the thread sees it.
        self.cancels.fetch_add(1, Ordering::AcqRel);
        // A no-op when nothing runs.
        self.interrupt.interrupt();
        Ok(())
    }

    async fn ping(&self) -> Result<()> {
        self.call(|conn| conn.query_row("SELECT 1", [], |_| Ok(())))
            .await?
            .map_err(|error| statement_error(error, "SELECT 1"))
    }

    async fn close(&self) {
        let jobs = self
            .jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        // A statement still running stops; the thread then ends and closes
        // the connection, rolling back what was not committed.
        self.interrupt.interrupt();
        drop(jobs);
    }

    async fn databases(&self) -> Result<Vec<DatabaseInfo>> {
        introspect::databases(self).await
    }

    /// Always empty: a SQLite database holds tables directly.
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
        introspect::ddl(self, object).await
    }

    /// Always empty: loadable extensions are not Kerno's yet (D-102).
    async fn extensions(&self) -> Result<Vec<ExtensionInfo>> {
        Ok(Vec::new())
    }

    async fn completion_catalog(&self) -> Result<Catalog> {
        introspect::completion_catalog(self).await
    }

    /// SQLite has no sessions, users, grants or server settings.
    fn admin_query(&self, view: AdminView) -> Option<String> {
        let _ = view;
        None
    }

    async fn cancel_session(&self, id: &str) -> Result<()> {
        self.dialect.cancel_session_sql(id).map(drop)
    }

    async fn terminate_session(&self, id: &str) -> Result<()> {
        self.dialect.terminate_session_sql(id).map(drop)
    }
}
