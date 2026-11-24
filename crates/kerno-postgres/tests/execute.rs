//! Running statements on PostgreSQL: streaming with demand, cancel, every
//! type's text form, notices, errors and transaction state. Runs against
//! the `kerno-postgres` container; skipped with a message when it is down.

mod common;

use std::time::{Duration, Instant};

use common::{db_error, exec_ok, next_event, read_rows, rows, run, scalar};
use kerno_core::exec::{self, ExecEvent, RowLimit};
use kerno_core::{Error, TxState, TypeKind};

const BIG: &str = "SELECT * FROM analytics.big_table ORDER BY id";

#[tokio::test]
async fn rows_stream_only_as_far_as_asked() {
    let Some(session) = common::session().await else {
        return;
    };
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
    // again at once, which it only is if the server stopped sending.
    drop(execution);
    let started = Instant::now();
    assert_eq!(scalar(&session, "SELECT 42").await, "42");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn abandoning_a_result_inside_a_transaction_keeps_the_transaction() {
    let Some(session) = common::session().await else {
        return;
    };
    // Paging through a result and then letting it go must not cost the
    // user their open transaction: a cancel request would abort it.
    exec::collect(session.clone(), "BEGIN").await.unwrap();
    exec::collect(
        session.clone(),
        "CREATE TEMP TABLE kerno_kept (n int) ON COMMIT DROP; INSERT INTO kerno_kept VALUES (7)",
    )
    .await
    .unwrap();
    assert_eq!(session.tx_state(), TxState::Open);

    let mut execution = exec::run(
        session.clone(),
        "SELECT g FROM generate_series(1, 200000) g".into(),
        RowLimit::Rows(100),
    );
    assert!(matches!(
        next_event(&mut execution).await,
        Some(ExecEvent::Columns(_))
    ));
    let (seen, complete) = read_rows(&mut execution, 100).await;
    assert_eq!((seen, complete), (100, false));
    drop(execution);

    // The transaction is still there, with what was done in it.
    assert_eq!(scalar(&session, "SELECT n FROM kerno_kept").await, "7");
    assert_eq!(session.tx_state(), TxState::Open);
    assert!(server_says_in_transaction(&session).await);
    exec::collect(session.clone(), "ROLLBACK").await.unwrap();
    assert_eq!(session.tx_state(), TxState::Idle);
}

#[tokio::test]
async fn fetch_all_delivers_every_row_in_order() {
    let Some(session) = common::session().await else {
        return;
    };
    let mut execution = exec::run(
        session.clone(),
        "SELECT id FROM analytics.big_table WHERE id <= 30000 ORDER BY id".into(),
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

#[tokio::test]
async fn a_result_that_ends_at_the_limit_completes_without_more_demand() {
    let Some(session) = common::session().await else {
        return;
    };
    let mut execution = exec::run(
        session.clone(),
        "SELECT id FROM analytics.big_table WHERE id <= 1000 ORDER BY id".into(),
        RowLimit::Rows(1_000),
    );
    let (seen, complete) = read_rows(&mut execution, usize::MAX).await;
    assert_eq!((seen, complete), (1_000, true));
}

#[tokio::test]
async fn a_running_statement_can_be_cancelled() {
    let Some(session) = common::session().await else {
        return;
    };
    let mut execution = exec::run(session.clone(), "SELECT pg_sleep(60)".into(), RowLimit::All);
    tokio::time::sleep(Duration::from_millis(300)).await;

    let started = Instant::now();
    session.cancel().await.unwrap();
    let error = loop {
        match next_event(&mut execution).await {
            Some(ExecEvent::Finished(result)) => break result.unwrap_err(),
            Some(_) => {}
            None => panic!("ended without Finished"),
        }
    };
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    let db = error.db().expect("the server reports the cancellation");
    assert_eq!(db.code.as_deref(), Some("57014"));
    assert!(db.message.contains("cancel"), "{}", db.message);

    // The session survives, and a cancel with nothing running is harmless.
    assert_eq!(scalar(&session, "SELECT 1").await, "1");
    session.cancel().await.unwrap();
    assert_eq!(scalar(&session, "SELECT 2").await, "2");
}

#[tokio::test]
async fn abandoning_a_statement_that_produces_nothing_cancels_it() {
    let Some(session) = common::session().await else {
        return;
    };
    let observer = common::session().await.unwrap();
    let pid = scalar(&session, "SELECT pg_backend_pid()").await;

    let execution = exec::run(session.clone(), "SELECT pg_sleep(60)".into(), RowLimit::All);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let active = format!("SELECT state FROM pg_stat_activity WHERE pid = {pid}");
    assert_eq!(scalar(&observer, &active).await, "active");

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

#[tokio::test]
async fn the_cancel_of_an_abandoned_result_never_stops_the_next_statement() {
    let Some(session) = common::session().await else {
        return;
    };
    // The server has sent the whole small result and is idle when the
    // consumer lets it go: the cancel request sent then must be over before
    // the next statement starts, or it lands on that one (as a late cancel
    // does in libpq's PQcancel without its wait).
    for round in 0..60 {
        let mut execution = exec::run(
            session.clone(),
            "SELECT g FROM generate_series(1, 5000) g".into(),
            RowLimit::Rows(1_000),
        );
        assert!(matches!(
            next_event(&mut execution).await,
            Some(ExecEvent::Columns(_))
        ));
        let (seen, complete) = read_rows(&mut execution, 1_000).await;
        assert_eq!((seen, complete), (1_000, false));
        drop(execution);
        let next = run(&session, "SELECT pg_sleep(0.05), 'next'::text AS next").await;
        assert!(next.is_ok(), "round {round}: {next:?}");
        // Nor on its description: the columns keep their types (a result
        // read again is drawn with the same header).
        let columns = next.unwrap().results[0].columns.clone();
        assert_eq!(columns[1].type_name, "text", "round {round}");
    }
}

/// How `execution` ends.
async fn ending(execution: &mut exec::Execution) -> kerno_core::Result<()> {
    loop {
        match next_event(execution).await {
            Some(ExecEvent::Finished(result)) => return result,
            Some(_) => {}
            None => panic!("the execution ended without a result"),
        }
    }
}

#[tokio::test]
async fn a_cancel_stops_the_statement_waiting_for_its_turn_too() {
    let Some(session) = common::session().await else {
        return;
    };
    // A holds the session; B waits behind it when the cancel comes (as a
    // statement waits behind a result being let go): B is not sent, rather
    // than run in full after a cancel the idle server would ignore.
    let mut first = exec::run(session.clone(), "SELECT pg_sleep(1)".into(), RowLimit::All);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut second = exec::run(session.clone(), "SELECT pg_sleep(30)".into(), RowLimit::All);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let started = Instant::now();
    session.cancel().await.unwrap();
    let first = ending(&mut first).await.unwrap_err();
    assert_eq!(first.db().and_then(|db| db.code.as_deref()), Some("57014"));
    assert!(matches!(ending(&mut second).await, Err(Error::Cancelled)));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );

    // A cancel of an idle session leaves the next statement alone.
    session.cancel().await.unwrap();
    assert_eq!(
        scalar(&session, "SELECT pg_sleep(0.2)::text || 'next'").await,
        "next"
    );
}

/// K71: a statement that waits for a lock while it is described (the
/// server parses it before it runs it) is stopped by closing its session,
/// and is not sent again once its description was cancelled: it would wait
/// for the lock on the server, unseen, the session closed for Kerno.
#[tokio::test]
async fn closing_stops_a_statement_waiting_for_a_lock_as_it_is_described() {
    let (Some(session), Some(holder), Some(watcher)) = (
        common::session().await,
        common::session().await,
        common::session().await,
    ) else {
        return;
    };
    let schema = common::scratch_schema("close_while_described");
    exec_ok(&holder, &format!("DROP SCHEMA IF EXISTS {schema} CASCADE")).await;
    exec_ok(&holder, &format!("CREATE SCHEMA {schema}")).await;
    exec_ok(&holder, &format!("CREATE TABLE {schema}.t (n int)")).await;
    exec_ok(&holder, "BEGIN").await;
    exec_ok(
        &holder,
        &format!("LOCK TABLE {schema}.t IN ACCESS EXCLUSIVE MODE"),
    )
    .await;
    let waiting = format!(
        "SELECT count(*) FROM pg_stat_activity WHERE wait_event_type = 'Lock' \
         AND query LIKE 'SELECT n FROM {schema}.t%'"
    );
    // Its result is still awaited: nothing but the close stops it.
    let reading = tokio::spawn(exec::collect(
        session.clone(),
        format!("SELECT n FROM {schema}.t"),
    ));
    let deadline = Instant::now() + Duration::from_secs(10);
    while scalar(&watcher, &waiting).await == "0" {
        assert!(Instant::now() < deadline, "the read never waited");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let closed = tokio::time::timeout(Duration::from_secs(5), session.close()).await;
    let mut left = scalar(&watcher, &waiting).await;
    let deadline = Instant::now() + Duration::from_secs(5);
    while left != "0" && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
        left = scalar(&watcher, &waiting).await;
    }
    exec_ok(&holder, "ROLLBACK").await;
    exec_ok(&holder, &format!("DROP SCHEMA {schema} CASCADE")).await;
    assert!(closed.is_ok(), "the close waited for the statement");
    assert_eq!(left, "0", "the statement still waits on the server");
    let read = tokio::time::timeout(Duration::from_secs(5), reading)
        .await
        .expect("the read ended")
        .unwrap();
    assert!(read.is_err(), "{read:?}");
}

#[tokio::test]
async fn every_type_arrives_in_its_text_form() {
    let Some(session) = common::session().await else {
        return;
    };
    let collected = run(&session, "SELECT * FROM public.all_types ORDER BY id")
        .await
        .unwrap();
    let result = &collected.results[0];
    let rows = result.rows();
    assert_eq!(rows.len(), 3);

    let column = |name: &str| {
        result
            .columns
            .iter()
            .position(|c| c.name == name)
            .unwrap_or_else(|| panic!("no column {name}"))
    };
    let kind = |name: &str| result.columns[column(name)].kind;
    let type_name = |name: &str| result.columns[column(name)].type_name.clone();
    let value = |row: usize, name: &str| rows[row][column(name)].clone();
    let text =
        |row: usize, name: &str| value(row, name).unwrap_or_else(|| panic!("{name} is NULL"));

    // Kinds drive rendering and literal quoting.
    for (name, expected) in [
        ("c_smallint", TypeKind::Integer),
        ("c_integer", TypeKind::Integer),
        ("c_bigint", TypeKind::Integer),
        ("c_oid", TypeKind::Integer),
        ("c_numeric", TypeKind::Decimal),
        ("c_money", TypeKind::Decimal),
        ("c_real", TypeKind::Float),
        ("c_double", TypeKind::Float),
        ("c_boolean", TypeKind::Boolean),
        ("c_char", TypeKind::Text),
        ("c_varchar", TypeKind::Text),
        ("c_text", TypeKind::Text),
        ("c_name", TypeKind::Text),
        ("c_xml", TypeKind::Text),
        ("c_bytea", TypeKind::Binary),
        ("c_date", TypeKind::Date),
        ("c_time", TypeKind::Time),
        ("c_timetz", TypeKind::Time),
        ("c_timestamp", TypeKind::DateTime),
        ("c_timestamptz", TypeKind::DateTimeTz),
        ("c_interval", TypeKind::Interval),
        ("c_uuid", TypeKind::Uuid),
        ("c_json", TypeKind::Json),
        ("c_jsonb", TypeKind::Json),
        ("c_int_array", TypeKind::Array),
        ("c_text_array", TypeKind::Array),
        ("c_num_array", TypeKind::Array),
        ("c_bit", TypeKind::Bit),
        ("c_varbit", TypeKind::Bit),
        ("c_enum", TypeKind::Enum),
        ("c_inet", TypeKind::Other),
        ("c_int4range", TypeKind::Other),
        ("c_point", TypeKind::Other),
        ("c_tsvector", TypeKind::Other),
        ("c_composite", TypeKind::Other),
        ("c_hstore", TypeKind::Other),
    ] {
        assert_eq!(kind(name), expected, "{name}");
    }
    assert_eq!(type_name("c_bigint"), "int8");
    assert_eq!(type_name("c_int_array"), "int4[]");
    assert_eq!(type_name("c_enum"), "mood");
    assert_eq!(type_name("c_hstore"), "hstore");

    // Row 1: ordinary values, exactly as psql would print them.
    assert_eq!(text(0, "c_smallint"), "12345");
    assert_eq!(text(0, "c_bigint"), "9223372036854775807");
    assert_eq!(text(0, "c_numeric"), "12345.678901");
    assert_eq!(text(0, "c_numeric_any"), "0.1");
    assert_eq!(text(0, "c_real"), "3.14");
    assert_eq!(text(0, "c_double"), "2.718281828459045");
    assert_eq!(text(0, "c_boolean"), "t");
    assert_eq!(text(0, "c_char"), "abc  ");
    assert_eq!(
        text(0, "c_text"),
        "Multi-line\ntext with \"quotes\", a tab\tand unicode: caf\u{e9} \u{2603} \u{1F600}"
    );
    assert_eq!(text(0, "c_date"), "2024-02-29");
    assert_eq!(text(0, "c_time"), "13:37:42.123456");
    assert_eq!(text(0, "c_timetz"), "13:37:42+02");
    assert_eq!(text(0, "c_timestamp"), "2024-02-29 13:37:42.123456");
    assert_eq!(text(0, "c_timestamptz"), "2024-02-29 13:37:42.123456+00");
    assert_eq!(text(0, "c_interval"), "1 year 2 mons 3 days 04:05:06.789");
    assert_eq!(text(0, "c_uuid"), "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11");
    assert_eq!(
        text(0, "c_json"),
        r#"{"a": 1, "b": [true, null], "c": {"d": "e"}}"#
    );
    assert_eq!(
        text(0, "c_jsonb"),
        r#"{"a": 1, "b": [true, null], "c": {"d": "e"}}"#
    );
    assert_eq!(text(0, "c_xml"), r#"<root><item id="1">x</item></root>"#);
    assert_eq!(text(0, "c_inet"), "192.168.1.10/24");
    assert_eq!(text(0, "c_cidr"), "10.0.0.0/8");
    assert_eq!(text(0, "c_macaddr"), "08:00:2b:01:02:03");
    assert_eq!(text(0, "c_int_array"), "{1,2,NULL,4}");
    assert_eq!(
        text(0, "c_text_array"),
        r#"{a,"b c","with \"quote\"",NULL}"#
    );
    assert_eq!(text(0, "c_num_array"), "{1.5,2.25}");
    assert_eq!(text(0, "c_int4range"), "[1,10)");
    assert_eq!(text(0, "c_daterange"), "[2024-01-01,2024-02-01)");
    assert_eq!(text(0, "c_point"), "(1.5,-2.5)");
    assert_eq!(text(0, "c_bit"), "10101010");
    assert_eq!(text(0, "c_varbit"), "1011");
    assert_eq!(text(0, "c_enum"), "happy");
    assert_eq!(text(0, "c_composite"), r#"("1 Main St",Paris,75001,FR)"#);
    assert_eq!(text(0, "c_oid"), "16384");

    // bytea: the bytes themselves, not their hex spelling.
    let bytea = column("c_bytea");
    assert_eq!(
        result.batches[0].cell(0, bytea),
        Some(&[0xde, 0xad, 0xbe, 0xef, 0x00, 0xff][..])
    );
    assert_eq!(result.batches[0].cell(2, bytea), Some(&[][..]));

    // Row 2: NULL everywhere but the key, and NULL is not the empty string.
    for (index, cell) in rows[1].iter().enumerate() {
        if index == column("id") {
            assert_eq!(cell.as_deref(), Some("2"));
        } else {
            assert_eq!(cell, &None, "{}", result.columns[index].name);
        }
    }

    // Row 3: edge values survive untouched.
    assert_eq!(text(2, "c_smallint"), "-32768");
    assert_eq!(text(2, "c_bigint"), "-9223372036854775808");
    assert_eq!(text(2, "c_numeric_any"), "NaN");
    assert_eq!(text(2, "c_real"), "Infinity");
    assert_eq!(text(2, "c_double"), "-Infinity");
    assert_eq!(text(2, "c_boolean"), "f");
    assert_eq!(text(2, "c_text"), "");
    assert_eq!(text(2, "c_varchar"), "");
    assert_eq!(text(2, "c_date"), "infinity");
    assert_eq!(text(2, "c_timestamp"), "-infinity");
    assert_eq!(text(2, "c_interval"), "-1 days");
    assert_eq!(text(2, "c_json"), "null");
    assert_eq!(text(2, "c_jsonb"), "[]");
    assert_eq!(text(2, "c_int_array"), "{}");
    assert_eq!(text(2, "c_int4range"), "empty");
    assert_eq!(text(2, "c_composite"), "(,,,)");
    assert_eq!(text(2, "c_varbit"), "");
}

#[tokio::test]
async fn commands_report_their_row_counts() {
    let Some(session) = common::session().await else {
        return;
    };
    let schema = common::scratch_schema("counts");
    exec_ok(&session, &format!("DROP SCHEMA IF EXISTS {schema} CASCADE")).await;
    exec_ok(&session, &format!("CREATE SCHEMA {schema}")).await;
    exec_ok(
        &session,
        &format!("CREATE TABLE {schema}.t (id int PRIMARY KEY, v text)"),
    )
    .await;

    let inserted = run(
        &session,
        &format!("INSERT INTO {schema}.t SELECT g, 'v' || g FROM generate_series(1, 5) g"),
    )
    .await
    .unwrap();
    assert_eq!(inserted.results.len(), 1);
    assert!(inserted.results[0].columns.is_empty());
    assert_eq!(inserted.results[0].rows_affected, Some(5));

    let updated = run(
        &session,
        &format!("UPDATE {schema}.t SET v = 'x' WHERE id > 3 RETURNING id, v"),
    )
    .await
    .unwrap();
    assert_eq!(updated.results[0].rows_affected, Some(2));
    assert_eq!(updated.results[0].columns.len(), 2);
    assert_eq!(updated.results[0].columns[0].kind, TypeKind::Integer);
    assert_eq!(updated.results[0].columns[1].kind, TypeKind::Text);
    assert_eq!(
        updated.results[0].rows(),
        vec![
            vec![Some("4".into()), Some("x".into())],
            vec![Some("5".into()), Some("x".into())]
        ]
    );

    let deleted = run(&session, &format!("DELETE FROM {schema}.t WHERE id = 1"))
        .await
        .unwrap();
    assert_eq!(deleted.results[0].rows_affected, Some(1));

    // A select with no rows still has its columns.
    let empty = run(
        &session,
        &format!("SELECT id, v FROM {schema}.t WHERE false"),
    )
    .await
    .unwrap();
    assert_eq!(empty.results[0].columns.len(), 2);
    assert_eq!(empty.results[0].row_count(), 0);
    assert_eq!(empty.results[0].rows_affected, Some(0));

    exec_ok(&session, &format!("DROP SCHEMA {schema} CASCADE")).await;
}

#[tokio::test]
async fn several_statements_in_one_request_give_several_results() {
    let Some(session) = common::session().await else {
        return;
    };
    let collected = run(&session, "SELECT 1 AS a; SELECT 'x' AS b, 2 AS c")
        .await
        .unwrap();
    assert_eq!(collected.results.len(), 2);
    assert_eq!(collected.results[0].rows(), vec![vec![Some("1".into())]]);
    assert_eq!(collected.results[1].columns[0].name, "b");
    assert_eq!(
        collected.results[1].rows(),
        vec![vec![Some("x".into()), Some("2".into())]]
    );

    // Nothing to run is not an error. The server answers an empty query
    // with EmptyQueryResponse, which tokio-postgres reports as a command
    // that affected 0 rows, so that is what the driver can say. (The app
    // never sends these: the statement splitter drops empty statements.)
    for sql in ["", "   ", "-- just a comment", ";"] {
        let collected = run(&session, sql)
            .await
            .unwrap_or_else(|e| panic!("{sql:?}: {e}"));
        assert_eq!(collected.results.len(), 1, "{sql:?}");
        assert!(collected.results[0].columns.is_empty(), "{sql:?}");
        assert_eq!(collected.results[0].rows_affected, Some(0), "{sql:?}");
    }
}

#[tokio::test]
async fn duplicate_and_unnamed_columns_are_kept() {
    let Some(session) = common::session().await else {
        return;
    };
    let collected = run(&session, "SELECT 1 AS x, 2 AS x, 3, NULL")
        .await
        .unwrap();
    let result = &collected.results[0];
    let names: Vec<_> = result.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["x", "x", "?column?", "?column?"]);
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

#[tokio::test]
async fn notices_and_warnings_are_delivered() {
    let Some(session) = common::session().await else {
        return;
    };
    let collected = run(&session, "SELECT shop.slow_notice(0.01)")
        .await
        .unwrap();
    assert_eq!(collected.results[0].rows(), vec![vec![Some("done".into())]]);
    let seen: Vec<(String, String)> = collected
        .notices
        .iter()
        .map(|n| (n.severity.clone(), n.message.clone()))
        .collect();
    assert_eq!(
        seen,
        [
            ("NOTICE".to_string(), "sleeping for 0.01 s".to_string()),
            ("WARNING".to_string(), "slept for 0.01 s".to_string()),
        ]
    );

    // Notices from DDL, and none left over for the next statement.
    let collected = run(&session, "DROP TABLE IF EXISTS public.kerno_never_existed")
        .await
        .unwrap();
    assert_eq!(collected.notices.len(), 1);
    assert!(
        collected.notices[0].message.contains("does not exist"),
        "{:?}",
        collected.notices
    );
    assert_eq!(collected.notices[0].code.as_deref(), Some("00000"));
    let collected = run(&session, "SELECT 1").await.unwrap();
    assert!(collected.notices.is_empty(), "{:?}", collected.notices);
}

#[tokio::test]
async fn server_errors_carry_code_position_detail_and_hint() {
    let Some(session) = common::session().await else {
        return;
    };

    let error = db_error(&session, "SELEC 1").await;
    assert_eq!(error.code.as_deref(), Some("42601"));
    assert_eq!(error.position, Some(1));
    assert_eq!(error.severity.as_deref(), Some("ERROR"));

    let error = db_error(&session, "SELECT *\nFROM no_such_table").await;
    assert_eq!(error.code.as_deref(), Some("42P01"));
    assert_eq!(error.position, Some(15));
    assert!(error.message.contains("no_such_table"), "{}", error.message);

    let error = db_error(&session, "SELECT full_nam FROM shop.customers").await;
    assert_eq!(error.code.as_deref(), Some("42703"));
    assert_eq!(error.position, Some(8));
    assert!(
        error
            .hint
            .as_deref()
            .is_some_and(|hint| hint.contains("full_name")),
        "{:?}",
        error.hint
    );

    let error = db_error(
        &session,
        "INSERT INTO shop.categories (id, name, slug) VALUES (1, 'dup', 'dup')",
    )
    .await;
    assert_eq!(error.code.as_deref(), Some("23505"));
    assert!(
        error
            .detail
            .as_deref()
            .is_some_and(|d| d.contains("(id)=(1)")),
        "{:?}",
        error.detail
    );

    // A runtime error after rows started: the rows delivered stand, then the error.
    let mut execution = exec::run(
        session.clone(),
        "SELECT 10 / (3 - g) FROM generate_series(1, 5) g".into(),
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
    assert!(seen <= 2, "{seen}");
    assert_eq!(error.db().and_then(|db| db.code.as_deref()), Some("22012"));

    // The session is fine afterwards.
    assert_eq!(scalar(&session, "SELECT 1").await, "1");
}

/// What the server itself says about the transaction block: inside one,
/// the transaction started before the current statement.
async fn server_says_in_transaction(session: &std::sync::Arc<dyn kerno_core::Session>) -> bool {
    scalar(session, "SELECT (now() <> statement_timestamp())::text").await == "true"
}

#[tokio::test]
async fn transaction_state_follows_the_server() {
    let Some(session) = common::session().await else {
        return;
    };
    let schema = common::scratch_schema("tx");
    exec_ok(&session, &format!("DROP SCHEMA IF EXISTS {schema} CASCADE")).await;
    exec_ok(&session, &format!("CREATE SCHEMA {schema}")).await;
    exec_ok(
        &session,
        &format!("CREATE TABLE {schema}.t (id int PRIMARY KEY)"),
    )
    .await;
    assert_eq!(session.tx_state(), TxState::Idle);
    assert!(!server_says_in_transaction(&session).await);

    // Commit keeps the rows.
    exec_ok(&session, "BEGIN").await;
    assert_eq!(session.tx_state(), TxState::Open);
    exec_ok(&session, &format!("INSERT INTO {schema}.t VALUES (1)")).await;
    assert!(server_says_in_transaction(&session).await);
    assert_eq!(session.tx_state(), TxState::Open);
    exec_ok(&session, "COMMIT").await;
    assert_eq!(session.tx_state(), TxState::Idle);
    assert!(!server_says_in_transaction(&session).await);
    assert_eq!(
        scalar(&session, &format!("SELECT count(*) FROM {schema}.t")).await,
        "1"
    );

    // Rollback discards them; another session never saw them.
    let other = common::session().await.unwrap();
    exec_ok(&session, "start transaction").await;
    exec_ok(&session, &format!("INSERT INTO {schema}.t VALUES (2)")).await;
    assert_eq!(
        scalar(&other, &format!("SELECT count(*) FROM {schema}.t")).await,
        "1"
    );
    exec_ok(&session, "ROLLBACK").await;
    assert_eq!(session.tx_state(), TxState::Idle);
    assert_eq!(
        scalar(&session, &format!("SELECT count(*) FROM {schema}.t")).await,
        "1"
    );

    // An error inside a block aborts it until rollback.
    exec_ok(&session, "BEGIN").await;
    let error = db_error(&session, &format!("INSERT INTO {schema}.t VALUES (1)")).await;
    assert_eq!(error.code.as_deref(), Some("23505"));
    assert_eq!(session.tx_state(), TxState::Failed);
    let error = db_error(&session, "SELECT 1").await;
    assert_eq!(error.code.as_deref(), Some("25P02"));
    assert_eq!(session.tx_state(), TxState::Failed);
    exec_ok(&session, "ROLLBACK").await;
    assert_eq!(session.tx_state(), TxState::Idle);
    assert!(!server_says_in_transaction(&session).await);

    // Savepoints recover an aborted block.
    exec_ok(&session, "BEGIN").await;
    exec_ok(&session, "SAVEPOINT s").await;
    let _ = db_error(&session, "SELECT 1 / 0").await;
    assert_eq!(session.tx_state(), TxState::Failed);
    exec_ok(&session, "ROLLBACK TO SAVEPOINT s").await;
    assert_eq!(session.tx_state(), TxState::Open);
    assert!(server_says_in_transaction(&session).await);
    exec_ok(&session, "COMMIT").await;
    assert_eq!(session.tx_state(), TxState::Idle);

    // An error outside a block leaves no block behind.
    let _ = db_error(&session, "SELECT 1 / 0").await;
    assert_eq!(session.tx_state(), TxState::Idle);
    assert!(!server_says_in_transaction(&session).await);

    // Cancelling inside a block aborts it.
    exec_ok(&session, "BEGIN").await;
    let mut execution = exec::run(session.clone(), "SELECT pg_sleep(60)".into(), RowLimit::All);
    tokio::time::sleep(Duration::from_millis(200)).await;
    session.cancel().await.unwrap();
    while let Some(event) = next_event(&mut execution).await {
        if let ExecEvent::Finished(result) = event {
            assert!(matches!(result, Err(Error::Db(_))), "{result:?}");
        }
    }
    assert_eq!(session.tx_state(), TxState::Failed);
    exec_ok(&session, "ROLLBACK").await;

    exec_ok(&session, &format!("DROP SCHEMA {schema} CASCADE")).await;
}

#[tokio::test]
async fn a_closed_session_reports_it() {
    let Some(session) = common::session().await else {
        return;
    };
    assert_eq!(scalar(&session, "SELECT 1").await, "1");
    session.close().await;
    let error = run(&session, "SELECT 1").await.unwrap_err();
    assert!(matches!(error, Error::Closed(_)), "{error:?}");
    assert!(error.is_disconnect());
    assert!(session.ping().await.is_err());
}

#[tokio::test]
async fn a_session_killed_by_the_server_reports_it() {
    let Some(session) = common::session().await else {
        return;
    };
    let admin = common::session().await.unwrap();
    let pid = scalar(&session, "SELECT pg_backend_pid()").await;

    admin.terminate_session(&pid).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let error = run(&session, "SELECT 1").await.unwrap_err();
    assert!(matches!(error, Error::Closed(_)), "{error:?}");

    // Asking again for a session that is gone is an error, not a silent success.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(admin.terminate_session(&pid).await.is_err());
    assert!(admin.cancel_session("not a pid").await.is_err());
}

#[tokio::test]
async fn another_sessions_statement_can_be_cancelled() {
    let Some(session) = common::session().await else {
        return;
    };
    let admin = common::session().await.unwrap();
    let pid = scalar(&session, "SELECT pg_backend_pid()").await;

    let mut execution = exec::run(session.clone(), "SELECT pg_sleep(60)".into(), RowLimit::All);
    tokio::time::sleep(Duration::from_millis(300)).await;
    admin.cancel_session(&pid).await.unwrap();
    let error = loop {
        match next_event(&mut execution).await {
            Some(ExecEvent::Finished(result)) => break result.unwrap_err(),
            Some(_) => {}
            None => panic!("ended without Finished"),
        }
    };
    assert_eq!(error.db().and_then(|db| db.code.as_deref()), Some("57014"));
    // Cancelled, not disconnected.
    assert_eq!(scalar(&session, "SELECT 1").await, "1");
    let _ = rows(&session, "SELECT 1").await;
}

#[tokio::test]
async fn the_isolation_level_is_set_for_this_session_only() {
    use kerno_core::IsolationLevel;
    let Some(session) = common::session().await else {
        return;
    };
    let Some(other) = common::session().await else {
        return;
    };
    let default = exec::isolation(other.clone()).await.unwrap();
    assert_eq!(default, Some(IsolationLevel::ReadCommitted));
    for &level in session.dialect().isolation_levels() {
        let now = exec::set_isolation(session.clone(), level).await.unwrap();
        assert_eq!(now, Some(level));
        // The session's next transaction runs at that level.
        exec_ok(&session, "BEGIN").await;
        assert_eq!(
            IsolationLevel::parse(&scalar(&session, "SHOW transaction_isolation").await),
            Some(level)
        );
        exec_ok(&session, "ROLLBACK").await;
    }
    // Another session keeps the server's default.
    assert_eq!(exec::isolation(other.clone()).await.unwrap(), default);
    session.close().await;
    other.close().await;
}

/// C6: the session's schema follows `SET search_path`, `SET LOCAL` and the
/// end of the transaction that undoes them; NULL when no schema of the
/// path exists.
#[tokio::test]
async fn the_schema_follows_set_search_path() {
    let Some(session) = common::session().await else {
        return;
    };
    let schema = common::scratch_schema("search_path");
    exec_ok(&session, &format!("DROP SCHEMA IF EXISTS {schema} CASCADE")).await;
    exec_ok(&session, &format!("CREATE SCHEMA {schema}")).await;
    // Read at connect.
    assert_eq!(session.schema().as_deref(), Some("public"));

    exec_ok(&session, &format!("SET search_path TO {schema}, public")).await;
    assert_eq!(session.schema().as_deref(), Some(schema.as_str()));
    exec_ok(&session, "RESET search_path").await;
    assert_eq!(session.schema().as_deref(), Some("public"));

    // A `SET LOCAL` lasts until the transaction ends.
    exec_ok(&session, "BEGIN").await;
    exec_ok(&session, &format!("SET LOCAL search_path TO {schema}")).await;
    assert_eq!(session.schema().as_deref(), Some(schema.as_str()));
    exec_ok(&session, "ROLLBACK").await;
    assert_eq!(session.schema().as_deref(), Some("public"));

    // A plain `SET` inside a block that rolls back is undone too.
    exec_ok(&session, "BEGIN").await;
    exec_ok(&session, &format!("SET search_path TO {schema}")).await;
    assert_eq!(session.schema().as_deref(), Some(schema.as_str()));
    exec_ok(&session, "ROLLBACK").await;
    assert_eq!(session.schema().as_deref(), Some("public"));

    // Through set_config, and the first schema of the path that exists.
    exec_ok(
        &session,
        &format!("SELECT set_config('search_path', 'kerno_test_missing, {schema}', false)"),
    )
    .await;
    assert_eq!(session.schema().as_deref(), Some(schema.as_str()));
    // No schema of the path exists: none.
    exec_ok(&session, "SET search_path TO kerno_test_missing").await;
    assert_eq!(session.schema(), None);

    exec_ok(&session, "SET search_path TO public").await;
    exec_ok(&session, &format!("DROP SCHEMA {schema} CASCADE")).await;
    session.close().await;
}

/// Asks `gone` every 100 ms, for at most `limit`, until it reports.
async fn gone_within(
    session: &std::sync::Arc<dyn kerno_core::Session>,
    limit: Duration,
) -> Option<kerno_core::DbError> {
    let started = Instant::now();
    while started.elapsed() < limit {
        if let Some(why) = session.gone().await {
            return Some(why);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    None
}

/// C15: any command restarts the server's idle timers, so a ping on the
/// tab's session would keep a forgotten transaction, and its locks, past
/// the DBA's `idle_in_transaction_session_timeout`. `gone` sends nothing:
/// watched with it, the session is still closed by the server, and says why.
#[tokio::test]
async fn watching_a_session_does_not_keep_it_alive() {
    let Some(session) = common::session().await else {
        return;
    };
    exec_ok(&session, "SET idle_in_transaction_session_timeout = '1s'").await;
    exec_ok(&session, "BEGIN").await;
    // Pinged more often than the timeout, it stays open long after it.
    for _ in 0..6 {
        tokio::time::sleep(Duration::from_millis(400)).await;
        session.ping().await.unwrap();
    }
    assert!(session.gone().await.is_none());
    assert_eq!(session.tx_state(), TxState::Open);

    let why = gone_within(&session, Duration::from_secs(10))
        .await
        .expect("the server closes the session");
    assert_eq!(why.code.as_deref(), Some("25P03"), "{why:?}");
    assert!(why.message.contains("idle-in-transaction"), "{why:?}");
    // The server rolled the transaction back with it.
    assert_eq!(session.tx_state(), TxState::Idle);
    let error = run(&session, "SELECT 1").await.unwrap_err();
    assert!(error.is_disconnect(), "{error:?}");
    assert!(error.to_string().contains("[25P03]"), "{error}");
}

/// C15: the other ways the server closes one session while the others
/// answer: a transaction too long, a session idle too long, an
/// administrator's terminate. Each is seen without touching the session.
#[tokio::test]
async fn a_session_the_server_closes_says_why() {
    let Some(admin) = common::session().await else {
        return;
    };
    for (setting, begin, code) in [
        ("SET transaction_timeout = '1s'", true, "25P04"),
        ("SET idle_session_timeout = '1s'", false, "57P05"),
        ("", true, "57P01"),
    ] {
        let session = common::session().await.unwrap();
        if !setting.is_empty() {
            exec_ok(&session, setting).await;
        }
        if begin {
            exec_ok(&session, "BEGIN").await;
        }
        assert!(session.gone().await.is_none(), "{code}");
        if setting.is_empty() {
            let pid = scalar(&session, "SELECT pg_backend_pid()").await;
            admin.terminate_session(&pid).await.unwrap();
        }
        let why = gone_within(&session, Duration::from_secs(10))
            .await
            .unwrap_or_else(|| panic!("{code}: not seen"));
        assert_eq!(why.code.as_deref(), Some(code), "{why:?}");
        assert_eq!(session.tx_state(), TxState::Idle, "{code}");
    }
    // Kerno's own close is not the server's doing, but the session is gone.
    let session = common::session().await.unwrap();
    session.close().await;
    assert!(session.gone().await.is_some_and(|why| why.code.is_none()));
}
