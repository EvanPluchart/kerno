//! Copying tables with their data to another connection (CLAUDE.md §4 J13,
//! DBeaver #17767): the copy itself across PostgreSQL, MySQL and MariaDB
//! (here, where both drivers are), and the wizard end to end.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};

use kerno_core::transfer::{
    self, IfExists, TableStatus, TableTransfer, TransferEvent, TransferMode, TransferPlan,
    TransferReport,
};
use kerno_core::{Driver as _, Session};

use super::*;
use crate::navigator::actions::ToolEntry;
use crate::transfer_dialog::{self, TransferDialog};

// The copy, without the window ----------------------------------------------

/// Runs `work` on a runtime of its own, as the app's bridge does.
fn block_on<T>(work: impl Future<Output = T>) -> T {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(work)
}

async fn session(engine: Engine) -> Arc<dyn Session> {
    let params = testing::params(engine);
    match engine {
        Engine::Postgres => kerno_postgres::PostgresDriver.connect(&params).await,
        Engine::MySql | Engine::MariaDb => kerno_mysql::MysqlDriver.connect(&params).await,
    }
    .expect("the test server")
}

/// Every row `sql` returns on `session`.
async fn rows(session: &Arc<dyn Session>, sql: &str) -> Vec<Vec<Option<String>>> {
    kerno_core::exec::collect(session.clone(), sql)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
        .results
        .last()
        .map(|result| result.rows())
        .unwrap_or_default()
}

/// The default plan for `tables` of `source`, into `schema` of `target`.
async fn plan_for(
    source: &Arc<dyn Session>,
    target: &Arc<dyn Session>,
    tables: &[TableRef],
    schema: &str,
) -> TransferPlan {
    let mut transfers = Vec::new();
    for table in tables {
        let structure = source.table_structure(table).await.unwrap();
        transfers.push(TableTransfer::new(
            source.engine(),
            target.engine(),
            table.clone(),
            &structure,
        ));
    }
    TransferPlan::new(transfers, schema)
}

/// Copies `plan` and says how fast it went, in rows per second.
async fn copy(
    source: &Arc<dyn Session>,
    target: &Arc<dyn Session>,
    plan: TransferPlan,
    cancel: Arc<AtomicBool>,
    progress: impl Fn(TransferEvent) + Send + Sync + 'static,
) -> TransferReport {
    transfer::run(source.clone(), target.clone(), plan, progress, cancel)
        .await
        .expect("the copy starts")
}

fn rate(report: &TransferReport) -> f64 {
    let table = &report.tables[0];
    table.written as f64 / table.elapsed.as_secs_f64()
}

const PG_TYPES: &str = "kerno_test_transfer_types";

/// A PostgreSQL table with one column per mapped type, a row of values and
/// a row of NULLs.
fn typed_source() -> Scratch {
    Scratch::new(
        Engine::Postgres,
        &[&format!("DROP SCHEMA IF EXISTS {PG_TYPES} CASCADE")],
        &[
            &format!("CREATE SCHEMA {PG_TYPES}"),
            &format!("CREATE TYPE {PG_TYPES}.mood AS ENUM ('sad', 'happy')"),
            &format!(
                "CREATE TABLE {PG_TYPES}.typed (
                    id integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
                    c_smallint smallint, c_integer integer, c_bigint bigint,
                    c_numeric numeric(12,3), c_real real, c_double double precision,
                    c_bool boolean, c_varchar varchar(40), c_char char(4), c_text text,
                    c_bytea bytea, c_date date, c_time time, c_ts timestamp,
                    c_tstz timestamptz, c_uuid uuid, c_json jsonb,
                    c_enum {PG_TYPES}.mood, c_bit bit(4))"
            ),
            &format!(
                "INSERT INTO {PG_TYPES}.typed (c_smallint, c_integer, c_bigint, c_numeric, \
                 c_real, c_double, c_bool, c_varchar, c_char, c_text, c_bytea, c_date, c_time, \
                 c_ts, c_tstz, c_uuid, c_json, c_enum, c_bit) VALUES (-12, 2147483647, \
                 -9223372036854775808, 12345.678, 1.5, 2.25, true, 'it''s \"quoted\" \\ back', \
                 'ab', E'line1\\nline2 café', '\\xdeadbeef00', '2024-02-29', '13:37:42.123456', \
                 '2024-02-29 13:37:42.123456', '2024-02-29 13:37:42.123456+02', \
                 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11', '{{\"a\": 1, \"b\": [true, null]}}', \
                 'happy', B'1010')"
            ),
            &format!("INSERT INTO {PG_TYPES}.typed DEFAULT VALUES"),
        ],
    )
}

/// DBeaver #17767: every mapped PostgreSQL type, and NULLs, reach MySQL,
/// MariaDB and PostgreSQL with their values.
#[test]
fn postgres_types_and_nulls_are_copied_to_mysql_mariadb_and_postgres() {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _source = typed_source();
    let table = TableRef::new(PG_TYPES, "typed");
    for (engine, target_schema) in [
        (Engine::MySql, "kerno_test_transfer_my"),
        (Engine::MariaDb, "kerno_test_transfer_maria"),
    ] {
        if !testing::available(engine) {
            continue;
        }
        let _target = Scratch::new(
            engine,
            &[&format!("DROP DATABASE IF EXISTS {target_schema}")],
            &[&format!("CREATE DATABASE {target_schema}")],
        );
        let table = table.clone();
        block_on(async move {
            let (source, target) = (session(Engine::Postgres).await, session(engine).await);
            let plan = plan_for(&source, &target, &[table], target_schema).await;
            assert_eq!(plan.problems(), Vec::<String>::new());
            let report = copy(&source, &target, plan, Arc::default(), |_| {}).await;
            assert!(report.succeeded(), "{engine:?}: {}", report.text());
            assert_eq!(report.written(), 2);
            let read = rows(
                &target,
                &format!(
                    "SELECT id, c_smallint, c_integer, c_bigint, c_numeric, c_real, c_double, \
                     c_bool, c_varchar, c_char, c_text, HEX(c_bytea), c_date, c_time, c_ts, \
                     c_tstz, c_uuid, JSON_EXTRACT(c_json, '$.b[0]'), c_enum, BIN(c_bit) \
                     FROM {target_schema}.typed ORDER BY id"
                ),
            )
            .await;
            let expected: Vec<Option<String>> = [
                "1",
                "-12",
                "2147483647",
                "-9223372036854775808",
                "12345.678",
                "1.5",
                "2.25",
                "1",
                "it's \"quoted\" \\ back",
                "ab",
                "line1\nline2 café",
                "DEADBEEF00",
                "2024-02-29",
                "13:37:42.123456",
                "2024-02-29 13:37:42.123456",
                // Converted to UTC.
                "2024-02-29 11:37:42.123456",
                "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11",
                "true",
                "happy",
                "1010",
            ]
            .iter()
            .map(|value| Some(value.to_string()))
            .collect();
            assert_eq!(read[0], expected, "{engine:?}");
            assert_eq!(read[1][0].as_deref(), Some("2"));
            assert!(
                read[1][1..].iter().all(Option::is_none),
                "{engine:?}: {:?}",
                read[1]
            );
            source.close().await;
            target.close().await;
        });
    }

    // PostgreSQL to PostgreSQL: the same rows, types kept, and the identity
    // goes on after the copied ids.
    let _target = Scratch::new(
        Engine::Postgres,
        &["DROP SCHEMA IF EXISTS kerno_test_transfer_pg CASCADE"],
        &["CREATE SCHEMA kerno_test_transfer_pg"],
    );
    block_on(async move {
        let (source, target) = (
            session(Engine::Postgres).await,
            session(Engine::Postgres).await,
        );
        let plan = plan_for(&source, &target, &[table], "kerno_test_transfer_pg").await;
        let report = copy(&source, &target, plan, Arc::default(), |_| {}).await;
        assert!(report.succeeded(), "{}", report.text());
        let differ = rows(
            &target,
            &format!(
                "SELECT count(*) FROM (SELECT * FROM {PG_TYPES}.typed EXCEPT ALL \
                 SELECT * FROM kerno_test_transfer_pg.typed) d"
            ),
        )
        .await;
        assert_eq!(differ[0][0].as_deref(), Some("0"));
        let next = rows(
            &target,
            "INSERT INTO kerno_test_transfer_pg.typed (c_text) VALUES ('next') RETURNING id",
        )
        .await;
        assert_eq!(next[0][0].as_deref(), Some("3"));
        source.close().await;
        target.close().await;
    });
}

const MY_TYPES: &str = "kerno_test_transfer_mysrc";

/// DBeaver #17767: every mapped MySQL type, and NULLs, reach PostgreSQL.
#[test]
fn mysql_types_and_nulls_are_copied_to_postgres() {
    if !testing::available(Engine::MySql) || !testing::available(Engine::Postgres) {
        return;
    }
    let _source = Scratch::new(
        Engine::MySql,
        &[&format!("DROP DATABASE IF EXISTS {MY_TYPES}")],
        &[
            &format!("CREATE DATABASE {MY_TYPES}"),
            &format!(
                "CREATE TABLE {MY_TYPES}.typed (
                    id int AUTO_INCREMENT PRIMARY KEY, c_tiny tinyint, c_bool tinyint(1),
                    c_small smallint unsigned, c_medium mediumint, c_int int unsigned,
                    c_big bigint unsigned, c_dec decimal(10,2), c_float float, c_double double,
                    c_bit bit(4), c_char char(3), c_varchar varchar(50), c_text mediumtext,
                    c_bin varbinary(8), c_blob blob, c_date date, c_time time(3),
                    c_dt datetime(6), c_ts timestamp NULL, c_json json,
                    c_enum enum('a','bb'), c_set set('x','y'), c_year year)"
            ),
            &format!(
                "INSERT INTO {MY_TYPES}.typed (c_tiny, c_bool, c_small, c_medium, c_int, c_big, \
                 c_dec, c_float, c_double, c_bit, c_char, c_varchar, c_text, c_bin, c_blob, \
                 c_date, c_time, c_dt, c_ts, c_json, c_enum, c_set, c_year) VALUES (-5, 1, \
                 65535, -8388608, 4294967295, 18446744073709551615, 12.34, 1.5, 2.25, b'1010', \
                 'ab', 'it''s a \\\\ x', 'multi\\nline é', X'0001ff', X'deadbeef', '2024-02-29', \
                 '13:37:42.123', '2024-02-29 13:37:42.123456', '2024-02-29 13:37:42', \
                 '{{\"a\": 1}}', 'bb', 'x,y', 2024)"
            ),
            &format!("INSERT INTO {MY_TYPES}.typed () VALUES ()"),
        ],
    );
    let _target = Scratch::new(
        Engine::Postgres,
        &["DROP SCHEMA IF EXISTS kerno_test_transfer_frommy CASCADE"],
        &["CREATE SCHEMA kerno_test_transfer_frommy"],
    );
    block_on(async {
        let (source, target) = (
            session(Engine::MySql).await,
            session(Engine::Postgres).await,
        );
        let table = TableRef::new(MY_TYPES, "typed");
        let plan = plan_for(&source, &target, &[table], "kerno_test_transfer_frommy").await;
        assert_eq!(plan.problems(), Vec::<String>::new());
        let report = copy(&source, &target, plan, Arc::default(), |_| {}).await;
        assert!(report.succeeded(), "{}", report.text());
        let read = rows(
            &target,
            "SELECT id::text, c_tiny::text, c_bool::text, c_small::text, c_medium::text, \
             c_int::text, c_big::text, c_dec::text, c_float::text, c_double::text, \
             c_bit::text, rtrim(c_char), c_varchar, c_text, encode(c_bin, 'hex'), \
             encode(c_blob, 'hex'), c_date::text, c_time::text, c_dt::text, c_ts::text, \
             c_json->>'a', c_enum, c_set, c_year::text \
             FROM kerno_test_transfer_frommy.typed ORDER BY id",
        )
        .await;
        let expected: Vec<Option<String>> = [
            "1",
            "-5",
            "true",
            "65535",
            "-8388608",
            "4294967295",
            "18446744073709551615",
            "12.34",
            "1.5",
            "2.25",
            "1010",
            "ab",
            "it's a \\ x",
            "multi\nline é",
            "0001ff",
            "deadbeef",
            "2024-02-29",
            "13:37:42.123",
            "2024-02-29 13:37:42.123456",
            "2024-02-29 13:37:42",
            "1",
            "bb",
            "x,y",
            "2024",
        ]
        .iter()
        .map(|value| Some(value.to_string()))
        .collect();
        assert_eq!(read[0], expected);
        assert_eq!(read[1][0].as_deref(), Some("2"));
        assert!(read[1][1..].iter().all(Option::is_none), "{:?}", read[1]);
        source.close().await;
        target.close().await;
    });
}

const PG_BIG: &str = "kerno_test_transfer_big";

fn big_source() -> Scratch {
    Scratch::new(
        Engine::Postgres,
        &[&format!("DROP SCHEMA IF EXISTS {PG_BIG} CASCADE")],
        &[
            &format!("CREATE SCHEMA {PG_BIG}"),
            &format!(
                "CREATE TABLE {PG_BIG}.lines (id integer PRIMARY KEY, label text, \
                 amount numeric(10,2), at timestamp)"
            ),
            &format!(
                "INSERT INTO {PG_BIG}.lines SELECT n, 'line ' || n, n / 100.0, \
                 timestamp '2024-01-01' + n * interval '1 minute' \
                 FROM generate_series(1, 50000) AS n"
            ),
        ],
    )
}

/// 50,000 rows in batches of 1,000, to every engine; the rates are
/// printed (`--nocapture`).
#[test]
fn fifty_thousand_rows_are_copied_in_batches_to_every_engine() {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _source = big_source();
    let table = TableRef::new(PG_BIG, "lines");
    let targets = [
        (Engine::Postgres, "kerno_test_transfer_bigpg"),
        (Engine::MySql, "kerno_test_transfer_bigmy"),
        (Engine::MariaDb, "kerno_test_transfer_bigmaria"),
    ];
    for (engine, schema) in targets {
        if !testing::available(engine) {
            continue;
        }
        let _target = match engine {
            Engine::Postgres => Scratch::new(
                engine,
                &[&format!("DROP SCHEMA IF EXISTS {schema} CASCADE")],
                &[&format!("CREATE SCHEMA {schema}")],
            ),
            _ => Scratch::new(
                engine,
                &[&format!("DROP DATABASE IF EXISTS {schema}")],
                &[&format!("CREATE DATABASE {schema}")],
            ),
        };
        let table = table.clone();
        block_on(async move {
            let (source, target) = (session(Engine::Postgres).await, session(engine).await);
            let plan = plan_for(&source, &target, &[table], schema).await;
            let batches = Arc::new(std::sync::atomic::AtomicU64::new(0));
            let counted = batches.clone();
            let report = copy(&source, &target, plan, Arc::default(), move |event| {
                if matches!(event, TransferEvent::Rows { .. }) {
                    counted.fetch_add(1, Ordering::Relaxed);
                }
            })
            .await;
            assert!(report.succeeded(), "{engine:?}: {}", report.text());
            assert_eq!(report.tables[0].written, 50_000);
            assert_eq!(batches.load(Ordering::Relaxed), 50);
            let check = rows(
                &target,
                &format!("SELECT COUNT(*), SUM(id) FROM {schema}.lines"),
            )
            .await;
            assert_eq!(check[0][0].as_deref(), Some("50000"));
            assert_eq!(check[0][1].as_deref(), Some("1250025000"));
            eprintln!(
                "PostgreSQL → {}: 50,000 rows in {:.2} s, {:.0} rows/s",
                engine.label(),
                report.tables[0].elapsed.as_secs_f64(),
                rate(&report)
            );
            source.close().await;
            target.close().await;
        });
    }

    // MySQL to PostgreSQL, 50,000 rows made by a cross join of digits.
    if !testing::available(Engine::MySql) {
        return;
    }
    let digits = "(SELECT 0 n UNION ALL SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3 \
                  UNION ALL SELECT 4 UNION ALL SELECT 5 UNION ALL SELECT 6 UNION ALL SELECT 7 \
                  UNION ALL SELECT 8 UNION ALL SELECT 9)";
    let _source = Scratch::new(
        Engine::MySql,
        &["DROP DATABASE IF EXISTS kerno_test_transfer_bigsrc"],
        &[
            "CREATE DATABASE kerno_test_transfer_bigsrc",
            "CREATE TABLE kerno_test_transfer_bigsrc.lines (id int PRIMARY KEY, \
             label varchar(40), amount decimal(10,2))",
            &format!(
                "INSERT INTO kerno_test_transfer_bigsrc.lines SELECT id, CONCAT('line ', id), \
                 id / 100 FROM (SELECT 1 + a.n + b.n * 10 + c.n * 100 + d.n * 1000 + \
                 e.n * 10000 AS id FROM {digits} a, {digits} b, {digits} c, {digits} d, \
                 {digits} e WHERE e.n < 5) ids"
            ),
        ],
    );
    let _target = Scratch::new(
        Engine::Postgres,
        &["DROP SCHEMA IF EXISTS kerno_test_transfer_bigfrommy CASCADE"],
        &["CREATE SCHEMA kerno_test_transfer_bigfrommy"],
    );
    block_on(async {
        let (source, target) = (
            session(Engine::MySql).await,
            session(Engine::Postgres).await,
        );
        let table = TableRef::new("kerno_test_transfer_bigsrc", "lines");
        let plan = plan_for(&source, &target, &[table], "kerno_test_transfer_bigfrommy").await;
        let report = copy(&source, &target, plan, Arc::default(), |_| {}).await;
        assert!(report.succeeded(), "{}", report.text());
        let check = rows(
            &target,
            "SELECT COUNT(*), SUM(id) FROM kerno_test_transfer_bigfrommy.lines",
        )
        .await;
        assert_eq!(check[0][0].as_deref(), Some("50000"));
        assert_eq!(check[0][1].as_deref(), Some("1250025000"));
        eprintln!(
            "MySQL → PostgreSQL: 50,000 rows in {:.2} s, {:.0} rows/s",
            report.tables[0].elapsed.as_secs_f64(),
            rate(&report)
        );
        source.close().await;
        target.close().await;
    });
}

/// A cancel stops between two batches: what was written stays, nothing
/// more is, and the target session is left out of any transaction.
#[test]
fn a_cancel_stops_the_copy_between_two_batches() {
    if !testing::available(Engine::Postgres) || !testing::available(Engine::MySql) {
        return;
    }
    let _source = Scratch::new(
        Engine::Postgres,
        &["DROP SCHEMA IF EXISTS kerno_test_transfer_cancel CASCADE"],
        &[
            "CREATE SCHEMA kerno_test_transfer_cancel",
            "CREATE TABLE kerno_test_transfer_cancel.lines AS \
             SELECT n AS id, 'line ' || n AS label FROM generate_series(1, 50000) AS n",
        ],
    );
    let _target = Scratch::new(
        Engine::MySql,
        &["DROP DATABASE IF EXISTS kerno_test_transfer_cancelmy"],
        &["CREATE DATABASE kerno_test_transfer_cancelmy"],
    );
    block_on(async {
        let (source, target) = (
            session(Engine::Postgres).await,
            session(Engine::MySql).await,
        );
        let table = TableRef::new("kerno_test_transfer_cancel", "lines");
        let plan = plan_for(&source, &target, &[table], "kerno_test_transfer_cancelmy").await;
        let cancel = Arc::new(AtomicBool::new(false));
        let asked = cancel.clone();
        // Cancelled once the first batch is written.
        let report = copy(&source, &target, plan, cancel, move |event| {
            if matches!(event, TransferEvent::Rows { .. }) {
                asked.store(true, Ordering::Release);
            }
        })
        .await;
        let table = &report.tables[0];
        assert_eq!(table.status, TableStatus::Cancelled, "{}", report.text());
        assert_eq!(table.written, 1_000);
        let count = rows(
            &target,
            "SELECT COUNT(*) FROM kerno_test_transfer_cancelmy.lines",
        )
        .await;
        assert_eq!(count[0][0].as_deref(), Some("1000"));
        assert_eq!(target.tx_state(), kerno_core::TxState::Idle);
        source.close().await;
        target.close().await;
    });
}

/// Append adds rows to a table that exists; a batch the target refuses is
/// rolled back whole, the batches before it stay, and the report gives the
/// error and the batch's first row.
#[test]
fn append_adds_rows_and_a_failing_batch_is_rolled_back() {
    if !testing::available(Engine::Postgres) || !testing::available(Engine::MySql) {
        return;
    }
    let _source = Scratch::new(
        Engine::Postgres,
        &["DROP SCHEMA IF EXISTS kerno_test_transfer_append CASCADE"],
        &[
            "CREATE SCHEMA kerno_test_transfer_append",
            "CREATE TABLE kerno_test_transfer_append.lines (id integer PRIMARY KEY, label text)",
            "INSERT INTO kerno_test_transfer_append.lines \
             SELECT n, 'line ' || n FROM generate_series(1, 5000) AS n",
        ],
    );
    let _target = Scratch::new(
        Engine::MySql,
        &["DROP DATABASE IF EXISTS kerno_test_transfer_appendmy"],
        &[
            "CREATE DATABASE kerno_test_transfer_appendmy",
            "CREATE TABLE kerno_test_transfer_appendmy.lines (id int PRIMARY KEY, \
             label varchar(40), note varchar(10) DEFAULT 'kept')",
            "CREATE TABLE kerno_test_transfer_appendmy.clash (id int PRIMARY KEY, \
             label varchar(40))",
            // Batch 3 (rows 2001 to 3000) holds an id that is there already.
            "INSERT INTO kerno_test_transfer_appendmy.clash VALUES (2500, 'there before')",
        ],
    );
    block_on(async {
        let (source, target) = (
            session(Engine::Postgres).await,
            session(Engine::MySql).await,
        );
        let table = TableRef::new("kerno_test_transfer_append", "lines");
        let mut plan = plan_for(
            &source,
            &target,
            std::slice::from_ref(&table),
            "kerno_test_transfer_appendmy",
        )
        .await;
        plan.mode = TransferMode::Append;
        let report = copy(&source, &target, plan.clone(), Arc::default(), |_| {}).await;
        assert!(report.succeeded(), "{}", report.text());
        let check = rows(
            &target,
            "SELECT COUNT(*), MIN(note) FROM kerno_test_transfer_appendmy.lines",
        )
        .await;
        assert_eq!(check[0][0].as_deref(), Some("5000"));
        // Columns the copy does not write keep their default.
        assert_eq!(check[0][1].as_deref(), Some("kept"));

        plan.tables[0].target_name = "clash".into();
        let report = copy(&source, &target, plan, Arc::default(), |_| {}).await;
        let outcome = &report.tables[0];
        match &outcome.status {
            TableStatus::Failed { error, first_row } => {
                assert!(error.contains("Duplicate entry"), "{error}");
                assert_eq!(first_row.as_deref(), Some("(2001, 'line 2001')"));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(outcome.written, 2_000);
        let check = rows(
            &target,
            "SELECT COUNT(*), MAX(id) FROM kerno_test_transfer_appendmy.clash",
        )
        .await;
        // Batches 1 and 2, and the row that was there: batch 3 left nothing.
        assert_eq!(check[0][0].as_deref(), Some("2001"));
        assert_eq!(check[0][1].as_deref(), Some("2500"));
        assert_eq!(target.tx_state(), kerno_core::TxState::Idle);

        // In Create mode a table that exists stops the copy, or is skipped.
        let mut create = plan_for(&source, &target, &[table], "kerno_test_transfer_appendmy").await;
        let report = copy(&source, &target, create.clone(), Arc::default(), |_| {}).await;
        assert!(matches!(
            report.tables[0].status,
            TableStatus::Failed { .. }
        ));
        create.if_exists = IfExists::Skip;
        let report = copy(&source, &target, create, Arc::default(), |_| {}).await;
        assert!(matches!(report.tables[0].status, TableStatus::Skipped(_)));
        assert_eq!(report.written(), 0);
        source.close().await;
        target.close().await;
    });
}

// The wizard -------------------------------------------------------------------

/// The app with two saved connections, the first connected: the source,
/// then the target.
fn two_connections(
    cx: &mut TestAppContext,
    source: ConnectionConfig,
    target: ConnectionConfig,
) -> Fixture {
    let storage = memory_storage();
    let ids = [
        storage.save_connection(None, &source).unwrap(),
        storage.save_connection(None, &target).unwrap(),
    ];
    let app = open(cx, storage);
    let secrets = cx.update(|cx| AppState::get(cx).secrets.clone());
    for id in &ids {
        kerno_core::secrets::save_secrets(secrets.as_ref(), id, &testing::secrets()).unwrap();
    }
    app.step(cx, |window, cx| {
        window.double_click(("connection", 0usize), cx)
    });
    app.wait_until(cx, "the source connection", |app, cx| {
        matches!(app.status(cx, 0), Status::Connected { .. })
    });
    app
}

impl Fixture {
    /// Opens the copy of `tables` of the first connection's database, or
    /// of every table of `schema` when there are none.
    fn transfer_dialog(
        &self,
        cx: &mut TestAppContext,
        schema: Option<&str>,
        tables: Vec<TableRef>,
    ) -> Entity<TransferDialog> {
        let target = Target {
            schema: schema.map(str::to_string),
            ..self.target(cx, None)
        };
        let connections = self.connections.clone();
        self.step(cx, move |window, cx| {
            transfer_dialog::open(connections, target, tables, window, cx)
        })
        .expect("the copy dialog")
    }

    fn transfer_state(&self, cx: &mut TestAppContext, dialog: &Entity<TransferDialog>) -> String {
        cx.update(|cx| dialog.read(cx).state(cx))
    }

    fn wait_transfer(
        &self,
        cx: &mut TestAppContext,
        dialog: &Entity<TransferDialog>,
        start: &str,
    ) -> String {
        self.wait_until(
            cx,
            &format!("the copy dialog to show {start:?}"),
            |app, cx| app.transfer_state(cx, dialog).starts_with(start),
        );
        self.transfer_state(cx, dialog)
    }

    /// Waits until the dialog's state is `state`.
    fn wait_transfer_exactly(
        &self,
        cx: &mut TestAppContext,
        dialog: &Entity<TransferDialog>,
        state: &str,
    ) {
        self.wait_until(
            cx,
            &format!("the copy dialog to be at {state:?}"),
            |app, cx| app.transfer_state(cx, dialog) == state,
        );
    }

    /// The SQL the confirmation shows.
    fn confirmation_sql(&self, cx: &mut TestAppContext) -> String {
        self.step(cx, |window, _| {
            gpui_kit::base::test_support::texts(window)
                .into_iter()
                .map(|text| text.text().to_string())
                // The confirmation's: the wizard's own preview has no INSERT.
                .find(|text| text.starts_with("CREATE TABLE") && text.contains("INSERT INTO"))
                .unwrap_or_default()
        })
    }
}

/// DBeaver #17767: the wizard end to end, PostgreSQL to MySQL; a column
/// without a MySQL counterpart blocks Start until it is skipped.
#[gpui_kit::test]
fn the_wizard_copies_tables_to_another_connection(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) || !testing::available(Engine::MySql) {
        return;
    }
    let _source = Scratch::new(
        Engine::Postgres,
        &["DROP SCHEMA IF EXISTS kerno_ui_transfer_src CASCADE"],
        &[
            "CREATE SCHEMA kerno_ui_transfer_src",
            "CREATE TABLE kerno_ui_transfer_src.t (id integer PRIMARY KEY, name text, tags text[])",
            "INSERT INTO kerno_ui_transfer_src.t VALUES (1, 'a', '{x}'), (2, NULL, NULL), \
             (3, 'c', '{}')",
        ],
    );
    let _target = Scratch::new(
        Engine::MySql,
        &["DROP DATABASE IF EXISTS kerno_ui_transfer_dst"],
        &["CREATE DATABASE kerno_ui_transfer_dst"],
    );
    let app = two_connections(
        cx,
        testing::config(Engine::Postgres),
        testing::config(Engine::MySql),
    );
    let dialog = app.transfer_dialog(cx, None, vec![TableRef::new("kerno_ui_transfer_src", "t")]);
    app.wait_transfer_exactly(cx, &dialog, "tables");
    app.click(cx, "transfer-next");
    assert_eq!(
        app.transfer_state(cx, &dialog),
        "target: Choose the connection to copy to."
    );
    app.click(cx, ("transfer-target", 1usize));
    app.wait_until(cx, "the target connection", |app, cx| {
        matches!(app.status(cx, 1), Status::Connected { .. })
    });
    app.replace_text(cx, "transfer-schema-field", "kerno_ui_transfer_dst");
    assert_eq!(app.transfer_state(cx, &dialog), "target");
    app.click(cx, "transfer-next");
    let blocked = app.wait_transfer(cx, &dialog, "mapping: t: `tags text[]`");
    assert_eq!(
        blocked,
        "mapping: t: `tags text[]` has no MySQL equivalent: skip the column or type a target type."
    );
    // Start does nothing while the column blocks it.
    app.click(cx, "transfer-start");
    assert!(!app.has(cx, "sql-confirm"));
    app.click(cx, ("transfer-skip", 2usize));
    assert_eq!(app.transfer_state(cx, &dialog), "mapping");
    app.click(cx, "transfer-start");
    let sql = app.confirmation_sql(cx);
    assert!(
        sql.starts_with("CREATE TABLE `kerno_ui_transfer_dst`.`t` (\n    `id` int NOT NULL,")
            && sql.contains("`name` longtext NULL")
            && !sql.contains("tags")
            && sql.contains("INSERT INTO `kerno_ui_transfer_dst`.`t` (`id`, `name`) VALUES (…)"),
        "{sql}"
    );
    // Nothing is written before the confirmation.
    assert_eq!(
        on_server(
            Engine::MySql,
            "SELECT COUNT(*) FROM information_schema.TABLES \
             WHERE TABLE_SCHEMA = 'kerno_ui_transfer_dst'"
        ),
        "0"
    );
    app.click(cx, "sql-confirm");
    let done = app.wait_transfer(cx, &dialog, "copied");
    assert!(done.contains("3 rows read, 3 written"), "{done}");
    let report = cx.update(|cx| dialog.read(cx).report()).expect("a report");
    assert_eq!(report.written(), 3);
    assert_eq!(report.tables[0].status, TableStatus::Copied);
    assert_eq!(
        on_server(
            Engine::MySql,
            "SELECT COUNT(*) FROM kerno_ui_transfer_dst.t"
        ),
        "3"
    );
    assert!(app.has(cx, "transfer-report"));
    app.click(cx, "transfer-copy-report");
    assert!(app.clipboard(cx).is_some_and(|text| {
        text.starts_with("kerno_ui_transfer_src.t → kerno_ui_transfer_dst.t: copied.")
    }));
}

/// A production target asks twice before anything is written; a read-only
/// one is refused before the mapping.
#[gpui_kit::test]
fn a_production_target_asks_twice_and_a_read_only_one_is_refused(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _source = Scratch::new(
        Engine::Postgres,
        &[
            "DROP SCHEMA IF EXISTS kerno_ui_transfer_prod_src CASCADE",
            "DROP SCHEMA IF EXISTS kerno_ui_transfer_prod_dst CASCADE",
        ],
        &[
            "CREATE SCHEMA kerno_ui_transfer_prod_src",
            "CREATE SCHEMA kerno_ui_transfer_prod_dst",
            "CREATE TABLE kerno_ui_transfer_prod_src.t (id serial PRIMARY KEY, name text)",
            "INSERT INTO kerno_ui_transfer_prod_src.t (name) VALUES ('a'), ('b')",
        ],
    );
    let mut production = testing::config(Engine::Postgres);
    production.name = "prod copy".into();
    production.environment = kerno_core::environment::PRODUCTION.into();
    let app = two_connections(cx, testing::config(Engine::Postgres), production);
    // Opened on a schema: its tables are ticked.
    let dialog = app.transfer_dialog(cx, Some("kerno_ui_transfer_prod_src"), Vec::new());
    app.wait_transfer_exactly(cx, &dialog, "tables");
    app.click(cx, "transfer-next");
    app.click(cx, ("transfer-target", 1usize));
    app.wait_until(cx, "the target connection", |app, cx| {
        matches!(app.status(cx, 1), Status::Connected { .. })
    });
    app.replace_text(cx, "transfer-schema-field", "kerno_ui_transfer_prod_dst");
    app.click(cx, "transfer-next");
    app.wait_transfer_exactly(cx, &dialog, "mapping");
    app.click(cx, "transfer-start");
    assert!(app.has(cx, "sql-production"));
    // The first click arms, the second copies.
    app.click(cx, "sql-confirm");
    assert!(app.has(cx, "sql-confirm"));
    assert_eq!(
        on_server(
            Engine::Postgres,
            "SELECT count(*) FROM pg_tables WHERE schemaname = 'kerno_ui_transfer_prod_dst'"
        ),
        "0"
    );
    app.click(cx, "sql-confirm");
    app.wait_transfer(cx, &dialog, "copied");
    assert_eq!(
        on_server(
            Engine::Postgres,
            "SELECT string_agg(name, ',' ORDER BY id) FROM kerno_ui_transfer_prod_dst.t"
        ),
        "a,b"
    );
    app.click(cx, "transfer-close");

    // A read-only target: refused in step 2, the mapping never read.
    cx.update(|cx| {
        app.connections.update(cx, |connections, cx| {
            let id = connections.saved()[1].id.clone();
            connections.set_read_only(&id, true, cx)
        })
    });
    let dialog = app.transfer_dialog(
        cx,
        None,
        vec![TableRef::new("kerno_ui_transfer_prod_src", "t")],
    );
    app.wait_transfer_exactly(cx, &dialog, "tables");
    app.click(cx, "transfer-next");
    app.click(cx, ("transfer-target", 1usize));
    assert_eq!(
        app.transfer_state(cx, &dialog),
        "target: “prod copy” is a read-only connection: nothing can be written to it."
    );
    app.click(cx, "transfer-next");
    assert!(app.transfer_state(cx, &dialog).starts_with("target:"));
}

/// The entry points: Tools ▸ on a table and on a schema of the tree, and
/// the command palette's command.
#[gpui_kit::test]
fn copy_tables_opens_from_tools_and_from_the_palette(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = Scratch::new(
        Engine::Postgres,
        &["DROP SCHEMA IF EXISTS kerno_ui_transfer_menu CASCADE"],
        &[
            "CREATE SCHEMA kerno_ui_transfer_menu",
            "CREATE TABLE kerno_ui_transfer_menu.a (n int)",
            "CREATE TABLE kerno_ui_transfer_menu.b (n int)",
        ],
    );
    let app = connected(cx, Engine::Postgres);
    let table = ["kerno", "kerno_ui_transfer_menu", "Tables", "a"];
    app.open_path(cx, &table);
    // A real right-click on the table, Tools ▸, the entry.
    let row = app.tree_row(cx, &table);
    app.right_click(cx, ("node", row), None);
    click_menu_entry(&app, cx, "Tools");
    click_menu_entry(&app, cx, "Copy tables to another connection…");
    assert!(app.has(cx, "transfer-dialog"));
    app.wait_until(cx, "the table ticked", |app, cx| {
        app.step(cx, |window, _| {
            window.try_find("transfer-blocked").is_none()
        })
    });
    app.click(cx, "transfer-close");
    assert!(!app.has(cx, "transfer-dialog"));

    // On the schema: its tables ticked.
    let row = app.tree_row(cx, &["kerno", "kerno_ui_transfer_menu"]);
    let navigator = cx.update(|cx| app.workspace.read(cx).navigator(cx));
    app.step(cx, |window, cx| {
        navigator.update(cx, |navigator, cx| {
            let id = navigator.id_at(row, cx);
            navigator.perform_tool(ToolEntry::CopyTables, &id, window, cx)
        })
    });
    assert!(app.has(cx, "transfer-dialog"));
    app.click(cx, "transfer-close");

    // The palette's command, on the active connection.
    app.dispatch(cx, crate::actions::CopyTables);
    assert!(app.has(cx, "transfer-dialog"));
}

/// The plan and the `CREATE` preview of the mapping page are worked out
/// when an input changes (a column skipped, a field typed, the target), not
/// at every frame, as `TasksView::reload` since M10 (DBeaver #17767).
#[gpui_kit::test]
fn the_mapping_page_builds_its_plan_once_per_change(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _source = Scratch::new(
        Engine::Postgres,
        &[
            "DROP SCHEMA IF EXISTS kerno_ui_transfer_frames CASCADE",
            "DROP SCHEMA IF EXISTS kerno_ui_transfer_frames_dst CASCADE",
        ],
        &[
            "CREATE SCHEMA kerno_ui_transfer_frames",
            "CREATE SCHEMA kerno_ui_transfer_frames_dst",
            "CREATE TABLE kerno_ui_transfer_frames.t (id integer PRIMARY KEY, name text)",
        ],
    );
    let app = two_connections(
        cx,
        testing::config(Engine::Postgres),
        testing::config(Engine::Postgres),
    );
    let dialog = app.transfer_dialog(
        cx,
        None,
        vec![TableRef::new("kerno_ui_transfer_frames", "t")],
    );
    app.wait_transfer_exactly(cx, &dialog, "tables");
    app.click(cx, "transfer-next");
    app.click(cx, ("transfer-target", 1usize));
    app.wait_until(cx, "the target connection", |app, cx| {
        matches!(app.status(cx, 1), Status::Connected { .. })
    });
    app.replace_text(cx, "transfer-schema-field", "kerno_ui_transfer_frames_dst");
    app.click(cx, "transfer-next");
    app.wait_transfer_exactly(cx, &dialog, "mapping");
    assert!(super::tool_wizards::paints(&app, cx, "\"name\" text"));
    let builds = |cx: &mut TestAppContext| cx.update(|cx| dialog.read(cx).plan_builds());
    let before = builds(cx);
    for _ in 0..5 {
        app.step(cx, |window, _| window.refresh());
    }
    assert_eq!(builds(cx), before, "frames build nothing");
    // A column skipped: built again.
    app.click(cx, ("transfer-skip", 1usize));
    app.step(cx, |window, _| window.refresh());
    let after = builds(cx);
    assert!(after > before, "{before} → {after}");
    assert!(!super::tool_wizards::paints(&app, cx, "\"name\" text"));
    for _ in 0..5 {
        app.step(cx, |window, _| window.refresh());
    }
    assert_eq!(builds(cx), after, "frames build nothing");
}
