//! Helpers shared by the PostgreSQL integration tests.
#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use kerno_core::config::{ConnectParams, Engine};
use kerno_core::exec::{self, Collected, ExecEvent, Execution};
use kerno_core::{Driver, Error, Result, Session, testing};
use kerno_postgres::PostgresDriver;

pub const ENGINE: Engine = Engine::Postgres;

/// Connects with `params`.
pub async fn connect(params: &ConnectParams) -> Result<Arc<dyn Session>> {
    PostgresDriver.connect(params).await
}

/// A session on the test container, or `None` (after a `SKIPPED` line)
/// when the container is down.
pub async fn session() -> Option<Arc<dyn Session>> {
    if !testing::available(ENGINE) {
        return None;
    }
    Some(
        connect(&testing::params(ENGINE))
            .await
            .expect("connect to kerno-postgres"),
    )
}

/// Runs `sql` and returns everything it produced.
pub async fn run(session: &Arc<dyn Session>, sql: &str) -> Result<Collected> {
    exec::collect(session.clone(), sql).await
}

/// The rows of the first result of `sql`.
pub async fn rows(session: &Arc<dyn Session>, sql: &str) -> Vec<Vec<Option<String>>> {
    run(session, sql)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
        .results
        .first()
        .map(|result| result.rows())
        .unwrap_or_default()
}

/// The single value `sql` returns.
pub async fn scalar(session: &Arc<dyn Session>, sql: &str) -> String {
    rows(session, sql)
        .await
        .into_iter()
        .next()
        .and_then(|row| row.into_iter().next())
        .flatten()
        .unwrap_or_else(|| panic!("{sql}: no value"))
}

/// Runs `sql` expecting it to succeed.
pub async fn exec_ok(session: &Arc<dyn Session>, sql: &str) {
    run(session, sql)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}

/// The server error `sql` fails with.
pub async fn db_error(session: &Arc<dyn Session>, sql: &str) -> kerno_core::DbError {
    match run(session, sql).await {
        Err(Error::Db(error)) => *error,
        other => panic!("{sql}: expected a server error, got {other:?}"),
    }
}

/// The next event, failing the test if none arrives in time.
pub async fn next_event(execution: &mut Execution) -> Option<ExecEvent> {
    tokio::time::timeout(Duration::from_secs(20), execution.next())
        .await
        .expect("timed out waiting for an execution event")
}

/// Reads row events until `count` rows were seen or the result completed.
/// Returns the number of rows seen and whether `Complete` arrived.
pub async fn read_rows(execution: &mut Execution, count: usize) -> (usize, bool) {
    let mut seen = 0;
    while seen < count {
        match next_event(execution).await {
            Some(ExecEvent::Rows(batch)) => seen += batch.row_count(),
            Some(ExecEvent::Complete { .. }) => return (seen, true),
            Some(ExecEvent::Finished(result)) => {
                result.expect("execution failed");
                return (seen, true);
            }
            Some(_) => {}
            None => return (seen, true),
        }
    }
    (seen, false)
}

/// A schema name unique to one test, so tests never see each other's
/// objects. Stable across runs: each test drops its schema before creating
/// it, which also clears what a failed earlier run left behind.
pub fn scratch_schema(test: &str) -> String {
    format!("kerno_test_{test}")
}
