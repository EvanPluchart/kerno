//! Streaming statement execution.
//!
//! A driver writes what a statement produces into an [`ExecSink`]; the
//! consumer reads [`ExecEvent`]s from the matching [`Execution`]. Rows flow
//! only as fast as the consumer asks for them ([`Execution::request_rows`]):
//! when demand runs out the driver stops reading from its socket, the
//! server blocks on its side, and memory stays bounded by what was fetched.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::{Notify, mpsc};

use crate::dialect::IsolationLevel;
use crate::driver::Session;
use crate::error::{DbError, Error, Result};
use crate::result::{BatchBuilder, Column, RowBatch};

/// A message the server attached to a statement without failing it
/// (PostgreSQL `NOTICE`/`WARNING`, MySQL warnings).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// Severity as the server names it (`NOTICE`, `WARNING`, `Note`, ...).
    pub severity: String,
    /// The message.
    pub message: String,
    /// Engine error code, when there is one.
    pub code: Option<String>,
    /// Extra detail, when there is some.
    pub detail: Option<String>,
}

/// What an execution produces, in order.
#[derive(Debug)]
pub enum ExecEvent {
    /// A result set starts; its rows follow.
    Columns(Arc<[Column]>),
    /// More rows of the current result set.
    Rows(RowBatch),
    /// A statement finished. `rows_affected` is the count the server
    /// reported (rows changed, or rows returned for a result set).
    Complete {
        /// Rows changed or returned, when the server says.
        rows_affected: Option<u64>,
    },
    /// The server said something on the side.
    Notice(Notice),
    /// The execution is over: nothing follows. `Err` carries the failure;
    /// results delivered before it stand.
    Finished(Result<()>),
}

/// Rows the consumer is ready to receive. `u64::MAX` means no limit.
#[derive(Debug)]
struct Demand {
    rows: AtomicU64,
    changed: Notify,
}

const UNLIMITED: u64 = u64::MAX;

/// The driver's end of an execution.
pub struct ExecSink {
    events: mpsc::Sender<ExecEvent>,
    demand: Arc<Demand>,
    builder: BatchBuilder,
    /// Rows flushed in the current result set, to grow the batch size.
    flushed_batches: u32,
    last_flush: Instant,
}

/// The consumer went away or asked to stop: the driver must abort the
/// statement and return.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stopped;

impl From<Stopped> for Error {
    fn from(_: Stopped) -> Self {
        Error::Cancelled
    }
}

/// First batch is small so the first rows reach the screen at once; later
/// batches are larger to keep per-batch overhead low.
const FIRST_BATCH_ROWS: usize = 64;
const BATCH_ROWS: usize = 1024;
/// A partial batch is flushed when a row arrives this long after the last
/// flush, so slowly produced rows show up as they come.
// ponytail: checked only when a row arrives, so a burst of fewer than
// BATCH_ROWS rows followed by a long server pause stays buffered until the
// next row or the end of the result. Add a timer-driven flush if a real
// workload shows it.
const FLUSH_INTERVAL: Duration = Duration::from_millis(30);

impl ExecSink {
    /// Announces a result set. Must precede its rows.
    pub async fn columns(&mut self, columns: Vec<Column>) -> Result<(), Stopped> {
        self.flush().await?;
        self.builder = BatchBuilder::new(columns.len());
        self.flushed_batches = 0;
        self.last_flush = Instant::now();
        self.send(ExecEvent::Columns(columns.into())).await
    }

    /// The builder for the row being written. Append exactly one cell per
    /// column, then call [`Self::end_row`].
    pub fn row(&mut self) -> &mut BatchBuilder {
        &mut self.builder
    }

    /// Completes the row written through [`Self::row`]. Waits here when the
    /// consumer has not asked for more rows: the driver must not read the
    /// next row from the server until this returns.
    pub async fn end_row(&mut self) -> Result<(), Stopped> {
        loop {
            let demand = self.demand.rows.load(Ordering::Acquire);
            let pending = self.builder.row_count() as u64;
            if demand == 0 {
                // The row just read is one past what was asked for. Hold it
                // (it proves the result has more) until demand returns.
                self.wait_for_demand().await?;
                continue;
            }
            let target = if self.flushed_batches == 0 {
                FIRST_BATCH_ROWS
            } else {
                BATCH_ROWS
            } as u64;
            if pending >= demand.min(target)
                || self.builder.is_full()
                || self.last_flush.elapsed() >= FLUSH_INTERVAL
            {
                self.flush().await?;
            }
            return Ok(());
        }
    }

    /// Reports the end of the current statement.
    pub async fn complete(&mut self, rows_affected: Option<u64>) -> Result<(), Stopped> {
        self.flush_all().await?;
        self.send(ExecEvent::Complete { rows_affected }).await
    }

    /// Forwards a server notice.
    pub async fn notice(&mut self, notice: Notice) -> Result<(), Stopped> {
        self.send(ExecEvent::Notice(notice)).await
    }

    /// Whether the consumer is gone.
    pub fn is_stopped(&self) -> bool {
        self.events.is_closed()
    }

    /// Resolves when the consumer goes away. Drivers race this against the
    /// server's next message, so an abandoned statement that produces
    /// nothing for a long time is still cancelled promptly.
    pub async fn stopped(&self) {
        self.events.closed().await;
    }

    async fn send(&mut self, event: ExecEvent) -> Result<(), Stopped> {
        self.events.send(event).await.map_err(|_| Stopped)
    }

    async fn wait_for_demand(&mut self) -> Result<(), Stopped> {
        let changed = self.demand.changed.notified();
        tokio::pin!(changed);
        // Register before re-checking, so a request between the check and
        // the await is not missed.
        changed.as_mut().enable();
        if self.demand.rows.load(Ordering::Acquire) > 0 {
            return Ok(());
        }
        tokio::select! {
            () = &mut changed => Ok(()),
            () = self.events.closed() => Err(Stopped),
        }
    }

    /// Sends the rows buffered so far, at most as many as were asked for.
    async fn flush(&mut self) -> Result<(), Stopped> {
        if self.builder.row_count() == 0 {
            return Ok(());
        }
        let rows = self.builder.row_count() as u64;
        let batch = self.builder.finish();
        self.flushed_batches += 1;
        self.last_flush = Instant::now();
        let demand = &self.demand.rows;
        // Saturating: demand may have been reset to a smaller number meanwhile.
        let _ = demand.try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            (current != UNLIMITED).then(|| current.saturating_sub(rows))
        });
        self.send(ExecEvent::Rows(batch)).await
    }

    /// Sends every buffered row, waiting for demand if the last row read was
    /// held back.
    async fn flush_all(&mut self) -> Result<(), Stopped> {
        while self.builder.row_count() > 0 {
            if self.demand.rows.load(Ordering::Acquire) == 0 {
                self.wait_for_demand().await?;
            }
            self.flush().await?;
        }
        Ok(())
    }
}

/// The consumer's end of an execution: a stream of [`ExecEvent`]s ending
/// with [`ExecEvent::Finished`]. Dropping it aborts the statement.
pub struct Execution {
    events: mpsc::Receiver<ExecEvent>,
    demand: Arc<Demand>,
    finished: bool,
}

impl Execution {
    /// The next event, or `None` after [`ExecEvent::Finished`] was returned.
    pub async fn next(&mut self) -> Option<ExecEvent> {
        if self.finished {
            return None;
        }
        match self.events.recv().await {
            Some(event) => {
                self.finished = matches!(event, ExecEvent::Finished(_));
                Some(event)
            }
            None => {
                // The driver task ended without a final event: it panicked
                // or the runtime shut down.
                self.finished = true;
                Some(ExecEvent::Finished(Err(Error::Closed(
                    "the query task stopped unexpectedly".into(),
                ))))
            }
        }
    }

    /// Asks for `rows` more rows (on top of what is still outstanding).
    pub fn request_rows(&self, rows: u64) {
        self.demand().request_rows(rows);
    }

    /// Asks for every remaining row.
    pub fn request_all(&self) {
        self.demand().request_all();
    }

    /// A handle to ask for more rows from elsewhere than where the events
    /// are read (the grid that shows them, for one).
    pub fn demand(&self) -> RowDemand {
        RowDemand(self.demand.clone())
    }
}

/// Asks an [`Execution`] for more rows. Outliving the execution is
/// harmless: nobody is left to hear the request.
#[derive(Debug, Clone)]
pub struct RowDemand(Arc<Demand>);

impl RowDemand {
    /// Asks for `rows` more rows (on top of what is still outstanding).
    pub fn request_rows(&self, rows: u64) {
        let _ = self
            .0
            .rows
            .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current != UNLIMITED).then(|| current.saturating_add(rows).min(UNLIMITED - 1))
            });
        self.0.changed.notify_waiters();
    }

    /// Asks for every remaining row.
    pub fn request_all(&self) {
        self.0.rows.store(UNLIMITED, Ordering::Release);
        self.0.changed.notify_waiters();
    }
}

/// How many rows an execution delivers before waiting for
/// [`Execution::request_rows`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowLimit {
    /// Deliver this many rows, then wait.
    Rows(u64),
    /// Deliver everything.
    All,
}

/// Creates a connected sink and execution.
pub fn channel(initial: RowLimit) -> (ExecSink, Execution) {
    // Small buffer: batches are large, and back-pressure is the point.
    let (tx, rx) = mpsc::channel(4);
    let demand = Arc::new(Demand {
        rows: AtomicU64::new(match initial {
            RowLimit::Rows(rows) => rows.min(UNLIMITED - 1),
            RowLimit::All => UNLIMITED,
        }),
        changed: Notify::new(),
    });
    (
        ExecSink {
            events: tx,
            demand: demand.clone(),
            builder: BatchBuilder::new(0),
            flushed_batches: 0,
            last_flush: Instant::now(),
        },
        Execution {
            events: rx,
            demand,
            finished: false,
        },
    )
}

/// Runs `sql` on `session` in a background task and returns the stream of
/// what it produces. Must be called from within the Tokio runtime.
///
/// If the returned [`Execution`] is dropped before the statement ends, the
/// driver cancels the statement on the server (see [`Session::execute`]).
pub fn run(session: Arc<dyn Session>, sql: String, initial: RowLimit) -> Execution {
    let (mut sink, execution) = channel(initial);
    tokio::spawn(async move {
        let mut result = session.execute(&sql, &mut sink).await;
        if result.is_ok() {
            result = sink.flush_all().await.map_err(Error::from);
        }
        let _ = sink.events.send(ExecEvent::Finished(result)).await;
    });
    execution
}

/// Everything one statement returned, fully fetched.
#[derive(Debug, Default)]
pub struct Collected {
    /// Result sets, in order.
    pub results: Vec<CollectedResult>,
    /// Notices raised along the way.
    pub notices: Vec<Notice>,
}

/// One result set or command outcome.
#[derive(Debug, Default)]
pub struct CollectedResult {
    /// Columns; empty for statements that return no rows.
    pub columns: Arc<[Column]>,
    /// The rows.
    pub batches: Vec<RowBatch>,
    /// Count reported by the server.
    pub rows_affected: Option<u64>,
}

impl CollectedResult {
    /// Number of rows fetched.
    pub fn row_count(&self) -> usize {
        self.batches.iter().map(RowBatch::row_count).sum()
    }

    /// Every row as owned strings (`None` for NULL). For tests and small
    /// metadata queries only: it allocates per cell.
    pub fn rows(&self) -> Vec<Vec<Option<String>>> {
        let mut rows = Vec::with_capacity(self.row_count());
        for batch in &self.batches {
            for row in 0..batch.row_count() {
                rows.push(
                    (0..batch.column_count())
                        .map(|column| {
                            batch
                                .cell(row, column)
                                .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
                        })
                        .collect(),
                );
            }
        }
        rows
    }
}

/// Runs `sql` and fetches everything it returns.
pub async fn collect(session: Arc<dyn Session>, sql: impl Into<String>) -> Result<Collected> {
    let mut execution = run(session, sql.into(), RowLimit::All);
    let mut collected = Collected::default();
    let mut current: Option<CollectedResult> = None;
    while let Some(event) = execution.next().await {
        match event {
            ExecEvent::Columns(columns) => {
                current = Some(CollectedResult {
                    columns,
                    ..CollectedResult::default()
                });
            }
            ExecEvent::Rows(batch) => {
                if let Some(result) = &mut current {
                    result.batches.push(batch);
                }
            }
            ExecEvent::Complete { rows_affected } => {
                let mut result = current.take().unwrap_or_default();
                result.rows_affected = rows_affected;
                collected.results.push(result);
            }
            ExecEvent::Notice(notice) => collected.notices.push(notice),
            ExecEvent::Finished(result) => result?,
        }
    }
    Ok(collected)
}

/// The isolation level the server gives `session`'s next transactions
/// ([`Dialect::isolation_sql`]); `None` for a name of no standard level.
pub async fn isolation(session: Arc<dyn Session>) -> Result<Option<IsolationLevel>> {
    let sql = session.dialect().isolation_sql();
    let collected = collect(session, sql).await?;
    Ok(collected
        .results
        .first()
        .and_then(|result| result.rows().into_iter().next())
        .and_then(|row| row.into_iter().next().flatten())
        .and_then(|name| IsolationLevel::parse(&name)))
}

/// Gives `session`'s next transactions `level` (for this session only),
/// then reads back the level the server now gives them.
pub async fn set_isolation(
    session: Arc<dyn Session>,
    level: IsolationLevel,
) -> Result<Option<IsolationLevel>> {
    collect(session.clone(), session.dialect().set_isolation_sql(level)).await?;
    isolation(session).await
}

impl From<DbError> for Notice {
    fn from(error: DbError) -> Self {
        Self {
            severity: error.severity.unwrap_or_else(|| "NOTICE".into()),
            message: error.message,
            code: error.code,
            detail: error.detail,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::TypeKind;

    /// Plays a driver producing `rows` single-column rows.
    async fn produce(mut sink: ExecSink, rows: u64) -> Result<u64, Stopped> {
        sink.columns(vec![Column::new("n", "int", TypeKind::Integer)])
            .await?;
        for n in 0..rows {
            sink.row().push_text(&n.to_string());
            sink.end_row().await?;
        }
        sink.complete(Some(rows)).await?;
        Ok(rows)
    }

    /// Reads events until `Complete`, returning the row values seen.
    async fn read_rows(execution: &mut Execution, until_rows: usize) -> (Vec<String>, bool) {
        let mut seen = Vec::new();
        let mut complete = false;
        while seen.len() < until_rows && !complete {
            match tokio::time::timeout(Duration::from_secs(5), execution.next())
                .await
                .expect("timed out waiting for an event")
            {
                Some(ExecEvent::Rows(batch)) => {
                    for row in 0..batch.row_count() {
                        seen.push(batch.text(row, 0).unwrap().to_string());
                    }
                }
                Some(ExecEvent::Complete { .. }) | None => complete = true,
                Some(_) => {}
            }
        }
        (seen, complete)
    }

    #[tokio::test]
    async fn delivers_exactly_the_rows_asked_for_then_waits() {
        let (sink, mut execution) = channel(RowLimit::Rows(100));
        let producer = tokio::spawn(produce(sink, 250));

        let (first, complete) = read_rows(&mut execution, 100).await;
        assert_eq!(first.len(), 100);
        assert!(!complete);
        assert_eq!(first[99], "99");

        // No demand left: nothing more may arrive, and the producer is parked.
        let idle = tokio::time::timeout(Duration::from_millis(150), execution.next()).await;
        assert!(idle.is_err(), "received an event without demand: {idle:?}");
        assert!(!producer.is_finished());

        execution.request_rows(100);
        let (second, complete) = read_rows(&mut execution, 100).await;
        assert_eq!(second.len(), 100);
        assert_eq!(second[0], "100");
        assert!(!complete);

        execution.request_all();
        let (rest, complete) = read_rows(&mut execution, usize::MAX).await;
        assert_eq!(rest.len(), 50);
        assert_eq!(rest[49], "249");
        assert!(complete);
        assert_eq!(producer.await.unwrap(), Ok(250));
    }

    #[tokio::test]
    async fn completes_without_more_demand_when_the_result_ends_at_the_limit() {
        // Exactly as many rows as asked for: the consumer must learn the
        // result is over without having to ask again.
        let (sink, mut execution) = channel(RowLimit::Rows(100));
        let producer = tokio::spawn(produce(sink, 100));
        let (rows, complete) = read_rows(&mut execution, usize::MAX).await;
        assert_eq!(rows.len(), 100);
        assert!(complete);
        assert_eq!(producer.await.unwrap(), Ok(100));
    }

    #[tokio::test]
    async fn first_rows_arrive_before_a_large_result_is_done() {
        let (sink, mut execution) = channel(RowLimit::All);
        let _producer = tokio::spawn(produce(sink, 100_000));
        let mut first_batch = None;
        while first_batch.is_none() {
            if let Some(ExecEvent::Rows(batch)) = execution.next().await {
                first_batch = Some(batch.row_count());
            }
        }
        assert_eq!(first_batch, Some(FIRST_BATCH_ROWS));
    }

    #[tokio::test]
    async fn dropping_the_execution_stops_a_parked_producer() {
        let (sink, mut execution) = channel(RowLimit::Rows(10));
        let producer = tokio::spawn(produce(sink, 1_000));
        let (rows, _) = read_rows(&mut execution, 10).await;
        assert_eq!(rows.len(), 10);
        drop(execution);
        let outcome = tokio::time::timeout(Duration::from_secs(5), producer)
            .await
            .expect("producer did not notice the consumer left")
            .unwrap();
        assert_eq!(outcome, Err(Stopped));
    }

    #[tokio::test]
    async fn empty_results_and_commands_complete() {
        let (mut sink, mut execution) = channel(RowLimit::Rows(0));
        tokio::spawn(async move {
            sink.columns(vec![Column::new("n", "int", TypeKind::Integer)])
                .await?;
            sink.complete(Some(0)).await?;
            sink.complete(Some(3)).await
        });
        assert!(matches!(
            execution.next().await,
            Some(ExecEvent::Columns(_))
        ));
        assert!(matches!(
            execution.next().await,
            Some(ExecEvent::Complete {
                rows_affected: Some(0)
            })
        ));
        assert!(matches!(
            execution.next().await,
            Some(ExecEvent::Complete {
                rows_affected: Some(3)
            })
        ));
        // The sink was dropped without `Finished`: reported as a failure once.
        assert!(matches!(
            execution.next().await,
            Some(ExecEvent::Finished(Err(Error::Closed(_))))
        ));
        assert!(execution.next().await.is_none());
    }
}
