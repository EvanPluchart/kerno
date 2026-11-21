//! Importing a CSV file into a table, in one transaction.
//!
//! The file is read a batch at a time on a blocking thread; each batch is
//! one multi-row `INSERT`. Memory holds one batch, whatever the size of the
//! file.

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::driver::{Session, TxState};
use crate::error::{Error, Result};
use crate::exec;
use crate::export_file::Encoding;
use crate::result::TypeKind;
use crate::schema::{ColumnInfo, ObjectRef, RelationKind, TableRef};
use crate::sql::edits::CellValue;
use crate::sql::params::is_number;
use crate::table_draft::{AutoIncrement, ColumnDraft, TableDraft};

/// How a CSV file is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsvOptions {
    /// What separates the fields of a record.
    pub delimiter: char,
    /// Whether the first record names the columns.
    pub header: bool,
    /// An unquoted field that reads exactly this is NULL. A quoted one is
    /// always a value, so `""` stays the empty string when this is empty.
    pub null_text: String,
    /// The file's encoding. A byte order mark at the start is skipped.
    pub encoding: Encoding,
}

/// The first records of a file, to show before importing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsvPreview {
    /// Column names: the header's, or `column_1`, `column_2`… One per
    /// column of the widest record read.
    pub headers: Vec<String>,
    /// Records as read; `None` for NULL: an unquoted field that reads the
    /// NULL text (a quoted one is a value, even `""`).
    pub rows: Vec<Vec<Option<String>>>,
    /// The kind each column's values look like (see [`guess_kind`]).
    pub guessed: Vec<TypeKind>,
}

/// The header (if any) and up to `rows` records of `path`, with a guess of
/// each column's kind. Reads the file: call it off the UI thread.
pub fn preview(path: &Path, options: &CsvOptions, rows: usize) -> Result<CsvPreview> {
    let failed = |error| read_error(path, error);
    let mut reader = CsvReader::open(path, options).map_err(failed)?;
    let header = if options.header {
        reader.next_record().map_err(failed)?
    } else {
        None
    };
    let mut records = Vec::new();
    while records.len() < rows {
        match reader.next_record().map_err(failed)? {
            Some(record) => records.push(record.fields),
            None => break,
        }
    }
    let text = |field: &Option<String>| field.clone().unwrap_or_else(|| options.null_text.clone());
    let width = records
        .iter()
        .map(Vec::len)
        .chain(header.iter().map(|record| record.fields.len()))
        .max()
        .unwrap_or(0);
    let headers = (0..width)
        .map(|index| {
            let named = header.as_ref().and_then(|record| record.fields.get(index));
            match named.map(text) {
                Some(name) if !name.is_empty() => name,
                _ => format!("column_{}", index + 1),
            }
        })
        .collect();
    let guessed = (0..width)
        .map(|index| {
            let values: Vec<&str> = records
                .iter()
                .filter_map(|row| row.get(index)?.as_deref())
                .collect();
            guess_kind(&values, &options.null_text)
        })
        .collect();
    Ok(CsvPreview {
        headers,
        rows: records,
        guessed,
    })
}

/// The records of `path`, header left out, as an import reads them: the
/// file is streamed, one record in memory at a time. Fails where the
/// import would (a quote not closed, text not in the encoding). Reads the
/// whole file: call it off the UI thread. Ends with [`Error::Cancelled`]
/// once `stop` is set, checked at each record: the preview's scan of a
/// large file, when a field changes and another scan starts (C3).
pub fn count_records(path: &Path, options: &CsvOptions, stop: &AtomicBool) -> Result<u64> {
    let failed = |error| read_error(path, error);
    let mut reader = CsvReader::open(path, options).map_err(failed)?;
    if options.header {
        reader.next_record().map_err(failed)?;
    }
    let mut records = 0;
    while reader.next_record().map_err(failed)?.is_some() {
        if stop.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        records += 1;
    }
    Ok(records)
}

/// A kind, and whether a value looks like it.
type Candidate = (TypeKind, fn(&str) -> bool);

/// The kind every value looks like, trying in order `Integer` (fits a
/// 64-bit integer, no leading zero), `Decimal`, `Boolean` (`t`, `f`,
/// `true`, `false`), `Date` (`YYYY-MM-DD`), `DateTime` (a date, `T` or a
/// space, `HH:MM[:SS[.fraction]]`, no zone), `Uuid`, `Json` (an object or
/// an array), and `Text` for anything else. Values equal to `null_text`
/// are left out; with no value left, the kind is `Text`.
pub fn guess_kind(values: &[&str], null_text: &str) -> TypeKind {
    let values: Vec<&str> = values
        .iter()
        .copied()
        .filter(|value| *value != null_text)
        .collect();
    let candidates: [Candidate; 7] = [
        (TypeKind::Integer, is_integer),
        (TypeKind::Decimal, is_decimal),
        (TypeKind::Boolean, |value| {
            ["t", "f", "true", "false"]
                .iter()
                .any(|spelling| value.eq_ignore_ascii_case(spelling))
        }),
        (TypeKind::Date, is_date),
        (TypeKind::DateTime, is_date_time),
        (TypeKind::Uuid, is_uuid),
        (TypeKind::Json, |value| {
            value.starts_with(['{', '['])
                && serde_json::from_str::<serde_json::Value>(value).is_ok()
        }),
    ];
    if values.is_empty() {
        return TypeKind::Text;
    }
    candidates
        .into_iter()
        .find(|(_, matches)| values.iter().all(|value| matches(value)))
        .map_or(TypeKind::Text, |(kind, _)| kind)
}

/// Digits, maybe signed, without a leading zero (codes such as `00123`
/// are text), that fit 64 bits.
fn is_integer(value: &str) -> bool {
    let digits = value.strip_prefix('-').unwrap_or(value);
    !digits.is_empty()
        && digits.bytes().all(|byte| byte.is_ascii_digit())
        && (digits == "0" || !digits.starts_with('0'))
        && value.parse::<i64>().is_ok()
}

/// A number with digits before the point, without a leading zero.
fn is_decimal(value: &str) -> bool {
    let unsigned = value.strip_prefix('-').unwrap_or(value);
    let whole = unsigned.split(['.', 'e', 'E']).next().unwrap_or("");
    is_number(value)
        && value == value.trim()
        && !value.starts_with('+')
        && !whole.is_empty()
        && (whole == "0" || !whole.starts_with('0'))
}

/// `YYYY-MM-DD` with a month and a day in range.
fn is_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    let number = |range: std::ops::Range<usize>| -> Option<u32> {
        let part = value.get(range)?;
        part.bytes()
            .all(|byte| byte.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && number(0..4).is_some()
        && number(5..7).is_some_and(|month| (1..=12).contains(&month))
        && number(8..10).is_some_and(|day| (1..=31).contains(&day))
}

/// A date, then `T` or a space and `HH:MM[:SS[.fraction]]`, no zone; or
/// a date alone, so a column mixing both is a date-time.
fn is_date_time(value: &str) -> bool {
    if value.len() == 10 {
        return is_date(value);
    }
    let Some((date, time)) = value.split_at_checked(10) else {
        return false;
    };
    let Some(time) = time.strip_prefix(['T', ' ']) else {
        return false;
    };
    let (clock, fraction) = time.split_once('.').unwrap_or((time, "0"));
    let parts: Vec<&str> = clock.split(':').collect();
    let two_digits = |part: &str, max: u32| {
        part.len() == 2 && part.parse::<u32>().is_ok_and(|number| number <= max)
    };
    is_date(date)
        && matches!(parts.len(), 2 | 3)
        && two_digits(parts[0], 23)
        && two_digits(parts[1], 59)
        && parts.get(2).is_none_or(|seconds| two_digits(seconds, 59))
        && !fraction.is_empty()
        && fraction.bytes().all(|byte| byte.is_ascii_digit())
}

/// `8-4-4-4-12` hexadecimal digits.
fn is_uuid(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    groups.iter().map(|group| group.len()).eq([8, 4, 4, 4, 12])
        && groups
            .iter()
            .all(|group| group.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// The table rows go into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A table that exists.
    Existing(TableRef),
    /// A table to create, with its columns: name and SQL type
    /// (see [`crate::Dialect::type_for`]).
    New {
        /// The table.
        table: TableRef,
        /// `(name, SQL type)` per column.
        columns: Vec<(String, String)>,
    },
}

/// What to import, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportJob {
    /// The CSV file.
    pub path: PathBuf,
    /// How it is written.
    pub options: CsvOptions,
    /// Where its rows go.
    pub target: Target,
    /// Per CSV column, the target column it goes into, or `None` to skip
    /// it. One entry per column of the file.
    pub mapping: Vec<Option<String>>,
    /// Rows per `INSERT` (500 is a good default).
    pub batch_rows: usize,
}

/// How an import ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReport {
    /// Rows inserted; 0 when the import failed.
    pub inserted: u64,
    /// Why nothing was imported, when a row could not be.
    pub failed: Option<ImportFailure>,
}

/// The record that stopped an import. Nothing was imported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportFailure {
    /// The line of the file the record starts on, from 1.
    pub line: u64,
    /// What went wrong: the server's message, or what is wrong with the
    /// record.
    pub message: String,
}

impl std::fmt::Display for ImportFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

/// The savepoint each batch runs under, so a failed batch can be retried
/// row by row to find the failing line.
const SAVEPOINT: &str = "kerno_import";

/// Imports the CSV file of `job` in one transaction: every row, or none.
///
/// `columns` are the columns of an existing target table (from
/// [`Session::table_structure`]); they decide how values are written. For
/// a new table they are ignored: the table is created, then its columns
/// read back.
///
/// The transaction starts, a new table is created, each batch of
/// `job.batch_rows` records is one multi-row `INSERT` under a savepoint,
/// and the transaction commits. A record the server refuses, or that does
/// not have the file's number of fields, rolls everything back and is
/// reported in [`ImportReport::failed`] with its line. On MySQL and MariaDB
/// `CREATE TABLE` commits on its own: a new table is created before the
/// transaction and dropped if the import does not complete.
///
/// `progress` receives the rows inserted so far after each batch. Setting
/// `cancel` rolls back between two batches and returns
/// [`Error::Cancelled`]. Fails without touching the server when the
/// mapping does not fit the file or the table, or when the session is in a
/// transaction. Must be called from within the Tokio runtime.
pub async fn import_csv(
    session: Arc<dyn Session>,
    columns: Vec<ColumnInfo>,
    job: ImportJob,
    progress: impl Fn(u64) + Send + 'static,
    cancel: Arc<AtomicBool>,
) -> Result<ImportReport> {
    let ImportJob {
        path,
        options,
        target,
        mapping,
        batch_rows,
    } = job;
    let invalid = |message: String| Err(Error::Config(message));
    if batch_rows == 0 {
        return invalid("an import needs at least one row per batch".into());
    }
    if session.tx_state() != TxState::Idle {
        return invalid(
            "a transaction is already open in this session; commit or roll it back first".into(),
        );
    }
    let (table, names): (&TableRef, Vec<&str>) = match &target {
        Target::Existing(table) => (table, columns.iter().map(|c| c.name.as_str()).collect()),
        Target::New { table, columns } => (
            table,
            columns.iter().map(|(name, _)| name.as_str()).collect(),
        ),
    };
    let mapped: Vec<(usize, &str)> = mapping
        .iter()
        .enumerate()
        .filter_map(|(index, target)| Some((index, target.as_deref()?)))
        .collect();
    if mapped.is_empty() {
        return invalid("no column of the file is mapped to a column of the table".into());
    }
    let mut seen = HashSet::new();
    for (_, name) in &mapped {
        if !names.contains(name) {
            return invalid(format!("the table has no column \"{name}\""));
        }
        if !seen.insert(*name) {
            return invalid(format!("two columns of the file go into \"{name}\""));
        }
    }

    // Open the file and check its header before anything runs.
    let (reader, header) = blocking({
        let (path, options) = (path.clone(), options.clone());
        move || {
            let mut reader = CsvReader::open(&path, &options)?;
            let header = if options.header {
                reader.next_record()?
            } else {
                None
            };
            Ok((reader, header))
        }
    })
    .await
    .map_err(|error| read_error(&path, error))?;
    if let Some(header) = header
        && header.fields.len() != mapping.len()
    {
        return invalid(format!(
            "the file has {} columns and the mapping {}",
            header.fields.len(),
            mapping.len()
        ));
    }

    let dialect = session.dialect();
    let create = match &target {
        Target::Existing(_) => Vec::new(),
        Target::New { table, columns } => create_table_sql(dialect, table, columns)?,
    };
    // A `CREATE` that commits (MySQL) runs before the transaction of rows.
    let create_first = !create.is_empty() && !dialect.transactional_ddl();
    if create_first {
        for sql in &create {
            run(&session, sql).await?;
        }
    }

    let mut inserted = 0;
    let outcome: Result<Option<ImportFailure>> = async {
        run(&session, session.dialect().begin_sql()).await?;
        if !create_first {
            for sql in &create {
                run(&session, sql).await?;
            }
        }
        let columns = match &target {
            Target::Existing(_) => columns,
            Target::New { .. } => session.table_structure(table).await?.columns,
        };
        let mut targets = Vec::with_capacity(mapped.len());
        for (index, name) in &mapped {
            let column = columns
                .iter()
                .find(|column| column.name == *name)
                .ok_or_else(|| Error::Config(format!("the table has no column \"{name}\"")))?;
            targets.push((*index, column));
        }
        let quoted: Vec<String> = mapped
            .iter()
            .map(|(_, name)| dialect.quote_ident(name))
            .collect();
        let head = format!(
            "INSERT INTO {} ({}) VALUES ",
            dialect.table_name(table),
            quoted.join(", ")
        );

        let mut reader = Some(reader);
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(Error::Cancelled);
            }
            let mut taken = reader
                .take()
                .expect("the reader is put back after each batch");
            let (back, batch) = blocking(move || {
                let batch = read_batch(&mut taken, batch_rows);
                Ok((taken, batch))
            })
            .await
            .map_err(|error| read_error(&path, error))?;
            reader = Some(back);
            let batch = match batch {
                Ok(batch) if batch.is_empty() => break,
                Ok(batch) => batch,
                Err(CsvError::Bad(failure)) => return Ok(Some(failure)),
                Err(CsvError::Io(error)) => return Err(read_error(&path, CsvError::Io(error))),
            };
            let mut rows = Vec::with_capacity(batch.len());
            for record in &batch {
                match row_values(record, mapping.len(), &targets, dialect) {
                    Ok(row) => rows.push(row),
                    Err(failure) => return Ok(Some(failure)),
                }
            }
            run(&session, &format!("SAVEPOINT {SAVEPOINT}")).await?;
            match run(&session, &format!("{head}{}", rows.join(", "))).await {
                Ok(()) => {
                    run(&session, &format!("RELEASE SAVEPOINT {SAVEPOINT}")).await?;
                    inserted += batch.len() as u64;
                    progress(inserted);
                }
                // A cancel that stopped the statement on the server is not a
                // record it refused: nothing is tried again.
                Err(_) if cancel.load(Ordering::Acquire) => return Err(Error::Cancelled),
                Err(error) if error.db().is_some() => {
                    // Find the record the server refuses.
                    run(&session, &format!("ROLLBACK TO SAVEPOINT {SAVEPOINT}")).await?;
                    for (record, row) in batch.iter().zip(&rows) {
                        match run(&session, &format!("{head}{row}")).await {
                            Ok(()) => {}
                            Err(_) if cancel.load(Ordering::Acquire) => {
                                return Err(Error::Cancelled);
                            }
                            Err(error) if error.db().is_some() => {
                                return Ok(Some(refused(record.line, &error)));
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    return Ok(Some(refused(batch[0].line, &error)));
                }
                Err(error) => return Err(error),
            }
        }
        run(&session, "COMMIT").await?;
        Ok(None)
    }
    .await;

    if let Ok(None) = outcome {
        return Ok(ImportReport {
            inserted,
            failed: None,
        });
    }
    if let Err(error) = run(&session, "ROLLBACK").await {
        tracing::warn!(%error, "rollback after a failed import did not succeed");
    }
    if create_first {
        let drop = dialect.drop_sql(&ObjectRef::Relation(table.clone(), RelationKind::Table))?;
        if let Err(error) = run(&session, &drop).await {
            tracing::warn!(%error, "dropping the table of a failed import did not succeed");
        }
    }
    outcome.map(|failed| ImportReport {
        inserted: 0,
        failed,
    })
}

async fn run(session: &Arc<dyn Session>, sql: &str) -> Result<()> {
    exec::collect(session.clone(), sql).await.map(|_| ())
}

/// Runs file work on a blocking thread.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, CsvError> + Send + 'static,
) -> Result<T, CsvError> {
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|error| Err(CsvError::Io(io::Error::other(error.to_string()))))
}

fn read_error(path: &Path, error: CsvError) -> Error {
    let context = format!("cannot read {}", path.display());
    match error {
        CsvError::Io(error) => Error::io(context, error),
        CsvError::Bad(failure) => Error::io(
            context,
            io::Error::new(io::ErrorKind::InvalidData, failure.to_string()),
        ),
    }
}

/// The record the server refused: its message and code, then its detail
/// and hint on lines of their own (C14).
fn refused(line: u64, error: &Error) -> ImportFailure {
    let message = crate::error::describe(error).to_string();
    ImportFailure { line, message }
}

/// The statements that create `table` with nullable `columns` and nothing
/// else, as an import into a new table does: `(name, SQL type)` per column
/// (see [`Target::New`]).
pub fn create_table_sql(
    dialect: &dyn crate::Dialect,
    table: &TableRef,
    columns: &[(String, String)],
) -> Result<Vec<String>> {
    dialect.create_table_sql(&new_table(table, columns))
}

/// The draft of a table with nullable `columns` and nothing else.
fn new_table(table: &TableRef, columns: &[(String, String)]) -> TableDraft {
    TableDraft {
        table: table.clone(),
        comment: None,
        columns: columns
            .iter()
            .map(|(name, data_type)| ColumnDraft {
                original: None,
                name: name.clone(),
                data_type: data_type.clone(),
                nullable: true,
                default: None,
                comment: None,
                auto_increment: AutoIncrement::Off,
                on_update: None,
                invisible: false,
            })
            .collect(),
        indexes: Vec::new(),
        foreign_keys: Vec::new(),
        primary_key: Vec::new(),
    }
}

/// `(v1, v2, …)` for the mapped fields of `record`.
fn row_values(
    record: &Record,
    width: usize,
    targets: &[(usize, &ColumnInfo)],
    dialect: &dyn crate::Dialect,
) -> std::result::Result<String, ImportFailure> {
    if record.fields.len() != width {
        return Err(ImportFailure {
            line: record.line,
            message: format!(
                "{} fields where the file has {width} columns",
                record.fields.len()
            ),
        });
    }
    let mut row = String::from("(");
    for (position, (index, column)) in targets.iter().enumerate() {
        if position > 0 {
            row.push_str(", ");
        }
        let value = match &record.fields[*index] {
            None => CellValue::Null,
            Some(text) if column.kind == TypeKind::Binary => match unhex(text) {
                Some(bytes) => CellValue::Bytes(bytes),
                None => CellValue::Text(text.clone()),
            },
            Some(text) => CellValue::Text(text.clone()),
        };
        row.push_str(&dialect.literal(&value, column));
    }
    row.push(')');
    Ok(row)
}

/// The bytes of `\x` and hexadecimal digits, as binary cells are exported.
fn unhex(text: &str) -> Option<Vec<u8>> {
    let digits = text.strip_prefix("\\x")?;
    if digits.len() % 2 != 0 {
        return None;
    }
    (0..digits.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(digits.get(at..at + 2)?, 16).ok())
        .collect()
}

/// One record of a CSV file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Record {
    /// The line it starts on, from 1.
    line: u64,
    /// Its fields; `None` for NULL.
    fields: Vec<Option<String>>,
}

#[derive(Debug)]
enum CsvError {
    Io(io::Error),
    /// The file is not CSV at this line.
    Bad(ImportFailure),
}

impl From<io::Error> for CsvError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Up to `rows` records; fewer only at the end of the file.
fn read_batch<R: Read>(
    reader: &mut CsvReader<R>,
    rows: usize,
) -> std::result::Result<Vec<Record>, CsvError> {
    let mut batch = Vec::with_capacity(rows);
    while batch.len() < rows {
        match reader.next_record()? {
            Some(record) => batch.push(record),
            None => break,
        }
    }
    Ok(batch)
}

/// Reads CSV records: fields quoted with `"` may hold the delimiter, line
/// breaks and doubled quotes; lines end with `\n` or `\r\n`; blank lines
/// are skipped.
struct CsvReader<R> {
    input: BufReader<Decoder<R>>,
    delimiter: char,
    null_text: String,
    /// Lines read so far.
    line: u64,
    buffer: Vec<u8>,
}

impl CsvReader<File> {
    fn open(path: &Path, options: &CsvOptions) -> std::result::Result<Self, CsvError> {
        Ok(Self::new(File::open(path)?, options))
    }
}

impl<R: Read> CsvReader<R> {
    fn new(input: R, options: &CsvOptions) -> Self {
        Self {
            input: BufReader::with_capacity(64 * 1024, Decoder::new(input, options.encoding)),
            delimiter: options.delimiter,
            null_text: options.null_text.clone(),
            line: 0,
            buffer: Vec::new(),
        }
    }

    fn next_record(&mut self) -> std::result::Result<Option<Record>, CsvError> {
        let mut fields = Vec::new();
        let mut field = String::new();
        // The current field started with a quote / is inside its quotes.
        let mut quoted = false;
        let mut in_quotes = false;
        let mut start = None;
        loop {
            self.buffer.clear();
            if self.input.read_until(b'\n', &mut self.buffer)? == 0 {
                return match start {
                    None => Ok(None),
                    Some(line) => Err(CsvError::Bad(ImportFailure {
                        line,
                        message: "a quoted field is not closed".into(),
                    })),
                };
            }
            self.line += 1;
            let line = self.line;
            let text = std::str::from_utf8(&self.buffer).map_err(|_| {
                CsvError::Bad(ImportFailure {
                    line,
                    message: "this line is not UTF-8: choose the file's encoding".into(),
                })
            })?;
            let text = if line == 1 {
                text.strip_prefix('\u{feff}').unwrap_or(text)
            } else {
                text
            };
            if start.is_none() && matches!(text, "\n" | "\r\n") {
                continue;
            }
            start.get_or_insert(line);
            let mut chars = text.chars().peekable();
            while let Some(c) = chars.next() {
                if in_quotes {
                    if c != '"' {
                        field.push(c);
                    } else if chars.next_if_eq(&'"').is_some() {
                        field.push('"');
                    } else {
                        in_quotes = false;
                    }
                } else if c == '"' && field.is_empty() && !quoted {
                    in_quotes = true;
                    quoted = true;
                } else if c == self.delimiter {
                    fields.push(self.value(std::mem::take(&mut field), quoted));
                    quoted = false;
                } else if c == '\n' || (c == '\r' && matches!(chars.peek(), Some('\n') | None)) {
                    break;
                } else {
                    field.push(c);
                }
            }
            if !in_quotes {
                fields.push(self.value(field, quoted));
                return Ok(Some(Record {
                    line: start.unwrap_or(line),
                    fields,
                }));
            }
        }
    }

    fn value(&self, field: String, quoted: bool) -> Option<String> {
        (quoted || field != self.null_text).then_some(field)
    }
}

/// Text in `encoding` read as UTF-8.
struct Decoder<R> {
    inner: R,
    encoding: Encoding,
    /// UTF-16 bytes not decoded yet: an odd byte, or the first half of a
    /// surrogate pair.
    raw: Vec<u8>,
    decoded: Vec<u8>,
    at: usize,
    end: bool,
}

impl<R: Read> Decoder<R> {
    fn new(inner: R, encoding: Encoding) -> Self {
        Self {
            inner,
            encoding,
            raw: Vec::new(),
            decoded: Vec::new(),
            at: 0,
            end: false,
        }
    }

    fn push(&mut self, c: char) {
        self.decoded
            .extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
    }
}

impl<R: Read> Read for Decoder<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if matches!(self.encoding, Encoding::Utf8 | Encoding::Utf8Bom) {
            return self.inner.read(buffer);
        }
        while self.at == self.decoded.len() {
            if self.end {
                return Ok(0);
            }
            self.decoded.clear();
            self.at = 0;
            let mut chunk = [0; 8192];
            let read = self.inner.read(&mut chunk)?;
            self.end = read == 0;
            if self.encoding == Encoding::Latin1 {
                for &byte in &chunk[..read] {
                    self.push(char::from(byte));
                }
                continue;
            }
            self.raw.extend_from_slice(&chunk[..read]);
            let mut units: Vec<u16> = self
                .raw
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes(*pair))
                .collect();
            let mut kept = self.raw.len() % 2;
            if !self.end
                && units
                    .last()
                    .is_some_and(|unit| (0xd800..0xdc00).contains(unit))
            {
                units.pop();
                kept += 2;
            }
            for c in char::decode_utf16(units) {
                self.push(c.unwrap_or(char::REPLACEMENT_CHARACTER));
            }
            if self.end && kept > 0 {
                self.push(char::REPLACEMENT_CHARACTER);
                kept = 0;
            }
            self.raw.drain(..self.raw.len() - kept);
        }
        let count = buffer.len().min(self.decoded.len() - self.at);
        buffer[..count].copy_from_slice(&self.decoded[self.at..self.at + count]);
        self.at += count;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(delimiter: char, null_text: &str) -> CsvOptions {
        CsvOptions {
            delimiter,
            header: false,
            null_text: null_text.into(),
            encoding: Encoding::Utf8,
        }
    }

    fn parse_with(bytes: &[u8], options: &CsvOptions) -> Vec<(u64, Vec<Option<String>>)> {
        let mut reader = CsvReader::new(bytes, options);
        let mut records = Vec::new();
        while let Some(record) = reader.next_record().unwrap() {
            records.push((record.line, record.fields));
        }
        records
    }

    fn parse(text: &str) -> Vec<Vec<Option<String>>> {
        parse_with(text.as_bytes(), &options(',', ""))
            .into_iter()
            .map(|(_, fields)| fields)
            .collect()
    }

    fn some(values: &[&str]) -> Vec<Option<String>> {
        values.iter().map(|value| Some(value.to_string())).collect()
    }

    #[test]
    fn quotes_doubled_quotes_and_line_breaks() {
        assert_eq!(
            parse("a,\"b,c\",\"say \"\"hi\"\"\"\r\n\"two\r\nlines\",\"x\ny\",z\n"),
            vec![
                some(&["a", "b,c", "say \"hi\""]),
                some(&["two\r\nlines", "x\ny", "z"]),
            ]
        );
        // A quote inside an unquoted field, or after a closing one, is text.
        assert_eq!(parse("ab\"c,\"d\"e\n"), vec![some(&["ab\"c", "de"])]);
    }

    #[test]
    fn line_endings_blank_lines_and_the_last_line() {
        assert_eq!(
            parse("a,b\r\n\r\nc,d\n\ne,f"),
            vec![some(&["a", "b"]), some(&["c", "d"]), some(&["e", "f"])]
        );
        assert_eq!(parse(""), Vec::<Vec<Option<String>>>::new());
        // A carriage return inside a line is text.
        assert_eq!(parse("a\rb\n"), vec![some(&["a\rb"])]);
    }

    #[test]
    fn null_text_unquoted_is_null_quoted_is_a_value() {
        let records = parse("1,,\"\",x\n");
        assert_eq!(
            records,
            vec![vec![
                Some("1".into()),
                None,
                Some(String::new()),
                Some("x".into())
            ]]
        );
        let records = parse_with(b"NULL;\"NULL\";;\n", &options(';', "NULL"));
        assert_eq!(
            records[0].1,
            vec![
                None,
                Some("NULL".into()),
                Some(String::new()),
                Some(String::new())
            ]
        );
    }

    #[test]
    fn records_know_the_line_they_start_on() {
        let records = parse_with(b"h1,h2\n\"a\nb\nc\",1\n\n2,3\n", &options(',', ""));
        let lines: Vec<u64> = records.iter().map(|(line, _)| *line).collect();
        assert_eq!(lines, [1, 2, 6]);
    }

    #[test]
    fn other_delimiters() {
        for delimiter in ['\t', ';', '|', '\u{2192}'] {
            let text = format!("a{delimiter}\"b{delimiter}c\"{delimiter}d\n");
            assert_eq!(
                parse_with(text.as_bytes(), &options(delimiter, "")),
                vec![(1, some(&["a", &format!("b{delimiter}c"), "d"]))]
            );
        }
        // A comma is text when it is not the delimiter.
        assert_eq!(
            parse_with(b"a,b\tc\n", &options('\t', "")),
            vec![(1, some(&["a,b", "c"]))]
        );
    }

    #[test]
    fn an_unclosed_quote_and_bad_utf8_name_their_line() {
        let mut reader = CsvReader::new(&b"a\n\"open\nmore\n"[..], &options(',', ""));
        reader.next_record().unwrap();
        match reader.next_record() {
            Err(CsvError::Bad(failure)) => {
                assert_eq!(failure.line, 2);
                assert!(failure.message.contains("not closed"), "{failure}");
            }
            other => panic!("{other:?}"),
        }
        let mut reader = CsvReader::new(&b"a\nb\xff\n"[..], &options(',', ""));
        reader.next_record().unwrap();
        match reader.next_record() {
            Err(CsvError::Bad(failure)) => assert_eq!(failure.line, 2),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn encodings_and_byte_order_marks() {
        let text = "\u{feff}caf\u{e9},\u{1f600}\r\nx,y\n";
        let expected = vec![
            (1, some(&["caf\u{e9}", "\u{1f600}"])),
            (2, some(&["x", "y"])),
        ];
        let utf16: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        for (encoding, bytes) in [
            (Encoding::Utf8, text.as_bytes().to_vec()),
            (Encoding::Utf8Bom, text.as_bytes().to_vec()),
            (Encoding::Utf16Le, utf16),
        ] {
            let options = CsvOptions {
                encoding,
                ..options(',', "")
            };
            assert_eq!(parse_with(&bytes, &options), expected, "{encoding:?}");
        }
        let latin1 = CsvOptions {
            encoding: Encoding::Latin1,
            ..options(',', "")
        };
        assert_eq!(
            parse_with(b"caf\xe9,\xff\n", &latin1),
            vec![(1, some(&["caf\u{e9}", "\u{ff}"]))]
        );
    }

    /// Hands out one byte per read, so UTF-16 units and surrogate pairs are
    /// split between reads.
    struct OneByte<'a>(&'a [u8]);

    impl Read for OneByte<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let Some((first, rest)) = self.0.split_first() else {
                return Ok(0);
            };
            buffer[0] = *first;
            self.0 = rest;
            Ok(1)
        }
    }

    #[test]
    fn utf16_split_between_reads() {
        let text = "\u{1f600}a\u{20ac}\n";
        let bytes: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let mut decoded = String::new();
        Decoder::new(OneByte(&bytes), Encoding::Utf16Le)
            .read_to_string(&mut decoded)
            .unwrap();
        assert_eq!(decoded, text);
        // An odd byte at the end is a replacement character.
        let mut decoded = String::new();
        Decoder::new(OneByte(b"a\x00b"), Encoding::Utf16Le)
            .read_to_string(&mut decoded)
            .unwrap();
        assert_eq!(decoded, "a\u{fffd}");
    }

    #[test]
    fn kinds_are_guessed_from_every_value() {
        let cases: [(&[&str], TypeKind); 16] = [
            (&["1", "-42", "0", "9223372036854775807"], TypeKind::Integer),
            (&["1", "2.5", "-0.25", "1e3"], TypeKind::Decimal),
            (&["99999999999999999999"], TypeKind::Decimal),
            (&["00123", "1"], TypeKind::Text),
            (&["01.5"], TypeKind::Text),
            (&["t", "FALSE", "true"], TypeKind::Boolean),
            (&["yes", "no"], TypeKind::Text),
            (&["0", "1"], TypeKind::Integer),
            (&["2024-02-29", "1999-12-31"], TypeKind::Date),
            (&["2024-13-01"], TypeKind::Text),
            (
                &["2024-02-29 10:00", "2024-02-29T10:00:59.123", "2024-01-01"],
                TypeKind::DateTime,
            ),
            (&["2024-02-29 10:00:00+02"], TypeKind::Text),
            (&["0f8fad5b-d9cb-469f-a165-70867728950e"], TypeKind::Uuid),
            (&["{\"a\": 1}", "[1, 2]"], TypeKind::Json),
            (&["{broken", "[1]"], TypeKind::Text),
            (&["1", "a"], TypeKind::Text),
        ];
        for (values, kind) in cases {
            assert_eq!(guess_kind(values, ""), kind, "{values:?}");
        }
    }

    #[test]
    fn null_text_is_left_out_of_the_guess() {
        assert_eq!(guess_kind(&["", "1", ""], ""), TypeKind::Integer);
        assert_eq!(guess_kind(&["NULL", "t"], "NULL"), TypeKind::Boolean);
        // An empty value that is not the NULL text is not a number.
        assert_eq!(guess_kind(&["", "1"], "NULL"), TypeKind::Text);
        assert_eq!(guess_kind(&["", ""], ""), TypeKind::Text);
        assert_eq!(guess_kind(&[], ""), TypeKind::Text);
    }

    #[test]
    fn preview_reads_the_header_and_the_first_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("people.csv");
        std::fs::write(
            &path,
            "id,name,,born\n1,Ada,x,1815-12-10\n2,,y,1906-12-09\n3,Bob,z,NULL,extra\n4,Cy,w,\n",
        )
        .unwrap();
        let options = CsvOptions {
            header: true,
            ..options(',', "NULL")
        };
        let preview = preview(&path, &options, 3).unwrap();
        assert_eq!(
            preview.headers,
            ["id", "name", "column_3", "born", "column_5"]
        );
        assert_eq!(preview.rows.len(), 3);
        let mut third = some(&["3", "Bob", "z", "", "extra"]);
        third[3] = None;
        assert_eq!(preview.rows[2], third);
        assert_eq!(
            preview.guessed,
            [
                TypeKind::Integer,
                TypeKind::Text,
                TypeKind::Text,
                TypeKind::Date,
                TypeKind::Text
            ]
        );
        let missing = super::preview(&dir.path().join("missing.csv"), &options, 3).unwrap_err();
        assert!(missing.to_string().contains("missing.csv"), "{missing}");
    }

    #[test]
    fn preview_rows_follow_the_null_rule_of_the_import() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nulls.csv");
        std::fs::write(&path, "a,b,c\n\"\",,\"NULL\"\nNULL,x,\n").unwrap();
        // With an empty NULL text: `""` is the empty string, a bare empty
        // field is NULL.
        let header = |null_text| CsvOptions {
            header: true,
            ..options(',', null_text)
        };
        let rows = preview(&path, &header(""), 10).unwrap().rows;
        assert_eq!(rows[0], [Some(String::new()), None, Some("NULL".into())]);
        // With `NULL`: only the bare word is NULL.
        let rows = preview(&path, &header("NULL"), 10).unwrap().rows;
        assert_eq!(rows[0], some(&["", "", "NULL"]));
        assert_eq!(rows[1], [None, Some("x".into()), Some(String::new())]);
    }

    #[test]
    fn records_are_counted_as_the_reader_reads_them() {
        let dir = tempfile::tempdir().unwrap();
        let file = |name: &str, bytes: &[u8]| {
            let path = dir.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        };
        let with_header = |header, encoding| CsvOptions {
            header,
            encoding,
            ..options(',', "")
        };
        let text = "id,note\r\n1,\"two\nlines\"\r\n\r\n2,\"a \"\"quote\"\"\"\n3,last";
        let path = file("a.csv", text.as_bytes());
        assert_eq!(
            count_records(
                &path,
                &with_header(true, Encoding::Utf8),
                &AtomicBool::new(false)
            )
            .unwrap(),
            3
        );
        assert_eq!(
            count_records(
                &path,
                &with_header(false, Encoding::Utf8),
                &AtomicBool::new(false)
            )
            .unwrap(),
            4
        );
        // UTF-16 with its byte order mark: the same records.
        let wide: Vec<u8> = "\u{feff}id,n\u{e9}\n1,\"x\ny\"\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        let path = file("b.csv", &wide);
        assert_eq!(
            count_records(
                &path,
                &with_header(true, Encoding::Utf16Le),
                &AtomicBool::new(false)
            )
            .unwrap(),
            1
        );
        let path = file("c.csv", b"");
        assert_eq!(
            count_records(
                &path,
                &with_header(true, Encoding::Utf8),
                &AtomicBool::new(false)
            )
            .unwrap(),
            0
        );
        // What stops an import stops the count, with its line.
        let path = file("d.csv", b"a\n1\n\"open\n");
        let error = count_records(
            &path,
            &with_header(true, Encoding::Utf8),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(error.to_string().contains("line 3"), "{error}");
    }

    /// C3: a scan whose flag is set stops at the next record (the preview's count, replaced by
    /// another when a field changes), with `Cancelled`; an unset flag reads to the end.
    #[test]
    fn a_stopped_count_ends_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.csv");
        std::fs::write(&path, "id\n".to_string() + &"1\n".repeat(10_000)).unwrap();
        let header = CsvOptions {
            header: true,
            ..options(',', "")
        };
        let stopped = AtomicBool::new(true);
        assert!(matches!(
            count_records(&path, &header, &stopped),
            Err(Error::Cancelled)
        ));
        assert_eq!(
            count_records(&path, &header, &AtomicBool::new(false)).unwrap(),
            10_000
        );
    }

    #[test]
    fn a_new_table_is_created_with_nullable_columns_only() {
        let columns = [
            ("id".to_string(), "bigint".to_string()),
            ("name".to_string(), "text".to_string()),
        ];
        assert_eq!(
            create_table_sql(
                &crate::dialect::test_support::AnsiDialect,
                &TableRef::new("s", "t"),
                &columns
            )
            .unwrap(),
            ["CREATE TABLE \"s\".\"t\" (\n    \"id\" bigint,\n    \"name\" text\n)"]
        );
    }

    #[test]
    fn binary_cells_come_back_from_hex() {
        assert_eq!(unhex("\\xdead00"), Some(vec![0xde, 0xad, 0]));
        assert_eq!(unhex("\\x"), Some(vec![]));
        assert_eq!(unhex("\\xabc"), None);
        assert_eq!(unhex("dead"), None);
        assert_eq!(unhex("\\xzz"), None);
    }
}
