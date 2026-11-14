//! Exporting what a statement returns to a file, as it arrives.
//!
//! Rows go from the driver to a writer thread through bounded channels:
//! when the disk is slower than the server, the driver stops reading and
//! the server waits. Memory holds a few batches, whatever the size of the
//! result.

use std::cell::Cell;
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::mpsc;

use crate::driver::Session;
use crate::error::{Error, Result};
use crate::exec::{self, ExecEvent, RowLimit};
use crate::export::{ExportFormat, Exporter};
use crate::result::{Column, RowBatch};
use crate::xlsx::{XlsxOptions, XlsxWriter};

/// The character encoding of a text file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// UTF-8 without a byte order mark.
    Utf8,
    /// UTF-8 starting with a byte order mark (`EF BB BF`), which some
    /// spreadsheets need to read UTF-8.
    Utf8Bom,
    /// UTF-16, little-endian, starting with a byte order mark (`FF FE`).
    Utf16Le,
    /// ISO-8859-1. On export, characters it cannot hold are written `?`.
    Latin1,
}

/// What to export, how, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportJob {
    /// One statement. Its first result set is exported.
    pub sql: String,
    /// What rows are written as.
    pub format: ExportFormat,
    /// The file's encoding; a workbook ([`ExportFormat::Xlsx`]) has its own.
    pub encoding: Encoding,
    /// The file to write. Replaced only once the export succeeded.
    pub path: PathBuf,
}

/// How far an export got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExportProgress {
    /// Rows written.
    pub rows: u64,
    /// Bytes written, after encoding. A workbook is put together at the
    /// end: 0 until then.
    pub bytes: u64,
    /// Workbook only: sheets, and text cells cut at Excel's 32,767
    /// characters.
    pub sheets: u32,
    pub truncated: u64,
}

/// Progress is reported after about this many rows.
const PROGRESS_ROWS: u64 = 5_000;
/// How often a waiting export looks at its cancel flag.
const CANCEL_POLL: Duration = Duration::from_millis(100);

/// What the async side hands the writer thread.
enum Chunk {
    Columns(Arc<[Column]>),
    Rows(RowBatch),
    /// The result is complete: finish the file and put it in place.
    Done,
}

/// Runs `job.sql` and writes every row of its first result set to
/// `job.path` as it arrives.
///
/// The file is written as `<path>.tmp` and renamed when complete, so an
/// existing file is replaced only by a whole export. Progress is reported
/// about every 5,000 rows, from the writer thread. Setting `cancel` stops
/// the export (and the statement on the server) and returns
/// [`Error::Cancelled`]. On any failure the partial file is removed: a
/// statement without a result set, a server error, or a file that cannot
/// be written, named in the error.
///
/// Must be called from within the Tokio runtime.
pub async fn export_to_file(
    session: Arc<dyn Session>,
    job: ExportJob,
    progress: impl Fn(ExportProgress) + Send + 'static,
    cancel: Arc<AtomicBool>,
) -> Result<ExportProgress> {
    let ExportJob {
        sql,
        format,
        encoding,
        path,
    } = job;
    let (sender, receiver) = mpsc::channel(4);
    let writer = tokio::task::spawn_blocking({
        let session = session.clone();
        move || {
            let tmp = tmp_path(&path);
            let target = Target {
                tmp: &tmp,
                path: &path,
                format,
                encoding,
            };
            let written = write_file(target, session.as_ref(), receiver, progress);
            if written.is_err() {
                let _ = fs::remove_file(&tmp);
            }
            written
        }
    });
    let fed = feed(session, sql, &sender, &cancel).await;
    if fed.is_ok() {
        // A writer that already failed has dropped its end; its error
        // follows.
        let _ = sender.send(Chunk::Done).await;
    }
    drop(sender);
    let written = writer.await.unwrap_or_else(|error| {
        Err(Error::io(
            "writing the export",
            io::Error::other(error.to_string()),
        ))
    });
    match (fed, written) {
        // The writer's own failure explains why feeding it stopped.
        (_, Err(error)) if !matches!(error, Error::Cancelled) => Err(error),
        (Err(error), _) => Err(error),
        (Ok(()), written) => written,
    }
}

/// `<path>.tmp`, next to the file.
fn tmp_path(path: &Path) -> PathBuf {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    PathBuf::from(tmp)
}

/// Runs the statement and passes its first result set to the writer.
/// Dropping the execution on return cancels the statement if it still runs.
async fn feed(
    session: Arc<dyn Session>,
    sql: String,
    sender: &mpsc::Sender<Chunk>,
    cancel: &AtomicBool,
) -> Result<()> {
    let mut execution = exec::run(session, sql, RowLimit::All);
    let mut tick = tokio::time::interval(CANCEL_POLL);
    let mut columns = false;
    let mut first_done = false;
    loop {
        if cancel.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        let event = tokio::select! {
            event = execution.next() => event,
            _ = tick.tick() => continue,
            // The writer failed (the file cannot be created, for one):
            // its error is the one to report.
            () = sender.closed() => return Err(Error::Cancelled),
        };
        let chunk = match event {
            Some(ExecEvent::Columns(found)) if !columns => {
                columns = true;
                Chunk::Columns(found)
            }
            Some(ExecEvent::Rows(batch)) if !first_done => Chunk::Rows(batch),
            Some(ExecEvent::Complete { .. }) if columns => {
                first_done = true;
                continue;
            }
            Some(ExecEvent::Finished(result)) => {
                result?;
                return if columns {
                    Ok(())
                } else {
                    Err(Error::io(
                        "export",
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "the statement returns no rows to export",
                        ),
                    ))
                };
            }
            None => return Ok(()),
            Some(_) => continue,
        };
        if sender.send(chunk).await.is_err() {
            // The writer failed; its error is the one to report.
            return Err(Error::Cancelled);
        }
    }
}

/// Where and how the writer thread writes.
struct Target<'a> {
    tmp: &'a Path,
    path: &'a Path,
    format: ExportFormat,
    encoding: Encoding,
}

/// The writer thread: creates the temporary file, writes what arrives,
/// and renames the file once told the result is complete. Returns
/// [`Error::Cancelled`] when the channel closes before that.
fn write_file(
    target: Target<'_>,
    session: &dyn Session,
    mut receiver: mpsc::Receiver<Chunk>,
    progress: impl Fn(ExportProgress),
) -> Result<ExportProgress> {
    let cannot_write =
        |error: io::Error| Error::io(format!("cannot write {}", target.path.display()), error);
    let write_error = |error: Error| match error {
        Error::Io { source, .. } => cannot_write(source),
        error => error,
    };
    let file = File::create(target.tmp).map_err(cannot_write)?;
    if let ExportFormat::Xlsx(options) = &target.format {
        drop(file);
        return write_workbook(&target, options, receiver, progress).map_err(write_error);
    }
    let bytes = Cell::new(0);
    let mut out = Encoder::new(BufWriter::new(file), target.encoding, &bytes);
    out.start().map_err(cannot_write)?;
    let columns = match receiver.blocking_recv() {
        Some(Chunk::Columns(columns)) => columns,
        _ => return Err(Error::Cancelled),
    };
    let mut exporter = Exporter::new(&mut out, target.format.clone(), &columns, session.dialect());
    let mut rows = 0;
    let mut reported = 0;
    loop {
        match receiver.blocking_recv() {
            Some(Chunk::Rows(batch)) => {
                let mut cells = Vec::with_capacity(batch.column_count());
                for row in 0..batch.row_count() {
                    cells.clear();
                    cells.extend((0..batch.column_count()).map(|column| batch.cell(row, column)));
                    exporter.write_row(&cells).map_err(write_error)?;
                }
                rows += batch.row_count() as u64;
                if rows - reported >= PROGRESS_ROWS {
                    reported = rows;
                    progress(ExportProgress {
                        rows,
                        bytes: bytes.get(),
                        ..ExportProgress::default()
                    });
                }
            }
            Some(Chunk::Done) => break,
            Some(Chunk::Columns(_)) | None => return Err(Error::Cancelled),
        }
    }
    exporter.finish().map_err(write_error)?;
    drop(out);
    move_into_place(&target)?;
    let done = ExportProgress {
        rows,
        bytes: bytes.get(),
        ..ExportProgress::default()
    };
    progress(done);
    Ok(done)
}

/// Renames the complete temporary file to the target.
fn move_into_place(target: &Target<'_>) -> Result<()> {
    fs::rename(target.tmp, target.path).map_err(|error| {
        Error::io(
            format!("cannot move the export to {}", target.path.display()),
            error,
        )
    })
}

/// [`write_file`] for a workbook: rows go to the worksheets' temporary
/// files as they arrive, and the workbook is saved to the temporary file
/// once the result is complete. A cancelled export saves nothing; the
/// worksheets' temporary files go with the writer.
fn write_workbook(
    target: &Target<'_>,
    options: &XlsxOptions,
    mut receiver: mpsc::Receiver<Chunk>,
    progress: impl Fn(ExportProgress),
) -> Result<ExportProgress> {
    let columns = match receiver.blocking_recv() {
        Some(Chunk::Columns(columns)) => columns,
        _ => return Err(Error::Cancelled),
    };
    let mut writer = XlsxWriter::new(&columns, options)?;
    let mut reported = 0;
    let mut rows = 0;
    loop {
        match receiver.blocking_recv() {
            Some(Chunk::Rows(batch)) => {
                let mut cells = Vec::with_capacity(batch.column_count());
                for row in 0..batch.row_count() {
                    cells.clear();
                    cells.extend((0..batch.column_count()).map(|column| batch.cell(row, column)));
                    writer.write_row(&cells)?;
                }
                rows += batch.row_count() as u64;
                if rows - reported >= PROGRESS_ROWS {
                    reported = rows;
                    progress(ExportProgress {
                        rows,
                        ..ExportProgress::default()
                    });
                }
            }
            Some(Chunk::Done) => break,
            Some(Chunk::Columns(_)) | None => return Err(Error::Cancelled),
        }
    }
    let report = writer.finish(target.tmp)?;
    move_into_place(target)?;
    let done = ExportProgress {
        rows: report.rows,
        bytes: fs::metadata(target.path).map_or(0, |meta| meta.len()),
        sheets: report.sheets,
        truncated: report.truncated,
    };
    progress(done);
    Ok(done)
}

/// Turns the UTF-8 an [`Exporter`] writes into the file's encoding, and
/// counts the bytes that reach `W`.
struct Encoder<'a, W: Write> {
    inner: W,
    encoding: Encoding,
    bytes: &'a Cell<u64>,
    /// The start of a character split between two writes.
    pending: Vec<u8>,
    /// The encoded text, reused from write to write.
    encoded: Vec<u8>,
}

impl<'a, W: Write> Encoder<'a, W> {
    fn new(inner: W, encoding: Encoding, bytes: &'a Cell<u64>) -> Self {
        Self {
            inner,
            encoding,
            bytes,
            pending: Vec::new(),
            encoded: Vec::new(),
        }
    }

    /// Writes the byte order mark, if the encoding has one.
    fn start(&mut self) -> io::Result<()> {
        let mark: &[u8] = match self.encoding {
            Encoding::Utf8Bom => b"\xef\xbb\xbf",
            Encoding::Utf16Le => b"\xff\xfe",
            Encoding::Utf8 | Encoding::Latin1 => b"",
        };
        self.put(mark)
    }

    fn put(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.inner.write_all(bytes)?;
        self.bytes.set(self.bytes.get() + bytes.len() as u64);
        Ok(())
    }
}

impl<W: Write> Write for Encoder<'_, W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if matches!(self.encoding, Encoding::Utf8 | Encoding::Utf8Bom) {
            self.put(buffer)?;
            return Ok(buffer.len());
        }
        self.pending.extend_from_slice(buffer);
        let complete = match std::str::from_utf8(&self.pending) {
            Ok(text) => text.len(),
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(error) => return Err(io::Error::new(io::ErrorKind::InvalidData, error)),
        };
        let text = std::str::from_utf8(&self.pending[..complete])
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let mut encoded = std::mem::take(&mut self.encoded);
        encoded.clear();
        for c in text.chars() {
            match self.encoding {
                Encoding::Latin1 => encoded.push(u8::try_from(c).unwrap_or(b'?')),
                _ => {
                    for unit in c.encode_utf16(&mut [0; 2]) {
                        encoded.extend_from_slice(&unit.to_le_bytes());
                    }
                }
            }
        }
        self.pending.drain(..complete);
        let written = self.put(&encoded);
        self.encoded = encoded;
        written.map(|()| buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(encoding: Encoding, writes: &[&[u8]]) -> (Vec<u8>, u64) {
        let bytes = Cell::new(0);
        let mut out = Vec::new();
        let mut encoder = Encoder::new(&mut out, encoding, &bytes);
        encoder.start().unwrap();
        for write in writes {
            encoder.write_all(write).unwrap();
        }
        encoder.flush().unwrap();
        drop(encoder);
        (out, bytes.get())
    }

    #[test]
    fn every_encoding_writes_its_mark_and_its_characters() {
        let text = "a\u{e9}\u{20ac}\u{1f600}\n";
        let utf16: Vec<u8> = [0xfeff_u16]
            .into_iter()
            .chain(text.encode_utf16())
            .flat_map(u16::to_le_bytes)
            .collect();
        let cases: [(Encoding, Vec<u8>); 4] = [
            (Encoding::Utf8, text.as_bytes().to_vec()),
            (
                Encoding::Utf8Bom,
                [b"\xef\xbb\xbf".as_slice(), text.as_bytes()].concat(),
            ),
            (Encoding::Utf16Le, utf16),
            // What Latin-1 cannot hold is a question mark.
            (Encoding::Latin1, b"a\xe9??\n".to_vec()),
        ];
        for (encoding, expected) in cases {
            let (written, bytes) = encode(encoding, &[text.as_bytes()]);
            assert_eq!(written, expected, "{encoding:?}");
            assert_eq!(bytes, expected.len() as u64, "{encoding:?}");
        }
    }

    #[test]
    fn a_character_split_between_writes_is_encoded_once_whole() {
        let text = "\u{20ac}\u{1f600}".as_bytes();
        for split in 1..text.len() {
            let (written, _) = encode(Encoding::Utf16Le, &[&text[..split], &text[split..]]);
            let expected: Vec<u8> = "\u{feff}\u{20ac}\u{1f600}"
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect();
            assert_eq!(written, expected, "split at {split}");
        }
    }

    #[test]
    fn the_temporary_file_sits_next_to_the_target() {
        assert_eq!(
            tmp_path(Path::new("/x/out.csv")),
            PathBuf::from("/x/out.csv.tmp")
        );
    }
}
