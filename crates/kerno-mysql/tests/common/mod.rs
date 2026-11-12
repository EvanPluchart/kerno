//! Helpers shared by the MySQL / MariaDB integration tests. Every test runs
//! its body once per engine whose container is up.
#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use kerno_core::config::{ConnectParams, Engine};
use kerno_core::exec::{self, Collected, ExecEvent, Execution};
use kerno_core::{Driver, Error, Result, Session, testing};
use kerno_mysql::MysqlDriver;

/// The engines to test: those whose container is up. A `SKIPPED` line is
/// printed for each one that is down.
pub fn engines() -> Vec<Engine> {
    [Engine::MySql, Engine::MariaDb]
        .into_iter()
        .filter(|engine| testing::available(*engine))
        .collect()
}

/// Connects with `params`.
pub async fn connect(params: &ConnectParams) -> Result<Arc<dyn Session>> {
    MysqlDriver.connect(params).await
}

/// A session on the test container of `engine`. Says which engine the
/// test is on, so a failure names it.
pub async fn session(engine: Engine) -> Arc<dyn Session> {
    eprintln!("--- {}", engine.label());
    connect(&testing::params(engine))
        .await
        .unwrap_or_else(|error| panic!("connect to {}: {error}", testing::service_name(engine)))
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

/// Reads events until the execution is over and returns how it ended.
pub async fn finish(execution: &mut Execution) -> Result<()> {
    loop {
        match next_event(execution).await {
            Some(ExecEvent::Finished(result)) => return result,
            Some(_) => {}
            None => panic!("the execution ended without a result"),
        }
    }
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

/// A database name unique to one test, so tests never see each other's
/// objects. Stable across runs: each test drops its database before
/// creating it, which also clears what a failed earlier run left behind.
pub fn scratch_database(test: &str) -> String {
    format!("kerno_test_{test}")
}

/// Drops and recreates the scratch database of `test`, and returns its name.
pub async fn fresh_database(session: &Arc<dyn Session>, test: &str) -> String {
    let database = scratch_database(test);
    exec_ok(session, &format!("DROP DATABASE IF EXISTS {database}")).await;
    exec_ok(session, &format!("CREATE DATABASE {database}")).await;
    database
}
