//! Running statements on MySQL and MariaDB: streaming with demand, cancel,
//! every type's text form, several results, warnings, errors and
//! transaction state. Runs against the `kerno-mysql` and `kerno-mariadb`
//! containers; skipped with a message when they are down.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{db_error, engines, exec_ok, finish, next_event, read_rows, rows, run, scalar};
use kerno_core::config::Engine;
use kerno_core::exec::{self, ExecEvent, RowLimit};
use kerno_core::{Error, Session, TxState, TypeKind};

const BIG: &str = "SELECT * FROM kerno_analytics.big_table ORDER BY id";

/// Ten million rows, streamed from the first one: reading it to the end
/// takes far longer than any limit below.
const HUGE: &str =
    "SELECT STRAIGHT_JOIN b.* FROM kerno.categories c JOIN kerno_analytics.big_table b";

/// Counts 5,000 rows at 20 ms each: 100 s if nothing stops it, and both
/// engines answer a kill with an error (a lone `SLEEP()` does not on MySQL).
const SLOW: &str = "SELECT COUNT(*) FROM kerno.orders WHERE SLEEP(0.02) = 0";

/// `ER_QUERY_INTERRUPTED`.
const INTERRUPTED: &str = "1317 (70100)";

/// What the server says the connection `id` is doing.
async fn command_of(observer: &Arc<dyn Session>, id: &str) -> String {
    scalar(
        observer,
        &format!("SELECT COMMAND FROM information_schema.PROCESSLIST WHERE ID = {id}"),
    )
    .await
}

/// How many bytes the server has sent to this session so far.
async fn bytes_sent(session: &Arc<dyn Session>) -> u64 {
    rows(session, "SHOW SESSION STATUS LIKE 'Bytes_sent'").await[0][1]
        .as_deref()
        .and_then(|bytes| bytes.parse().ok())
        .expect("the Bytes_sent counter")
}

#[tokio::test]
async fn rows_stream_only_as_far_as_asked() {
    for engine in engines() {
        let session = common::session(engine).await;
        let started = Instant::now();
        let mut execution = exec::run(session.clone(), BIG.into(), RowLimit::Rows(1_000));

        let columns = match next_event(&mut execution).await {
            Some(ExecEvent::Columns(columns)) => columns,
            other => panic!("expected the columns first, got {other:?}"),
        };
        assert_eq!(columns.len(), 20);
        assert_eq!(columns[0].name, "id");

        let (seen, complete) = read_rows(&mut execution, 1_000).await;
        assert_eq!((seen, complete), (1_000, false));
        // The first page of a million-row result arrives long before the
        // server could have sent everything.
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );

        // No demand: nothing more arrives, the statement stays suspended.
        let idle = tokio::time::timeout(Duration::from_millis(300), execution.next()).await;
        assert!(idle.is_err(), "received an event without demand: {idle:?}");

        execution.request_rows(2_500);
        let (seen, complete) = read_rows(&mut execution, 2_500).await;
        assert_eq!((seen, complete), (2_500, false));

        // Abandon the rest of the million rows: the session must be usable
        // again at once.
        drop(execution);
        let started = Instant::now();
        assert_eq!(scalar(&session, "SELECT 42").await, "42");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        // And it is usable because the server stopped, not because the
        // driver read the other 996,500 rows off the wire (some 200 MB):
        // the server sent the pages asked for and what fits in the buffers
        // between the two.
        let sent = bytes_sent(&session).await;
        assert!(sent < 30_000_000, "the server sent {sent} bytes");
    }
}

#[tokio::test]
async fn abandoning_a_suspended_result_stops_the_server() {
    for engine in engines() {
        let session = common::session(engine).await;
        let observer = common::session(engine).await;
        let id = scalar(&session, "SELECT CONNECTION_ID()").await;

        let mut execution = exec::run(session.clone(), HUGE.into(), RowLimit::Rows(500));
        let (seen, complete) = read_rows(&mut execution, 500).await;
        assert_eq!((seen, complete), (500, false));
        // Suspended on the server, with ten million rows still to send.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(command_of(&observer, &id).await, "Query");

        drop(execution);
        // Usable at once, which it only is if the server stopped sending:
        // reading the rest off the wire would take a minute.
        let started = Instant::now();
        assert_eq!(scalar(&session, "SELECT 42").await, "42");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(command_of(&observer, &id).await, "Sleep");
        let sent = bytes_sent(&session).await;
        assert!(sent < 30_000_000, "the server sent {sent} bytes");
        // The kill did not linger to hit the statements that follow.
        assert_eq!(
            scalar(&session, "SELECT COUNT(*) FROM kerno.orders").await,
            "5000"
        );
    }
}

#[tokio::test]
async fn the_kill_of_an_abandoned_result_never_stops_the_next_statement() {
    for engine in engines() {
        let session = common::session(engine).await;
        // The whole small result is on its way when the consumer lets it
        // go: the statement is over on the server when the kill arrives.
        for round in 0..30 {
            let mut execution = exec::run(
                session.clone(),
                "SELECT id FROM kerno.orders".into(),
                RowLimit::Rows(1_000),
            );
            let (seen, complete) = read_rows(&mut execution, 1_000).await;
            assert_eq!((seen, complete), (1_000, false));
            drop(execution);
            let next = run(&session, "SELECT SLEEP(0.05), 'next'").await;
            assert!(next.is_ok(), "{engine:?} round {round}: {next:?}");
        }
    }
}

#[tokio::test]
async fn a_cancel_stops_the_statement_waiting_for_its_turn_too() {
    for engine in engines() {
        let session = common::session(engine).await;
        // A holds the session; B waits behind it when the cancel comes: B
        // is not sent, rather than run in full after the kill.
        let mut first = exec::run(session.clone(), "SELECT SLEEP(1)".into(), RowLimit::All);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let mut second = exec::run(session.clone(), "SELECT SLEEP(30)".into(), RowLimit::All);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let started = Instant::now();
        session.cancel().await.unwrap();
        // SLEEP ends early and quietly when killed.
        let _ = finish(&mut first).await;
        assert!(
            matches!(finish(&mut second).await, Err(Error::Cancelled)),
            "{engine:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{engine:?}: {:?}",
            started.elapsed()
        );

        // A cancel of an idle session leaves the next statement alone.
        session.cancel().await.unwrap();
        assert_eq!(
            scalar(&session, "SELECT CONCAT(SLEEP(0.2), 'next')").await,
            "0next",
            "{engine:?}"
        );
    }
}

#[tokio::test]
async fn fetch_all_delivers_every_row_in_order() {
    for engine in engines() {
        let session = common::session(engine).await;
        let mut execution = exec::run(
            session.clone(),
            "SELECT id FROM kerno_analytics.big_table WHERE id <= 30000 ORDER BY id".into(),
            RowLimit::Rows(100),
        );
        execution.request_all();
        let mut expected = 1u64;
        let mut reported = None;
        loop {
            match next_event(&mut execution).await {
                Some(ExecEvent::Rows(batch)) => {
                    for row in 0..batch.row_count() {
                        assert_eq!(batch.text(row, 0), Some(expected.to_string().as_str()));
                        expected += 1;
                    }
                }
                Some(ExecEvent::Complete { rows_affected }) => reported = rows_affected,
                Some(ExecEvent::Finished(result)) => {
                    result.unwrap();
                    break;
                }
                Some(_) => {}
                None => panic!("ended without Finished"),
            }
        }
        assert_eq!(expected - 1, 30_000);
        assert_eq!(reported, Some(30_000));
    }
}

#[tokio::test]
async fn a_result_that_ends_at_the_limit_completes_without_more_demand() {
    for engine in engines() {
        let session = common::session(engine).await;
        let mut execution = exec::run(
            session.clone(),
            "SELECT id FROM kerno_analytics.big_table WHERE id <= 1000 ORDER BY id".into(),
            RowLimit::Rows(1_000),
        );
        let (seen, complete) = read_rows(&mut execution, usize::MAX).await;
        assert_eq!((seen, complete), (1_000, true));
    }
}

#[tokio::test]
async fn a_running_statement_can_be_cancelled() {
    for engine in engines() {
        let session = common::session(engine).await;
        let mut execution = exec::run(session.clone(), SLOW.into(), RowLimit::All);
        tokio::time::sleep(Duration::from_millis(300)).await;

        let started = Instant::now();
        session.cancel().await.unwrap();
        let error = finish(&mut execution).await.unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        let db = error.db().expect("the server reports the interruption");
        assert_eq!(db.code.as_deref(), Some(INTERRUPTED));
        assert!(db.message.contains("interrupted"), "{}", db.message);

        // The session survives, and a cancel with nothing running is
        // harmless: it does not hit the next statement.
        assert_eq!(scalar(&session, "SELECT 1").await, "1");
        session.cancel().await.unwrap();
        assert_eq!(scalar(&session, "SELECT 2").await, "2");
        assert_eq!(
            scalar(&session, "SELECT COUNT(*) FROM kerno.orders").await,
            "5000"
        );
    }
}

#[tokio::test]
async fn cancelling_a_sleep_ends_it_at_once() {
    for engine in engines() {
        let session = common::session(engine).await;
        let mut execution = exec::run(session.clone(), "SELECT SLEEP(60)".into(), RowLimit::All);
        tokio::time::sleep(Duration::from_millis(300)).await;

        let started = Instant::now();
        session.cancel().await.unwrap();
        let mut value = None;
        let outcome = loop {
            match next_event(&mut execution).await {
                Some(ExecEvent::Rows(batch)) => value = batch.text(0, 0).map(str::to_string),
                Some(ExecEvent::Finished(result)) => break result,
                Some(_) => {}
                None => panic!("ended without Finished"),
            }
        };
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the sleep was not interrupted: {:?}",
            started.elapsed()
        );
        match engine {
            // MariaDB fails the statement.
            Engine::MariaDb => {
                let error = outcome.unwrap_err();
                assert_eq!(
                    error.db().and_then(|db| db.code.as_deref()),
                    Some(INTERRUPTED)
                );
            }
            // MySQL lets an interrupted SLEEP() return 1 instead of 0 and
            // reports no error: the driver passes on what the server said.
            _ => {
                outcome.unwrap();
                assert_eq!(value.as_deref(), Some("1"));
            }
        }
        assert_eq!(scalar(&session, "SELECT 1").await, "1");
    }
}

#[tokio::test]
async fn abandoning_a_statement_that_produces_nothing_cancels_it() {
    for engine in engines() {
        let session = common::session(engine).await;
        let observer = common::session(engine).await;
        let id = scalar(&session, "SELECT CONNECTION_ID()").await;

        let execution = exec::run(session.clone(), "SELECT SLEEP(60)".into(), RowLimit::All);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(command_of(&observer, &id).await, "Query");

        drop(execution);
        // The server stops the sleep instead of running it for a minute.
        let started = Instant::now();
        assert_eq!(scalar(&session, "SELECT 7").await, "7");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
    }
}

#[tokio::test]
async fn every_type_arrives_in_its_text_form() {
    for engine in engines() {
        let session = common::session(engine).await;
        let mariadb = engine == Engine::MariaDb;
        let collected = run(&session, "SELECT * FROM kerno.all_types ORDER BY id")
            .await
            .unwrap();
        let result = &collected.results[0];
        let rows = result.rows();
        assert_eq!(rows.len(), 3);
        assert_eq!(result.rows_affected, Some(3));

        let column = |name: &str| {
            result
                .columns
                .iter()
                .position(|c| c.name == name)
                .unwrap_or_else(|| panic!("no column {name}"))
        };
        let text = |row: usize, name: &str| {
            rows[row][column(name)]
                .clone()
                .unwrap_or_else(|| panic!("{name} is NULL"))
        };
        let bytes = |row: usize, name: &str| {
            result.batches[0]
                .cell(row, column(name))
                .unwrap_or_else(|| panic!("{name} is NULL"))
                .to_vec()
        };

        // Every column: its type name and the kind that drives rendering
        // and literal quoting.
        let described: Vec<(&str, &str, TypeKind)> = result
            .columns
            .iter()
            .map(|c| (c.name.as_str(), c.type_name.as_str(), c.kind))
            .collect();
        // MariaDB's JSON is LONGTEXT, and a result column says no more.
        let json = if mariadb {
            ("c_json", "LONGTEXT", TypeKind::Text)
        } else {
            ("c_json", "JSON", TypeKind::Json)
        };
        assert_eq!(
            described,
            [
                ("id", "INT", TypeKind::Integer),
                ("c_tinyint", "TINYINT", TypeKind::Integer),
                ("c_tinyint_u", "TINYINT UNSIGNED", TypeKind::Integer),
                ("c_bool", "TINYINT(1)", TypeKind::Boolean),
                ("c_smallint", "SMALLINT", TypeKind::Integer),
                ("c_mediumint", "MEDIUMINT", TypeKind::Integer),
                ("c_int", "INT", TypeKind::Integer),
                ("c_int_u", "INT UNSIGNED", TypeKind::Integer),
                ("c_bigint", "BIGINT", TypeKind::Integer),
                ("c_bigint_u", "BIGINT UNSIGNED", TypeKind::Integer),
                ("c_decimal", "DECIMAL", TypeKind::Decimal),
                ("c_float", "FLOAT", TypeKind::Float),
                ("c_double", "DOUBLE", TypeKind::Float),
                ("c_bit", "BIT", TypeKind::Bit),
                ("c_bit1", "BIT", TypeKind::Bit),
                ("c_char", "CHAR", TypeKind::Text),
                ("c_varchar", "VARCHAR", TypeKind::Text),
                ("c_tinytext", "TINYTEXT", TypeKind::Text),
                ("c_text", "TEXT", TypeKind::Text),
                ("c_mediumtext", "MEDIUMTEXT", TypeKind::Text),
                ("c_longtext", "LONGTEXT", TypeKind::Text),
                ("c_binary", "BINARY", TypeKind::Binary),
                ("c_varbinary", "VARBINARY", TypeKind::Binary),
                ("c_blob", "BLOB", TypeKind::Binary),
                ("c_date", "DATE", TypeKind::Date),
                ("c_time", "TIME", TypeKind::Time),
                ("c_datetime", "DATETIME", TypeKind::DateTime),
                ("c_timestamp", "TIMESTAMP", TypeKind::DateTime),
                ("c_year", "YEAR", TypeKind::Integer),
                ("c_enum", "ENUM", TypeKind::Enum),
                ("c_set", "SET", TypeKind::Enum),
                json,
                ("c_point", "GEOMETRY", TypeKind::Binary),
            ]
        );

        // Row 1: ordinary values, exactly as the mysql client would print them.
        for (name, expected) in [
            ("id", "1"),
            ("c_tinyint", "127"),
            ("c_tinyint_u", "255"),
            ("c_bool", "1"),
            ("c_smallint", "32767"),
            ("c_mediumint", "8388607"),
            ("c_int", "2147483647"),
            ("c_int_u", "4294967295"),
            ("c_bigint", "9223372036854775807"),
            ("c_bigint_u", "18446744073709551615"),
            ("c_decimal", "12345.678901"),
            ("c_float", "3.14"),
            ("c_double", "2.718281828459045"),
            // BIT: the digits, not the packed bytes the server sends.
            ("c_bit", "10101010"),
            ("c_bit1", "1"),
            ("c_char", "abc"),
            ("c_varchar", "varchar value"),
            ("c_tinytext", "tiny"),
            (
                "c_text",
                "Multi-line\ntext with \"quotes\", a tab\tand unicode: caf\u{e9} \u{2603} \u{1F600}",
            ),
            ("c_mediumtext", "medium"),
            ("c_longtext", "long"),
            ("c_date", "2024-02-29"),
            ("c_time", "13:37:42.123456"),
            ("c_datetime", "2024-02-29 13:37:42.123456"),
            ("c_timestamp", "2024-02-29 13:37:42.123456"),
            ("c_year", "2024"),
            ("c_enum", "happy"),
            ("c_set", "a,c"),
            ("c_json", r#"{"a": 1, "b": [true, null], "c": {"d": "e"}}"#),
        ] {
            assert_eq!(text(0, name), expected, "{name}");
        }
        // Binary columns: the bytes themselves.
        assert_eq!(bytes(0, "c_binary"), [0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(bytes(0, "c_varbinary"), [0x00, 0xff, 0x10]);
        assert_eq!(bytes(0, "c_blob"), [0xde, 0xad, 0xbe, 0xef, 0x00, 0xff]);
        // Geometry: SRID 0, then WKB of POINT(1.5 -2.5), little-endian.
        let mut point = vec![0, 0, 0, 0, 1, 1, 0, 0, 0];
        point.extend(1.5f64.to_le_bytes());
        point.extend((-2.5f64).to_le_bytes());
        assert_eq!(bytes(0, "c_point"), point);

        // Row 2: NULL everywhere but the key, and NULL is not the empty string.
        for (index, cell) in rows[1].iter().enumerate() {
            if index == column("id") {
                assert_eq!(cell.as_deref(), Some("2"));
            } else {
                assert_eq!(cell, &None, "{}", result.columns[index].name);
            }
        }

        // Row 3: edge values survive untouched.
        for (name, expected) in [
            ("c_tinyint", "-128"),
            ("c_tinyint_u", "0"),
            ("c_bool", "0"),
            ("c_smallint", "-32768"),
            ("c_mediumint", "-8388608"),
            ("c_int", "-2147483648"),
            ("c_bigint", "-9223372036854775808"),
            ("c_bigint_u", "0"),
            ("c_decimal", "-99999999999999.999999"),
            ("c_float", "-15000000000"),
            ("c_double", "1.7976931348623157e308"),
            ("c_bit", "00000000"),
            ("c_bit1", "0"),
            ("c_char", ""),
            ("c_varchar", ""),
            ("c_text", ""),
            ("c_longtext", ""),
            ("c_date", "1000-01-01"),
            // TIME is a duration: negative, and far beyond a day.
            ("c_time", "-838:59:59.000000"),
            ("c_datetime", "9999-12-31 23:59:59.999999"),
            ("c_timestamp", "1970-01-01 00:00:01.000000"),
            ("c_year", "1901"),
            ("c_enum", "sad"),
            ("c_set", ""),
            ("c_json", "[]"),
        ] {
            assert_eq!(text(2, name), expected, "{name}");
        }
        assert_eq!(bytes(2, "c_binary"), [0, 0, 0, 0]);
        assert_eq!(bytes(2, "c_varbinary"), [0u8; 0]);
        assert_eq!(bytes(2, "c_blob"), [0u8; 0]);
    }
}

#[tokio::test]
async fn expressions_zero_dates_and_wide_bits() {
    for engine in engines() {
        let session = common::session(engine).await;
        let database = common::fresh_database(&session, "values").await;
        // Zero dates need a permissive mode to be stored at all.
        exec_ok(&session, "SET SESSION sql_mode = ''").await;
        exec_ok(
            &session,
            &format!(
                "CREATE TABLE {database}.t (d DATE, dt DATETIME, ts TIMESTAMP NULL, \
                 b12 BIT(12), b64 BIT(64), flag TINYINT(1), y YEAR)"
            ),
        )
        .await;
        exec_ok(
            &session,
            &format!(
                "INSERT INTO {database}.t VALUES \
                 ('0000-00-00', '0000-00-00 00:00:00', '0000-00-00 00:00:00', \
                  b'111100000001', b'1111111111111111111111111111111111111111111111111111111111111111', \
                  5, 0)"
            ),
        )
        .await;
        let collected = run(&session, &format!("SELECT * FROM {database}.t"))
            .await
            .unwrap();
        let result = &collected.results[0];
        assert_eq!(
            result.rows(),
            vec![vec![
                // Zero dates as the server prints them, not NULL, not an error.
                Some("0000-00-00".to_string()),
                Some("0000-00-00 00:00:00".to_string()),
                Some("0000-00-00 00:00:00".to_string()),
                Some("111100000001".to_string()),
                Some("1".repeat(64)),
                // A TINYINT(1) can hold more than 0 and 1; the number is kept.
                Some("5".to_string()),
                Some("0000".to_string()),
            ]]
        );
        assert_eq!(result.columns[5].kind, TypeKind::Boolean);

        // Expressions: types the server derives, not columns of a table.
        let collected = run(
            &session,
            "SELECT 1 AS i, 1.5 AS d, 1e0 AS f, 'x' AS s, NULL AS n, X'00ff' AS b, \
                    NOW() AS ts, TRUE AS t, CAST(18446744073709551615 AS UNSIGNED) AS u",
        )
        .await
        .unwrap();
        let kinds: Vec<(&str, TypeKind)> = collected.results[0]
            .columns
            .iter()
            .map(|c| (c.name.as_str(), c.kind))
            .collect();
        assert_eq!(
            kinds,
            [
                ("i", TypeKind::Integer),
                ("d", TypeKind::Decimal),
                ("f", TypeKind::Float),
                ("s", TypeKind::Text),
                ("n", TypeKind::Other),
                ("b", TypeKind::Binary),
                ("ts", TypeKind::DateTime),
                ("t", TypeKind::Integer),
                ("u", TypeKind::Integer),
            ]
        );
        assert_eq!(collected.results[0].columns[8].type_name, "BIGINT UNSIGNED");
        assert_eq!(
            collected.results[0].batches[0].cell(0, 5),
            Some(&[0x00, 0xff][..])
        );
        assert_eq!(collected.results[0].batches[0].cell(0, 4), None);

        exec_ok(&session, &format!("DROP DATABASE {database}")).await;
    }
}

#[tokio::test]
async fn commands_report_their_row_counts() {
    for engine in engines() {
        let session = common::session(engine).await;
        let database = common::fresh_database(&session, "counts").await;
        exec_ok(
            &session,
            &format!("CREATE TABLE {database}.t (id int PRIMARY KEY, v varchar(20))"),
        )
        .await;

        let inserted = run(
            &session,
            &format!(
                "INSERT INTO {database}.t SELECT id, CONCAT('v', id) FROM kerno.categories WHERE id <= 5"
            ),
        )
        .await
        .unwrap();
        assert_eq!(inserted.results.len(), 1);
        assert!(inserted.results[0].columns.is_empty());
        assert_eq!(inserted.results[0].rows_affected, Some(5));

        let updated = run(
            &session,
            &format!("UPDATE {database}.t SET v = 'x' WHERE id > 3"),
        )
        .await
        .unwrap();
        assert_eq!(updated.results[0].rows_affected, Some(2));

        // Rows matched, not rows changed: an UPDATE that sets a value to
        // what it already is still found its row. Applying pending edits
        // relies on it (one statement, exactly one row).
        let same = run(
            &session,
            &format!("UPDATE {database}.t SET v = 'x' WHERE id = 5"),
        )
        .await
        .unwrap();
        assert_eq!(same.results[0].rows_affected, Some(1));
        let none = run(
            &session,
            &format!("UPDATE {database}.t SET v = 'x' WHERE id = 99"),
        )
        .await
        .unwrap();
        assert_eq!(none.results[0].rows_affected, Some(0));

        let deleted = run(&session, &format!("DELETE FROM {database}.t WHERE id = 1"))
            .await
            .unwrap();
        assert_eq!(deleted.results[0].rows_affected, Some(1));

        // A select with no rows still has its columns.
        let empty = run(
            &session,
            &format!("SELECT id, v FROM {database}.t WHERE false"),
        )
        .await
        .unwrap();
        assert_eq!(empty.results[0].columns.len(), 2);
        assert_eq!(empty.results[0].columns[0].kind, TypeKind::Integer);
        assert_eq!(empty.results[0].row_count(), 0);
        assert_eq!(empty.results[0].rows_affected, Some(0));

        exec_ok(&session, &format!("DROP DATABASE {database}")).await;
    }
}

#[tokio::test]
async fn a_procedure_call_gives_its_result_sets_then_its_own_outcome() {
    for engine in engines() {
        let session = common::session(engine).await;
        let database = common::fresh_database(&session, "procedures").await;
        let orders = rows(
            &session,
            "SELECT id, status, total FROM kerno.orders WHERE customer_id = 7 ORDER BY id",
        )
        .await;
        assert!(orders.len() > 1);

        // One SELECT inside: its rows, then the outcome of the CALL itself.
        let collected = run(&session, "CALL kerno.customer_orders(7, @count)")
            .await
            .unwrap();
        assert_eq!(collected.results.len(), 2);
        let names: Vec<_> = collected.results[0]
            .columns
            .iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(names, ["id", "status", "total"]);
        assert_eq!(collected.results[0].rows(), orders);
        assert_eq!(
            collected.results[0].rows_affected,
            Some(orders.len() as u64)
        );
        assert!(collected.results[1].columns.is_empty());
        // The OUT parameter was set.
        assert_eq!(
            scalar(&session, "SELECT @count").await,
            orders.len().to_string()
        );

        // Several SELECTs inside: one result set each, in order.
        exec_ok(
            &session,
            &format!(
                "CREATE PROCEDURE {database}.three() BEGIN \
                     SELECT 1 AS a; \
                     SELECT 'x' AS b, 2 AS c UNION ALL SELECT 'y', 3; \
                     SELECT id FROM kerno.categories WHERE false; \
                 END"
            ),
        )
        .await;
        let collected = run(&session, &format!("CALL {database}.three()"))
            .await
            .unwrap();
        assert_eq!(collected.results.len(), 4);
        assert_eq!(collected.results[0].rows(), vec![vec![Some("1".into())]]);
        assert_eq!(collected.results[1].columns[0].name, "b");
        assert_eq!(
            collected.results[1].rows(),
            vec![
                vec![Some("x".into()), Some("2".into())],
                vec![Some("y".into()), Some("3".into())]
            ]
        );
        assert_eq!(collected.results[1].rows_affected, Some(2));
        assert_eq!(collected.results[2].columns[0].name, "id");
        assert_eq!(collected.results[2].row_count(), 0);
        assert!(collected.results[3].columns.is_empty());

        // The session is ready for the next statement.
        assert_eq!(scalar(&session, "SELECT 1").await, "1");
        exec_ok(&session, &format!("DROP DATABASE {database}")).await;
    }
}

#[tokio::test]
async fn several_statements_in_one_request_give_several_results() {
    for engine in engines() {
        let session = common::session(engine).await;
        let collected = run(&session, "SELECT 1 AS a; SELECT 'x' AS b, 2 AS c; DO 1")
            .await
            .unwrap();
        assert_eq!(collected.results.len(), 3);
        assert_eq!(collected.results[0].rows(), vec![vec![Some("1".into())]]);
        assert_eq!(collected.results[1].columns[0].name, "b");
        assert_eq!(
            collected.results[1].rows(),
            vec![vec![Some("x".into()), Some("2".into())]]
        );
        assert!(collected.results[2].columns.is_empty());
        assert_eq!(collected.results[2].rows_affected, Some(0));

        // A later statement that fails: what came before stands, then the
        // error; nothing after it runs.
        let mut execution = exec::run(
            session.clone(),
            "SELECT 1; SELECT * FROM kerno.no_such_table; SELECT 3".into(),
            RowLimit::All,
        );
        let mut results = 0;
        let error = loop {
            match next_event(&mut execution).await {
                Some(ExecEvent::Complete { .. }) => results += 1,
                Some(ExecEvent::Finished(result)) => break result.unwrap_err(),
                Some(_) => {}
                None => panic!("ended without Finished"),
            }
        };
        assert_eq!(results, 1);
        assert_eq!(
            error.db().and_then(|db| db.code.as_deref()),
            Some("1146 (42S02)")
        );
        assert_eq!(scalar(&session, "SELECT 4").await, "4");

        // Nothing to run is not an error. (The app never sends these: the
        // statement splitter drops empty statements.)
        for sql in ["", "   ", "-- just a comment", ";"] {
            let collected = run(&session, sql)
                .await
                .unwrap_or_else(|e| panic!("{sql:?}: {e}"));
            assert_eq!(collected.results.len(), 1, "{sql:?}");
            assert!(collected.results[0].columns.is_empty(), "{sql:?}");
            assert_eq!(collected.results[0].rows_affected, Some(0), "{sql:?}");
        }
    }
}

#[tokio::test]
async fn duplicate_and_unnamed_columns_are_kept() {
    for engine in engines() {
        let session = common::session(engine).await;
        let collected = run(&session, "SELECT 1 AS x, 2 AS x, 3, NULL")
            .await
            .unwrap();
        let result = &collected.results[0];
        let names: Vec<_> = result.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["x", "x", "3", "NULL"]);
        assert_eq!(
            result.rows(),
            vec![vec![
                Some("1".into()),
                Some("2".into()),
                Some("3".into()),
                None
            ]]
        );
    }
}

#[tokio::test]
async fn warnings_are_delivered_as_notices() {
    for engine in engines() {
        let session = common::session(engine).await;
        // A statement that succeeds with warnings.
        let collected = run(&session, "SELECT 1 / 0 AS q, CAST('abc' AS SIGNED) AS n")
            .await
            .unwrap();
        assert_eq!(
            collected.results[0].rows(),
            vec![vec![None, Some("0".into())]]
        );
        let seen: Vec<(&str, Option<&str>, &str)> = collected
            .notices
            .iter()
            .map(|n| (n.severity.as_str(), n.code.as_deref(), n.message.as_str()))
            .collect();
        assert_eq!(
            seen,
            [
                ("Warning", Some("1365"), "Division by 0"),
                (
                    "Warning",
                    Some("1292"),
                    "Truncated incorrect INTEGER value: 'abc'"
                ),
            ]
        );

        // A note from DDL, and none left over for the next statement.
        let collected = run(&session, "DROP TABLE IF EXISTS kerno.kerno_never_existed")
            .await
            .unwrap();
        assert_eq!(collected.notices.len(), 1, "{:?}", collected.notices);
        assert_eq!(collected.notices[0].severity, "Note");
        assert_eq!(collected.notices[0].code.as_deref(), Some("1051"));
        assert!(
            collected.notices[0].message.contains("kerno_never_existed"),
            "{:?}",
            collected.notices
        );
        let collected = run(&session, "SELECT 1").await.unwrap();
        assert!(collected.notices.is_empty(), "{:?}", collected.notices);

        // Reading the warnings does not disturb what follows.
        assert_eq!(session.tx_state(), TxState::Idle);
        assert_eq!(scalar(&session, "SELECT 2").await, "2");
    }
}

#[tokio::test]
async fn server_errors_carry_code_and_position() {
    for engine in engines() {
        let session = common::session(engine).await;

        let error = db_error(&session, "SELEC 1").await;
        assert_eq!(error.code.as_deref(), Some("1064 (42000)"));
        assert_eq!(error.position, Some(1));
        assert_eq!(error.severity.as_deref(), Some("ERROR"));
        assert!(error.message.contains("SQL syntax"), "{}", error.message);

        let error = db_error(&session, "SELECT *\nFROM WHERE").await;
        assert_eq!(error.code.as_deref(), Some("1064 (42000)"));
        assert_eq!(error.position, Some(15));

        // Only the parser says where; other errors name what is wrong.
        let error = db_error(&session, "SELECT *\nFROM kerno.no_such_table").await;
        assert_eq!(error.code.as_deref(), Some("1146 (42S02)"));
        assert_eq!(error.position, None);
        assert!(error.message.contains("no_such_table"), "{}", error.message);

        let error = db_error(&session, "SELECT full_nam FROM kerno.customers").await;
        assert_eq!(error.code.as_deref(), Some("1054 (42S22)"));
        assert!(error.message.contains("full_nam"), "{}", error.message);

        let error = db_error(
            &session,
            "INSERT INTO kerno.categories (id, name, slug) VALUES (1, 'dup', 'dup')",
        )
        .await;
        assert_eq!(error.code.as_deref(), Some("1062 (23000)"));
        assert!(
            error.message.contains("Duplicate entry '1'"),
            "{}",
            error.message
        );

        // An error a trigger raises on purpose.
        let error = db_error(
            &session,
            "INSERT INTO kerno.reviews (product_id, customer_id, rating, created_at) VALUES (1, 1, 9, NOW())",
        )
        .await;
        assert!(
            error
                .code
                .as_deref()
                .is_some_and(|code| code.ends_with("(45000)")),
            "{error:?}"
        );
        assert_eq!(error.message, "rating must be between 1 and 5");

        // A runtime error after rows started: the rows delivered stand,
        // then the error.
        let mut execution = exec::run(
            session.clone(),
            "SELECT c.id, (SELECT d.id FROM kerno.categories d WHERE c.id >= 4 AND d.id >= c.id) \
             FROM kerno.categories c ORDER BY c.id"
                .into(),
            RowLimit::All,
        );
        let mut seen = 0;
        let error = loop {
            match next_event(&mut execution).await {
                Some(ExecEvent::Rows(batch)) => seen += batch.row_count(),
                Some(ExecEvent::Finished(result)) => break result.unwrap_err(),
                Some(_) => {}
                None => panic!("ended without Finished"),
            }
        };
        assert!(seen <= 3, "{seen}");
        // ER_SUBQUERY_NO_1_ROW.
        assert_eq!(
            error.db().and_then(|db| db.code.as_deref()),
            Some("1242 (21000)")
        );

        // The session is fine afterwards.
        assert_eq!(scalar(&session, "SELECT 1").await, "1");
    }
}

/// Checks what the server itself says about the transaction: whether this
/// connection has one open in InnoDB. The server answers from a cache it
/// refreshes only when nobody read it for 100 ms, hence the patient retries.
async fn assert_server_in_transaction(observer: &Arc<dyn Session>, id: &str, expected: bool) {
    let sql = format!(
        "SELECT COUNT(*) FROM information_schema.INNODB_TRX WHERE trx_mysql_thread_id = {id}"
    );
    for _ in 0..20 {
        // Never answered from the cache of an earlier check.
        tokio::time::sleep(Duration::from_millis(150)).await;
        if (scalar(observer, &sql).await == "1") == expected {
            return;
        }
    }
    panic!(
        "the server says connection {id} is in a transaction: {}",
        !expected
    );
}

#[tokio::test]
async fn transaction_state_follows_the_server() {
    for engine in engines() {
        let session = common::session(engine).await;
        let other = common::session(engine).await;
        let id = scalar(&session, "SELECT CONNECTION_ID()").await;
        let database = common::fresh_database(&session, "tx").await;
        exec_ok(
            &session,
            &format!("CREATE TABLE {database}.t (id int PRIMARY KEY)"),
        )
        .await;
        let count = format!("SELECT COUNT(*) FROM {database}.t");
        assert_eq!(session.tx_state(), TxState::Idle);

        // Commit keeps the rows.
        exec_ok(&session, "START TRANSACTION").await;
        assert_eq!(session.tx_state(), TxState::Open);
        exec_ok(&session, &format!("INSERT INTO {database}.t VALUES (1)")).await;
        assert_eq!(session.tx_state(), TxState::Open);
        assert_server_in_transaction(&other, &id, true).await;
        exec_ok(&session, "COMMIT").await;
        assert_eq!(session.tx_state(), TxState::Idle);
        assert_server_in_transaction(&other, &id, false).await;
        assert_eq!(scalar(&session, &count).await, "1");

        // Rollback discards them; another session never saw them.
        exec_ok(&session, "BEGIN").await;
        exec_ok(&session, &format!("INSERT INTO {database}.t VALUES (2)")).await;
        assert_eq!(scalar(&other, &count).await, "1");
        exec_ok(&session, "ROLLBACK").await;
        assert_eq!(session.tx_state(), TxState::Idle);
        assert_eq!(scalar(&session, &count).await, "1");

        // An error inside a transaction undoes the statement, not the
        // transaction: it stays open and usable. There is no failed state.
        exec_ok(&session, "START TRANSACTION").await;
        exec_ok(&session, &format!("INSERT INTO {database}.t VALUES (3)")).await;
        let error = db_error(&session, &format!("INSERT INTO {database}.t VALUES (1)")).await;
        assert_eq!(error.code.as_deref(), Some("1062 (23000)"));
        assert_eq!(session.tx_state(), TxState::Open);
        assert_server_in_transaction(&other, &id, true).await;
        assert_eq!(scalar(&session, &count).await, "2");
        let _ = db_error(&session, "SELEC").await;
        assert_eq!(session.tx_state(), TxState::Open);
        exec_ok(&session, "ROLLBACK").await;
        assert_eq!(session.tx_state(), TxState::Idle);
        assert_eq!(scalar(&session, &count).await, "1");

        // An error outside a transaction leaves none behind.
        let _ = db_error(&session, "SELEC").await;
        assert_eq!(session.tx_state(), TxState::Idle);

        // The state is the server's, not a reading of the SQL text: a
        // transaction opened where no parser of statement starts would see
        // it, and one ended behind the driver's back by DDL.
        exec_ok(&session, "SELECT 1; START TRANSACTION").await;
        assert_eq!(session.tx_state(), TxState::Open);
        exec_ok(&session, "/* not a commit */ ROLLBACK").await;
        assert_eq!(session.tx_state(), TxState::Idle);
        exec_ok(
            &session,
            &format!("CREATE PROCEDURE {database}.begins() BEGIN START TRANSACTION; INSERT INTO {database}.t VALUES (4); END"),
        )
        .await;
        exec_ok(&session, &format!("CALL {database}.begins()")).await;
        assert_eq!(session.tx_state(), TxState::Open);
        assert_server_in_transaction(&other, &id, true).await;
        // DDL commits implicitly.
        exec_ok(&session, &format!("CREATE TABLE {database}.u (id int)")).await;
        assert_eq!(session.tx_state(), TxState::Idle);
        assert_server_in_transaction(&other, &id, false).await;
        assert_eq!(scalar(&other, &count).await, "2");

        // With autocommit off, the first statement opens a transaction.
        exec_ok(&session, "SET autocommit = 0").await;
        assert_eq!(session.tx_state(), TxState::Idle);
        exec_ok(&session, &format!("INSERT INTO {database}.t VALUES (5)")).await;
        assert_eq!(session.tx_state(), TxState::Open);
        exec_ok(&session, "ROLLBACK").await;
        assert_eq!(session.tx_state(), TxState::Idle);
        exec_ok(&session, "SET autocommit = 1").await;

        // Cancelling inside a transaction stops the statement and keeps
        // the transaction.
        exec_ok(&session, "START TRANSACTION").await;
        exec_ok(&session, &format!("INSERT INTO {database}.t VALUES (6)")).await;
        let mut execution = exec::run(session.clone(), SLOW.into(), RowLimit::All);
        tokio::time::sleep(Duration::from_millis(200)).await;
        session.cancel().await.unwrap();
        let result = finish(&mut execution).await;
        assert!(matches!(result, Err(Error::Db(_))), "{result:?}");
        assert_eq!(session.tx_state(), TxState::Open);
        assert_eq!(scalar(&session, &count).await, "3");
        exec_ok(&session, "ROLLBACK").await;
        assert_eq!(scalar(&session, &count).await, "2");

        exec_ok(&session, &format!("DROP DATABASE {database}")).await;
    }
}

#[tokio::test]
async fn the_current_database_follows_use() {
    for engine in engines() {
        let session = common::session(engine).await;
        assert_eq!(session.database().as_deref(), Some("kerno"));
        exec_ok(&session, "USE kerno_other").await;
        assert_eq!(session.database().as_deref(), Some("kerno_other"));
        assert_eq!(scalar(&session, "SELECT COUNT(*) FROM notes").await, "2");
        // Inside a longer request too.
        exec_ok(&session, "SELECT 1; USE `kerno_analytics`; SELECT 2").await;
        assert_eq!(session.database().as_deref(), Some("kerno_analytics"));
        // A USE that fails changes nothing.
        let error = db_error(&session, "USE no_such_database").await;
        assert_eq!(error.code.as_deref(), Some("1049 (42000)"));
        assert_eq!(session.database().as_deref(), Some("kerno_analytics"));

        // Dropping the current database leaves the session in none.
        let database = common::fresh_database(&session, "use").await;
        exec_ok(&session, &format!("USE {database}")).await;
        assert_eq!(session.database().as_deref(), Some(database.as_str()));
        exec_ok(&session, &format!("DROP DATABASE {database}")).await;
        assert_eq!(session.database(), None);
        assert_eq!(
            scalar(&session, "SELECT COUNT(*) FROM kerno.categories").await,
            "10"
        );
    }
}

#[tokio::test]
async fn a_closed_session_reports_it() {
    for engine in engines() {
        let session = common::session(engine).await;
        let observer = common::session(engine).await;
        let id = scalar(&session, "SELECT CONNECTION_ID()").await;
        assert_eq!(scalar(&session, "SELECT 1").await, "1");
        session.close().await;
        let error = run(&session, "SELECT 1").await.unwrap_err();
        assert!(matches!(error, Error::Closed(_)), "{error:?}");
        assert!(error.is_disconnect());
        assert!(session.ping().await.is_err());
        assert!(session.databases().await.is_err());
        // The server has let go of the connection.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            scalar(
                &observer,
                &format!("SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE ID = {id}")
            )
            .await,
            "0"
        );
    }
}

#[tokio::test]
async fn explain_statements_of_the_dialect_run() {
    for engine in engines() {
        let session = common::session(engine).await;
        let sql = "SELECT * FROM kerno.orders WHERE customer_id = 7 ORDER BY placed_at";
        let dialect = session.dialect();
        assert_eq!(
            scalar(
                &session,
                &format!(
                    "SELECT COUNT(*) FROM ({sql} {}) page",
                    dialect.limit_offset(3, 2)
                )
            )
            .await,
            "3"
        );

        // The plan as JSON, without running the statement.
        let plan = scalar(&session, &dialect.explain(sql, false)).await;
        assert!(plan.trim_start().starts_with('{'), "{plan}");
        assert!(plan.contains("\"query_block\""), "{plan}");
        assert!(plan.contains("\"table_name\": \"orders\""), "{plan}");

        // With actual values, the statement runs.
        let analyzed = scalar(&session, &dialect.explain(sql, true)).await;
        match engine {
            // MariaDB: the same document with r_* measurements.
            Engine::MariaDb => {
                assert!(analyzed.trim_start().starts_with('{'), "{analyzed}");
                assert!(analyzed.contains("\"r_rows\""), "{analyzed}");
                assert!(analyzed.contains("\"r_total_time_ms\""), "{analyzed}");
            }
            // MySQL: a tree in text, the form EXPLAIN ANALYZE has on every
            // version that knows it.
            _ => {
                assert!(analyzed.trim_start().starts_with("->"), "{analyzed}");
                assert!(analyzed.contains("actual time="), "{analyzed}");
                assert!(analyzed.contains("orders"), "{analyzed}");
            }
        }
    }
}

#[tokio::test]
async fn the_server_cannot_make_the_client_send_a_file() {
    for engine in engines() {
        let session = common::session(engine).await;
        let observer = common::session(engine).await;
        let database = common::fresh_database(&observer, "infile").await;
        exec_ok(&observer, &format!("CREATE TABLE {database}.t (line TEXT)")).await;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("secret.txt");
        std::fs::write(&file, "local file content\n").unwrap();

        // LOAD DATA LOCAL makes the server ask the client for a file, any
        // file. The driver answers no request of that kind: the statement
        // fails and nothing was read or sent.
        let outcome = run(
            &session,
            &format!(
                "LOAD DATA LOCAL INFILE '{}' INTO TABLE {database}.t",
                file.display()
            ),
        )
        .await;
        assert!(outcome.is_err(), "{outcome:?}");
        assert_eq!(
            scalar(&observer, &format!("SELECT COUNT(*) FROM {database}.t")).await,
            "0"
        );
        exec_ok(&observer, &format!("DROP DATABASE {database}")).await;
    }
}

#[tokio::test]
async fn an_interrupted_exchange_never_answers_the_next_statement() {
    for engine in engines() {
        let session = common::session(engine).await;
        // The caller gives up on `execute` itself (not on its results)
        // while the server is still working on the answer.
        let (mut sink, _execution) = exec::channel(RowLimit::All);
        let interrupted = tokio::time::timeout(
            Duration::from_millis(200),
            session.execute("SELECT SLEEP(2), 'stale'", &mut sink),
        )
        .await;
        assert!(
            interrupted.is_err(),
            "the statement should still be running"
        );

        // That answer is still on its way. The next statement must not be
        // given it: the session says it is unusable instead.
        let error = run(&session, "SELECT 42").await.unwrap_err();
        assert!(matches!(error, Error::Closed(_)), "{error:?}");
        assert!(error.to_string().contains("interrupted"), "{error}");
        let error = run(&session, "SELECT 42").await.unwrap_err();
        assert!(matches!(error, Error::Closed(_)), "{error:?}");
        assert!(session.databases().await.is_err());
        assert!(session.ping().await.is_err());
    }
}

#[tokio::test]
async fn closing_a_busy_session_stops_its_statement() {
    for engine in engines() {
        let session = common::session(engine).await;
        let mut execution = exec::run(session.clone(), SLOW.into(), RowLimit::All);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let started = Instant::now();
        session.close().await;
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        assert!(finish(&mut execution).await.is_err());
        let error = run(&session, "SELECT 1").await.unwrap_err();
        assert!(matches!(error, Error::Closed(_)), "{error:?}");
    }
}

#[tokio::test]
async fn a_session_killed_by_the_server_reports_it() {
    for engine in engines() {
        let session = common::session(engine).await;
        let admin = common::session(engine).await;
        let id = scalar(&session, "SELECT CONNECTION_ID()").await;
        exec_ok(&session, "START TRANSACTION").await;
        assert_eq!(session.tx_state(), TxState::Open);

        admin.terminate_session(&id).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let error = run(&session, "SELECT 1").await.unwrap_err();
        assert!(matches!(error, Error::Closed(_)), "{error:?}");
        assert!(error.is_disconnect());
        // The transaction went with the connection.
        assert_eq!(session.tx_state(), TxState::Idle);
        let error = run(&session, "SELECT 1").await.unwrap_err();
        assert!(matches!(error, Error::Closed(_)), "{error:?}");
        assert!(session.ping().await.is_err());

        // Asking again for a session that is gone is an error, not a
        // silent success.
        let error = admin.terminate_session(&id).await.unwrap_err();
        assert_eq!(
            error.db().and_then(|db| db.code.as_deref()),
            Some("1094 (HY000)"),
            "{error:?}"
        );
        assert!(admin.cancel_session(&id).await.is_err());
        let error = admin.cancel_session("not an id").await.unwrap_err();
        assert!(matches!(error, Error::Config(_)), "{error:?}");
        let error = admin
            .terminate_session("1; DROP DATABASE kerno")
            .await
            .unwrap_err();
        assert!(matches!(error, Error::Config(_)), "{error:?}");
    }
}

#[tokio::test]
async fn a_session_killed_while_running_reports_it() {
    for engine in engines() {
        let session = common::session(engine).await;
        let admin = common::session(engine).await;
        let id = scalar(&session, "SELECT CONNECTION_ID()").await;
        let mut execution = exec::run(session.clone(), SLOW.into(), RowLimit::All);
        tokio::time::sleep(Duration::from_millis(300)).await;
        admin.terminate_session(&id).await.unwrap();
        let error = finish(&mut execution).await.unwrap_err();
        assert!(matches!(error, Error::Closed(_)), "{error:?}");
        let error = run(&session, "SELECT 1").await.unwrap_err();
        assert!(matches!(error, Error::Closed(_)), "{error:?}");
    }
}

#[tokio::test]
async fn another_sessions_statement_can_be_cancelled() {
    for engine in engines() {
        let session = common::session(engine).await;
        let admin = common::session(engine).await;
        let id = scalar(&session, "SELECT CONNECTION_ID()").await;

        let mut execution = exec::run(session.clone(), SLOW.into(), RowLimit::All);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let started = Instant::now();
        admin.cancel_session(&id).await.unwrap();
        let error = finish(&mut execution).await.unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(
            error.db().and_then(|db| db.code.as_deref()),
            Some(INTERRUPTED)
        );
        // Cancelled, not disconnected: the same connection goes on.
        assert_eq!(scalar(&session, "SELECT CONNECTION_ID()").await, id);
    }
}
