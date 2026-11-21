//! Opening a database file, and SQLite's errors in Kerno's terms.

use std::path::Path;
use std::time::Duration;

use kerno_core::{DbError, Error};
use rusqlite::{Connection, ErrorCode, OpenFlags};

/// How long a statement waits for a lock another connection holds before
/// it fails with "database is locked".
pub(crate) const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The words [`kerno_core::error::explain`] reads to tell the failures of a
/// file apart; the raw SQLite message follows each.
pub(crate) const MISSING: &str = "the database file does not exist";
pub(crate) const NOT_A_DATABASE: &str = "the file is not a SQLite database";
pub(crate) const LOCKED: &str = "the database file is locked";
pub(crate) const READ_ONLY_FILE: &str = "the database file can only be read";

/// Opens `path` as it is: never created (a mistyped path must not leave an
/// empty database behind), never as a URI, read-only when asked. The first
/// read happens here, since an unreadable file opens without complaint.
pub(crate) fn open(path: &Path, read_only: bool) -> kerno_core::Result<Connection> {
    let shown = path.display();
    if !path.exists() {
        return Err(Error::Connect(format!("{MISSING}: {shown}")));
    }
    if path.is_dir() {
        return Err(Error::Connect(format!(
            "{NOT_A_DATABASE}: {shown} is a folder"
        )));
    }
    let access = if read_only {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    };
    let conn = Connection::open_with_flags(path, access | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|error| connect_error(&error, path))?;
    conn.busy_timeout(BUSY_TIMEOUT)
        .map_err(|error| connect_error(&error, path))?;
    conn.query_row("SELECT count(*) FROM sqlite_schema", [], |row| {
        row.get::<_, i64>(0)
    })
    .map_err(|error| connect_error(&error, path))?;
    // SQLite opens a file it may not write read-only without a word.
    if !read_only && conn.is_readonly("main").unwrap_or(false) {
        return Err(Error::Connect(format!(
            "{READ_ONLY_FILE}: {shown} is read-only on disk (attempt to write a readonly database)"
        )));
    }
    Ok(conn)
}

/// A failure to open or first read `path`.
fn connect_error(error: &rusqlite::Error, path: &Path) -> Error {
    let shown = path.display();
    let what = match error.sqlite_error_code() {
        Some(ErrorCode::NotADatabase) => NOT_A_DATABASE,
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => LOCKED,
        Some(ErrorCode::CannotOpen) if !path.exists() => MISSING,
        Some(ErrorCode::ReadOnly) => READ_ONLY_FILE,
        _ => return Error::Connect(format!("{shown}: {error}")),
    };
    Error::Connect(format!("{what}: {shown} ({error})"))
}

/// A failure of a statement, as the UI shows it: SQLite's message, its
/// code, and where in `sql` (the statement as it was sent) it points.
pub(crate) fn statement_error(error: rusqlite::Error, sql: &str) -> Error {
    if error.sqlite_error_code() == Some(ErrorCode::OperationInterrupted) {
        return Error::Cancelled;
    }
    let (message, position) = match &error {
        rusqlite::Error::SqlInputError {
            msg,
            sql: tail,
            offset,
            ..
        } => {
            // A script is prepared statement by statement: the offset is in
            // the tail being prepared, which ends the whole text.
            let start = sql.len().saturating_sub(tail.len());
            let at = start + usize::try_from(*offset).unwrap_or(0);
            let position = sql
                .get(..at.min(sql.len()))
                .map(|before| before.chars().count() as u32 + 1);
            (msg.clone(), position)
        }
        rusqlite::Error::SqliteFailure(_, Some(msg)) => (msg.clone(), None),
        other => (other.to_string(), None),
    };
    // SQLite's extended result code, as its documentation numbers it.
    let code = match &error {
        rusqlite::Error::SqliteFailure(failure, _)
        | rusqlite::Error::SqlInputError { error: failure, .. } => {
            Some(failure.extended_code.to_string())
        }
        _ => None,
    };
    let hint = matches!(
        error.sqlite_error_code(),
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
    .then(|| {
        "Another connection holds the file: a result still being read or a transaction left \
         open in another tab, or another program. Finish or close it, then run again."
            .to_string()
    });
    DbError {
        message,
        code,
        severity: Some("ERROR".into()),
        position,
        hint,
        detail: None,
    }
    .into()
}
