//! The maintenance tools of the MySQL / MariaDB dialect, run on a scratch
//! database: every tool with its options answers with its status rows
//! (`Table`, `Op`, `Msg_type`, `Msg_text`), commits the open transaction,
//! and TRUNCATE empties the table. Runs against the `kerno-mysql` and
//! `kerno-mariadb` containers; skipped with a message when they are down.

mod common;

use std::future::Future;
use std::sync::Arc;

use common::{engines, exec_ok, run, scalar};
use kerno_core::Session;
use kerno_core::maintenance::{CheckOption, Maintenance, RepairOptions, TruncateOptions};
use kerno_core::schema::{ObjectRef, RelationKind, TableRef};

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

#[tokio::test]
async fn table_tools_answer_with_their_status_rows() {
    for engine in engines() {
        let session = common::session(engine).await;
        let database = common::scratch_database("maintenance");
        let cleanup = [format!("DROP DATABASE IF EXISTS {database}")];
        cleaning_up(&session, &cleanup, tools(session.clone(), database)).await;
    }
}

/// The `(Op, Msg_type, Msg_text)` rows `sql` answers with, after checking
/// the columns.
async fn status(session: &Arc<dyn Session>, sql: &str) -> Vec<(String, String, String)> {
    let collected = run(session, sql)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    let result = collected.results.first().expect("a result");
    let columns: Vec<&str> = result.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(columns, ["Table", "Op", "Msg_type", "Msg_text"], "{sql}");
    result
        .rows()
        .into_iter()
        .map(|row| {
            let cell = |i: usize| row[i].clone().unwrap_or_default();
            (cell(1), cell(2), cell(3))
        })
        .collect()
}

/// Whether the last row of `rows` is the final `status OK`.
fn ok(rows: &[(String, String, String)]) -> bool {
    rows.last()
        .is_some_and(|(_, kind, text)| kind == "status" && text == "OK")
}

async fn tools(session: Arc<dyn Session>, database: String) {
    let d = session.dialect();
    let inno = TableRef::new(&database, "order `items");
    let my = TableRef::new(&database, "plain");
    let (inno_name, my_name) = (d.table_name(&inno), d.table_name(&my));
    let table = ObjectRef::Relation(inno, RelationKind::Table);
    let myisam = ObjectRef::Relation(my, RelationKind::Table);
    for setup in [
        format!("CREATE DATABASE {database}"),
        format!("CREATE TABLE {inno_name} (id INT PRIMARY KEY) ENGINE = InnoDB"),
        format!("CREATE TABLE {my_name} (id INT PRIMARY KEY) ENGINE = MyISAM"),
        format!("INSERT INTO {inno_name} VALUES (1), (2), (3)"),
        format!("INSERT INTO {my_name} VALUES (1), (2)"),
    ] {
        exec_ok(&session, &setup).await;
    }
    let sql = |tool: &Maintenance, target: &ObjectRef| {
        let out = d.maintenance_sql(tool, target).unwrap();
        assert!(out.autocommit);
        assert_eq!(out.returns_rows, !matches!(tool, Maintenance::Truncate(_)));
        out.sql
    };

    let mut tools = vec![
        Maintenance::Analyze,
        Maintenance::Optimize,
        Maintenance::Check(None),
    ];
    tools.extend(CheckOption::ALL.map(|option| Maintenance::Check(Some(option))));
    for tool in &tools {
        let rows = status(&session, &sql(tool, &table)).await;
        assert!(ok(&rows), "{tool:?}: {rows:?}");
    }
    // InnoDB has no repair: the server says so in a note, or (MariaDB,
    // here after the OPTIMIZE above) answers OK. MyISAM repairs, with
    // every option.
    let repair = Maintenance::Repair(RepairOptions::default());
    let rows = status(&session, &sql(&repair, &table)).await;
    assert!(
        rows.iter().all(|(op, _, _)| op == "repair") && !rows.is_empty(),
        "{rows:?}"
    );
    for options in [
        RepairOptions::default(),
        RepairOptions {
            quick: true,
            extended: true,
            use_frm: true,
        },
    ] {
        let rows = status(&session, &sql(&Maintenance::Repair(options), &myisam)).await;
        assert!(ok(&rows), "{options:?}: {rows:?}");
    }

    // Each tool commits the open transaction: the row stays after ROLLBACK.
    // (On the MyISAM table, so TRUNCATE empties the other one at the end.)
    let count = format!("SELECT COUNT(*) FROM {inno_name}");
    let committed = [
        Maintenance::Analyze,
        Maintenance::Check(None),
        Maintenance::Optimize,
        repair,
        Maintenance::Truncate(TruncateOptions::default()),
    ];
    for (id, tool) in (4..).zip(committed) {
        exec_ok(&session, "START TRANSACTION").await;
        exec_ok(&session, &format!("INSERT INTO {inno_name} VALUES ({id})")).await;
        run(&session, &sql(&tool, &myisam))
            .await
            .unwrap_or_else(|error| panic!("{tool:?}: {error}"));
        exec_ok(&session, "ROLLBACK").await;
        assert_eq!(scalar(&session, &count).await, id.to_string(), "{tool:?}");
    }

    exec_ok(
        &session,
        &sql(&Maintenance::Truncate(TruncateOptions::default()), &table),
    )
    .await;
    assert_eq!(scalar(&session, &count).await, "0");
}
