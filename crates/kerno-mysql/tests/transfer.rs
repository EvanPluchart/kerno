//! Export to a file and CSV import on MySQL and MariaDB: every format
//! written from a real result and read back, cancel and failures, the
//! 1,000,000-row export and a 100,000-row import. Skipped with a message
//! when the containers are down.
//!
//! Throughput is printed on stderr: run with `--nocapture` to see it.

mod common;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{engines, exec_ok, rows, run, scalar};
use kerno_core::export::{DelimitedOptions, ExportFormat, LineEnding};
use kerno_core::export_file::{Encoding, ExportJob, ExportProgress, export_to_file};
use kerno_core::import::{
    CsvOptions, ImportFailure, ImportJob, ImportReport, Target, import_csv, preview,
};
use kerno_core::schema::TableRef;
use kerno_core::sql::split_statements;
use kerno_core::{Engine, Error, Result, Session, TxState, TypeKind};

/// Runs the `cleanup` statements, then `body`, then `cleanup` again whether
/// `body` passed or panicked.
async fn cleaning_up(
    session: &Arc<dyn Session>,
    cleanup: &[String],
    body: impl Future<Output = ()> + Send + 'static,
) {
    for sql in cleanup {
        exec_ok(session, sql).await;
    }
    let outcome = tokio::spawn(body).await;
    let mut failed = Vec::new();
    for sql in cleanup {
        if let Err(error) = run(session, sql).await {
            failed.push(format!("{sql}: {error}"));
        }
    }
    if let Err(error) = outcome {
        std::panic::resume_unwind(error.into_panic());
    }
    assert!(failed.is_empty(), "cleanup failed: {failed:#?}");
}

/// Runs `body` on each engine in a scratch database dropped before and
/// after.
async fn in_scratch<F: Future<Output = ()> + Send + 'static>(
    test: &str,
    body: impl Fn(Arc<dyn Session>, Engine, String) -> F,
) {
    for engine in engines() {
        let session = common::session(engine).await;
        let database = common::scratch_database(test);
        let cleanup = [format!("DROP DATABASE IF EXISTS {database}")];
        let setup = format!("CREATE DATABASE {database} CHARACTER SET utf8mb4");
        let body = body(session.clone(), engine, database);
        let future = {
            let session = session.clone();
            async move {
                exec_ok(&session, &setup).await;
                body.await;
            }
        };
        cleaning_up(&session, &cleanup, future).await;
    }
}

async fn export(
    session: &Arc<dyn Session>,
    sql: &str,
    format: ExportFormat,
    encoding: Encoding,
    path: &Path,
) -> Result<ExportProgress> {
    let job = ExportJob {
        sql: sql.into(),
        format,
        encoding,
        path: path.into(),
    };
    export_to_file(session.clone(), job, |_| {}, Arc::default()).await
}

async fn import(
    session: &Arc<dyn Session>,
    job: ImportJob,
    progress: impl Fn(u64) + Send + 'static,
    cancel: Arc<AtomicBool>,
) -> Result<ImportReport> {
    let columns = match &job.target {
        Target::Existing(table) => session.table_structure(table).await?.columns,
        Target::New { .. } => Vec::new(),
    };
    import_csv(session.clone(), columns, job, progress, cancel).await
}

fn job(path: &Path, options: CsvOptions, target: Target, mapping: &[Option<&str>]) -> ImportJob {
    ImportJob {
        path: path.into(),
        options,
        target,
        mapping: mapping.iter().map(|name| name.map(String::from)).collect(),
        batch_rows: 500,
    }
}

fn csv_options(header: bool) -> CsvOptions {
    CsvOptions {
        delimiter: ',',
        header,
        null_text: String::new(),
        encoding: Encoding::Utf8,
    }
}

fn tmp_of(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.tmp", path.display()))
}

/// A table with every awkward value: NULL, the empty string, quotes, the
/// delimiters, line breaks, unicode, the word NULL, spaces at the ends,
/// binary and JSON.
async fn source_table(session: &Arc<dyn Session>, database: &str) {
    exec_ok(
        session,
        &format!(
            "CREATE TABLE {database}.src (id INT PRIMARY KEY, t TEXT, n DECIMAL(10,2), \
             flag TINYINT(1), d DATE, b BLOB, j JSON)"
        ),
    )
    .await;
    exec_ok(
        session,
        &format!(
            "INSERT INTO {database}.src VALUES \
             (1, 'plain', 12.50, 1, '2024-02-29', X'deadbeef', '{{\"a\": [1, \"x\"]}}'), \
             (2, NULL, NULL, NULL, NULL, NULL, NULL), \
             (3, '', -0.5, 0, '1999-12-31', X'', '[]'), \
             (4, CONCAT('O''Brien, \"Bob\"; tab', CHAR(9), 'and pipe |'), 1e3, 1, NULL, X'00ff2c22', '\"s\"'), \
             (5, CONCAT('two', CHAR(13), CHAR(10), 'lines', CHAR(10), 'and', CHAR(13), 'more'), \
                 0, NULL, '2000-01-01', NULL, '{{}}'), \
             (6, 'caf\u{e9} \u{1f600} \u{fc}n\u{ef}', 1, NULL, NULL, NULL, NULL), \
             (7, 'NULL', NULL, NULL, NULL, NULL, NULL), \
             (8, ' padded ', NULL, NULL, NULL, NULL, NULL)"
        ),
    )
    .await;
    exec_ok(
        session,
        &format!("CREATE TABLE {database}.dst LIKE {database}.src"),
    )
    .await;
}

async fn table_rows(session: &Arc<dyn Session>, table: &str) -> Vec<Vec<Option<String>>> {
    rows(session, &format!("SELECT * FROM {table} ORDER BY id")).await
}

#[tokio::test]
async fn delimited_files_round_trip_through_an_import() {
    in_scratch("transfer_round_trip", |session, _, database| async move {
        source_table(&session, &database).await;
        let dir = tempfile::tempdir().unwrap();
        let semicolons = DelimitedOptions {
            delimiter: ';',
            header: false,
            null_text: "NULL".into(),
            line_ending: LineEnding::Lf,
        };
        let variants = [
            (DelimitedOptions::csv(), Encoding::Utf8),
            (DelimitedOptions::tsv(), Encoding::Utf8Bom),
            (semicolons, Encoding::Utf16Le),
        ];
        let expected = table_rows(&session, &format!("{database}.src")).await;
        let names = ["id", "t", "n", "flag", "d", "b", "j"].map(Some);
        for (options, encoding) in variants {
            let path = dir.path().join("src.csv");
            let written = export(
                &session,
                &format!("SELECT * FROM {database}.src ORDER BY id"),
                ExportFormat::Delimited(options.clone()),
                encoding,
                &path,
            )
            .await
            .unwrap();
            assert_eq!(written.rows, 8);
            assert_eq!(written.bytes, std::fs::metadata(&path).unwrap().len());

            exec_ok(&session, &format!("TRUNCATE {database}.dst")).await;
            let csv = CsvOptions {
                delimiter: options.delimiter,
                header: options.header,
                null_text: options.null_text.clone(),
                encoding,
            };
            let target = Target::Existing(TableRef::new(database.clone(), "dst"));
            let report = import(
                &session,
                job(&path, csv, target, &names),
                |_| {},
                Arc::default(),
            )
            .await
            .unwrap();
            assert_eq!(
                report,
                ImportReport {
                    inserted: 8,
                    failed: None
                },
                "{options:?} {encoding:?}"
            );
            assert_eq!(
                table_rows(&session, &format!("{database}.dst")).await,
                expected,
                "{options:?} {encoding:?}"
            );
        }
    })
    .await;
}

#[tokio::test]
async fn json_markdown_and_insert_files_hold_every_row() {
    in_scratch("transfer_formats", |session, _, database| async move {
        source_table(&session, &database).await;
        let dir = tempfile::tempdir().unwrap();
        let query = format!("SELECT * FROM {database}.src ORDER BY id");
        let expected = table_rows(&session, &format!("{database}.src")).await;
        let path = dir.path().join("out");

        // JSON: the server reads the documents back.
        export(
            &session,
            &query,
            ExportFormat::JsonArray,
            Encoding::Utf8,
            &path,
        )
        .await
        .unwrap();
        let doc = session
            .dialect()
            .quote_string(&std::fs::read_to_string(&path).unwrap());
        assert_eq!(
            rows(
                &session,
                &format!(
                    "SELECT JSON_LENGTH({doc}), JSON_UNQUOTE(JSON_EXTRACT({doc}, '$[4].t')), \
                     JSON_EXTRACT({doc}, '$[1].t'), JSON_EXTRACT({doc}, '$[0].n') = 12.5"
                )
            )
            .await,
            [[
                Some("8".to_string()),
                expected[4][1].clone(),
                Some("null".into()),
                Some("1".into())
            ]]
        );
        export(
            &session,
            &query,
            ExportFormat::NdJson,
            Encoding::Utf8,
            &path,
        )
        .await
        .unwrap();
        let lines = std::fs::read_to_string(&path).unwrap();
        assert_eq!(lines.lines().count(), 8);
        for (line, row) in lines.lines().zip(&expected) {
            let line = session.dialect().quote_string(line);
            assert_eq!(
                rows(
                    &session,
                    &format!("SELECT JSON_UNQUOTE(JSON_EXTRACT({line}, '$.id'))")
                )
                .await,
                [[row[0].clone()]]
            );
        }

        // SQL INSERT: the statements fill the other table.
        let insert = ExportFormat::SqlInsert {
            table: TableRef::new(database.clone(), "dst"),
        };
        export(&session, &query, insert, Encoding::Utf8, &path)
            .await
            .unwrap();
        let script = std::fs::read_to_string(&path).unwrap();
        let statements = split_statements(&script, session.dialect().lex_rules());
        assert_eq!(statements.len(), 8);
        for statement in &statements {
            exec_ok(&session, statement.text(&script)).await;
        }
        assert_eq!(
            table_rows(&session, &format!("{database}.dst")).await,
            expected
        );

        // Markdown: a header, a separator and one line per row.
        export(
            &session,
            &query,
            ExportFormat::Markdown,
            Encoding::Utf8,
            &path,
        )
        .await
        .unwrap();
        let markdown = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = markdown.lines().collect();
        assert_eq!(lines.len(), 10);
        assert_eq!(lines[0], "| id | t | n | flag | d | b | j |");
        assert!(
            lines[6].contains("two<br>lines<br>and<br>more"),
            "{}",
            lines[6]
        );
    })
    .await;
}

#[tokio::test]
async fn cancel_stops_the_statement_and_removes_the_file() {
    for engine in engines() {
        let session = common::session(engine).await;
        let dir = tempfile::tempdir().unwrap();

        // While the server has sent nothing yet.
        let path = dir.path().join("sleep.csv");
        let cancel = Arc::new(AtomicBool::new(false));
        let job = ExportJob {
            sql: "SELECT SLEEP(30)".into(),
            format: ExportFormat::Delimited(DelimitedOptions::csv()),
            encoding: Encoding::Utf8,
            path: path.clone(),
        };
        let started = Instant::now();
        let export = tokio::spawn(export_to_file(session.clone(), job, |_| {}, cancel.clone()));
        tokio::time::sleep(Duration::from_millis(300)).await;
        cancel.store(true, Ordering::Release);
        let outcome = export.await.unwrap();
        assert!(matches!(outcome, Err(Error::Cancelled)), "{outcome:?}");
        assert!(!path.exists() && !tmp_of(&path).exists());
        // The statement was stopped on the server: the session answers.
        assert_eq!(scalar(&session, "SELECT 1").await, "1");
        assert!(started.elapsed() < Duration::from_secs(10));

        // While rows stream: cancelled from the first progress report.
        let path = dir.path().join("big.csv");
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let job = ExportJob {
            sql: "SELECT * FROM kerno_analytics.big_table".into(),
            format: ExportFormat::Delimited(DelimitedOptions::csv()),
            encoding: Encoding::Utf8,
            path: path.clone(),
        };
        let outcome = export_to_file(
            session.clone(),
            job,
            move |_| flag.store(true, Ordering::Release),
            cancel,
        )
        .await;
        assert!(matches!(outcome, Err(Error::Cancelled)), "{outcome:?}");
        assert!(!path.exists() && !tmp_of(&path).exists());
        assert_eq!(scalar(&session, "SELECT 2").await, "2");
    }
}

#[tokio::test]
async fn failures_remove_the_file_and_say_why() {
    for engine in engines() {
        let session = common::session(engine).await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.csv");
        let csv = || ExportFormat::Delimited(DelimitedOptions::csv());
        std::fs::write(&path, "earlier").unwrap();

        let error = export(
            &session,
            "SELECT nope FROM kerno.nowhere",
            csv(),
            Encoding::Utf8,
            &path,
        )
        .await
        .unwrap_err();
        assert!(error.db().is_some(), "{error:?}");
        assert!(error.to_string().contains("nowhere"), "{error}");

        let error = export(&session, "SET @kerno_x = 1", csv(), Encoding::Utf8, &path)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("no rows to export"), "{error}");

        let unwritable = dir.path().join("missing").join("out.csv");
        let error = export(&session, "SELECT 1", csv(), Encoding::Utf8, &unwritable)
            .await
            .unwrap_err();
        assert!(matches!(error, Error::Io { .. }), "{error:?}");
        assert!(error.to_string().contains("missing"), "{error}");

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "earlier");
        let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(left.len(), 1);
    }
}

/// The `\n` in a file, read a block at a time.
fn line_count(path: &Path) -> usize {
    use std::io::Read;
    let mut file = std::fs::File::open(path).unwrap();
    let mut block = vec![0; 1 << 16];
    let mut lines = 0;
    loop {
        match file.read(&mut block).unwrap() {
            0 => return lines,
            read => lines += block[..read].iter().filter(|byte| **byte == b'\n').count(),
        }
    }
}

/// Peak resident memory of this process while `body` runs, sampled with
/// `ps` every 200 ms, minus what it was before.
async fn peak_growth_kb<T>(body: impl Future<Output = T>) -> (T, u64) {
    fn rss_kb() -> u64 {
        let output = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .unwrap()
    }
    let before = rss_kb();
    let peak = Arc::new(AtomicU64::new(before));
    let done = Arc::new(AtomicBool::new(false));
    let sampler = std::thread::spawn({
        let (peak, done) = (peak.clone(), done.clone());
        move || {
            while !done.load(Ordering::Acquire) {
                peak.fetch_max(rss_kb(), Ordering::AcqRel);
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    });
    let value = body.await;
    done.store(true, Ordering::Release);
    sampler.join().unwrap();
    (value, peak.load(Ordering::Acquire).saturating_sub(before))
}

#[tokio::test]
async fn the_million_row_table_exports_line_by_line() {
    for engine in engines() {
        let session = common::session(engine).await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.csv");
        let reports = Arc::new(Mutex::new(Vec::new()));
        let job = ExportJob {
            sql: "SELECT * FROM kerno_analytics.big_table".into(),
            format: ExportFormat::Delimited(DelimitedOptions::csv()),
            encoding: Encoding::Utf8,
            path: path.clone(),
        };
        let started = Instant::now();
        let (written, growth) = peak_growth_kb(export_to_file(
            session.clone(),
            job,
            {
                let reports = reports.clone();
                move |progress| reports.lock().unwrap().push(progress)
            },
            Arc::default(),
        ))
        .await;
        let written = written.unwrap();
        let elapsed = started.elapsed();
        assert_eq!(written.rows, 1_000_000);
        let size = std::fs::metadata(&path).unwrap().len();
        assert_eq!(written.bytes, size);
        assert_eq!(line_count(&path), 1_000_001);
        let reports = reports.lock().unwrap();
        // How many depends on how the batches arrive; enough of them show it streams.
        assert!(reports.len() >= 20, "{} progress reports", reports.len());
        assert!(reports.windows(2).all(|pair| pair[0].rows <= pair[1].rows));
        eprintln!(
            "{} export: 1,000,000 rows, {} MB in {:.1} s = {:.0} rows/s; peak RSS growth {} MB",
            engine.label(),
            size / 1_000_000,
            elapsed.as_secs_f64(),
            1_000_000.0 / elapsed.as_secs_f64(),
            growth / 1024
        );
    }
}

/// `rows` records of every guessable kind, with a header.
fn generated_csv(path: &Path, rows: u64) {
    use std::io::Write;
    let mut out = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    writeln!(out, "id,amount,flag,day,at,uid,doc,name").unwrap();
    for i in 1..=rows {
        writeln!(
            out,
            "{i},{}.{:02},{},2020-01-{:02},2020-01-{:02} 10:{:02}:00,00000000-0000-4000-8000-{i:012x},\
             \"{{\"\"k\"\": {}}}\",\"name {i}, \"\"{}\"\"\"",
            i * 3 / 2,
            i % 100,
            i % 2 == 0,
            1 + i % 28,
            1 + i % 28,
            i % 60,
            i % 100,
            i % 7
        )
        .unwrap();
    }
    out.flush().unwrap();
}

#[tokio::test]
async fn a_hundred_thousand_rows_go_into_a_new_table() {
    in_scratch(
        "transfer_new_table",
        |session, engine, database| async move {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("rows.csv");
            generated_csv(&path, 100_000);
            let options = csv_options(true);
            let preview = preview(&path, &options, 50).unwrap();
            assert_eq!(
                preview.guessed,
                [
                    TypeKind::Integer,
                    TypeKind::Decimal,
                    TypeKind::Boolean,
                    TypeKind::Date,
                    TypeKind::DateTime,
                    TypeKind::Uuid,
                    TypeKind::Json,
                    TypeKind::Text
                ]
            );
            let columns: Vec<(String, String)> = preview
                .headers
                .iter()
                .zip(&preview.guessed)
                .map(|(name, kind)| (name.clone(), session.dialect().type_for(*kind).to_string()))
                .collect();
            let names: Vec<Option<&str>> =
                preview.headers.iter().map(|h| Some(h.as_str())).collect();
            let target = Target::New {
                table: TableRef::new(database.clone(), "imported"),
                columns,
            };
            let started = Instant::now();
            let report = import(
                &session,
                job(&path, options, target, &names),
                |_| {},
                Arc::default(),
            )
            .await
            .unwrap();
            let elapsed = started.elapsed();
            assert_eq!(
                report,
                ImportReport {
                    inserted: 100_000,
                    failed: None
                }
            );
            eprintln!(
                "{} import: 100,000 rows in {:.1} s = {:.0} rows/s",
                engine.label(),
                elapsed.as_secs_f64(),
                100_000.0 / elapsed.as_secs_f64()
            );
            let table = format!("{database}.imported");
            assert_eq!(
                scalar(&session, &format!("SELECT count(*) FROM {table}")).await,
                "100000"
            );
            assert_eq!(
                rows(
                    &session,
                    &format!(
                        "SELECT id, amount, flag, day, at, uid, JSON_EXTRACT(doc, '$.k'), name \
                     FROM {table} WHERE id = 7"
                    )
                )
                .await,
                [[
                    "7",
                    "10.0700000000",
                    "0",
                    "2020-01-08",
                    "2020-01-08 10:07:00",
                    "00000000-0000-4000-8000-000000000007",
                    "7",
                    "name 7, \"0\""
                ]
                .map(|value| Some(value.to_string()))]
            );
            let types: Vec<String> = rows(
                &session,
                &format!(
                    "SELECT COLUMN_TYPE FROM information_schema.COLUMNS \
                 WHERE TABLE_SCHEMA = '{database}' AND TABLE_NAME = 'imported' \
                 ORDER BY ORDINAL_POSITION"
                ),
            )
            .await
            .into_iter()
            .map(|row| row[0].clone().unwrap())
            .collect();
            let (bigint, json) = match engine {
                Engine::MariaDb => ("bigint(20)", "longtext"),
                _ => ("bigint", "json"),
            };
            assert_eq!(
                types,
                [
                    bigint,
                    "decimal(38,10)",
                    "tinyint(1)",
                    "date",
                    "datetime",
                    "char(36)",
                    json,
                    "text"
                ]
            );
        },
    )
    .await;
}

#[tokio::test]
async fn a_mapping_skips_and_reorders_columns() {
    in_scratch("transfer_mapping", |session, _, database| async move {
        exec_ok(
            &session,
            &format!("CREATE TABLE {database}.m (a INT, b TEXT, c DATE)"),
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.csv");
        std::fs::write(
            &path,
            "c_col,ignored,a_col,b_col\r\n2024-01-02,zzz,1,x\r\n,\"\",2,\"\"\r\n",
        )
        .unwrap();
        let target = Target::Existing(TableRef::new(database.clone(), "m"));
        let report = import(
            &session,
            job(
                &path,
                csv_options(true),
                target,
                &[Some("c"), None, Some("a"), Some("b")],
            ),
            |_| {},
            Arc::default(),
        )
        .await
        .unwrap();
        assert_eq!(report.inserted, 2);
        assert_eq!(
            rows(
                &session,
                &format!("SELECT a, b, c FROM {database}.m ORDER BY a")
            )
            .await,
            vec![
                vec![
                    Some("1".into()),
                    Some("x".into()),
                    Some("2024-01-02".into())
                ],
                vec![Some("2".into()), Some(String::new()), None],
            ]
        );
    })
    .await;
}

/// A file of `rows` records `id,name` with a two-line name in record 3 and
/// `bad` as the record `bad_at` (1-based).
fn file_with(path: &Path, rows: usize, bad_at: usize, bad: &str) {
    let mut text = String::from("id,name\n");
    for i in 1..=rows {
        match i {
            _ if i == bad_at => text.push_str(bad),
            3 => text.push_str("3,\"two\nlines\""),
            _ => text.push_str(&format!("{i},n{i}")),
        }
        text.push('\n');
    }
    std::fs::write(path, text).unwrap();
}

async fn table_count(session: &Arc<dyn Session>, database: &str) -> String {
    scalar(
        session,
        &format!(
            "SELECT count(*) FROM information_schema.TABLES WHERE TABLE_SCHEMA = '{database}'"
        ),
    )
    .await
}

#[tokio::test]
async fn a_bad_record_rolls_everything_back_and_names_its_line() {
    in_scratch("transfer_bad_line", |session, _, database| async move {
        exec_ok(
            &session,
            &format!("CREATE TABLE {database}.t (id INT, name TEXT)"),
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.csv");
        let names = [Some("id"), Some("name")];
        // Record 700 starts on line 702: the header, and record 3 spans
        // two lines.
        file_with(&path, 1_200, 700, "oops,n700");
        let target = Target::Existing(TableRef::new(database.clone(), "t"));
        let report = import(
            &session,
            job(&path, csv_options(true), target, &names),
            |_| {},
            Arc::default(),
        )
        .await
        .unwrap();
        let failure = report.failed.clone().expect("the import fails");
        assert_eq!(report.inserted, 0);
        assert_eq!(failure.line, 702, "{failure}");
        assert!(failure.message.contains("oops"), "{failure}");
        assert_eq!(
            scalar(&session, &format!("SELECT count(*) FROM {database}.t")).await,
            "0"
        );
        assert_eq!(session.tx_state(), TxState::Idle);

        // A new table, created before the transaction here, is dropped.
        file_with(&path, 10, 9, "oops,n9");
        let target = Target::New {
            table: TableRef::new(database.clone(), "fresh"),
            columns: vec![("id".into(), "int".into()), ("name".into(), "text".into())],
        };
        let report = import(
            &session,
            job(&path, csv_options(true), target, &names),
            |_| {},
            Arc::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            report.failed.map(|ImportFailure { line, .. }| line),
            Some(11)
        );
        assert_eq!(table_count(&session, &database).await, "1");
    })
    .await;
}

#[tokio::test]
async fn cancel_rolls_the_import_back() {
    in_scratch(
        "transfer_cancel_import",
        |session, _, database| async move {
            exec_ok(
                &session,
                &format!("CREATE TABLE {database}.t (id INT, name TEXT)"),
            )
            .await;
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("rows.csv");
            file_with(&path, 5_000, 0, "");
            let names = [Some("id"), Some("name")];
            let targets = [
                Target::Existing(TableRef::new(database.clone(), "t")),
                Target::New {
                    table: TableRef::new(database.clone(), "fresh"),
                    columns: vec![("id".into(), "int".into()), ("name".into(), "text".into())],
                },
            ];
            for target in targets {
                let cancel = Arc::new(AtomicBool::new(false));
                let flag = cancel.clone();
                let outcome = import(
                    &session,
                    job(&path, csv_options(true), target, &names),
                    move |inserted| {
                        assert_eq!(inserted, 500);
                        flag.store(true, Ordering::Release);
                    },
                    cancel,
                )
                .await;
                assert!(matches!(outcome, Err(Error::Cancelled)), "{outcome:?}");
            }
            assert_eq!(
                scalar(&session, &format!("SELECT count(*) FROM {database}.t")).await,
                "0"
            );
            assert_eq!(table_count(&session, &database).await, "1");
        },
    )
    .await;
}
