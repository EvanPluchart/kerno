//! The maintenance tools of the PostgreSQL dialect, run on scratch objects
//! (a schema, and a database for the database-wide tools) and checked by
//! their effect: the row estimate after ANALYZE and VACUUM, the new file of
//! VACUUM FULL, the rows of a refreshed materialized view, the identity
//! restarted by TRUNCATE, a trigger that stops firing. Runs against the
//! `kerno-postgres` container; skipped with a message when it is down.

mod common;

use std::future::Future;
use std::sync::Arc;

use common::{db_error, exec_ok, run, scalar};
use kerno_core::maintenance::{Maintenance, RefreshOptions, TruncateOptions, VacuumOptions};
use kerno_core::schema::{ObjectRef, RelationKind, TableRef};
use kerno_core::{Session, testing};

/// SQLSTATE of a statement refused inside a transaction block.
const IN_TRANSACTION: &str = "25001";
/// SQLSTATE of a TRUNCATE refused because a foreign key references the table.
const FEATURE_NOT_SUPPORTED: &str = "0A000";
/// SQLSTATE of reading a materialized view refreshed WITH NO DATA.
const NOT_POPULATED: &str = "55000";

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

/// The statement of `tool` on `target`.
fn sql(session: &Arc<dyn Session>, tool: Maintenance, target: &ObjectRef) -> String {
    session
        .dialect()
        .maintenance_sql(&tool, target)
        .unwrap_or_else(|error| panic!("{tool:?} on {target:?}: {error}"))
        .sql
}

/// Runs `sql`, which must report through notices (`VERBOSE`).
async fn verbose(session: &Arc<dyn Session>, sql: &str) {
    let collected = run(session, sql)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    assert!(!collected.notices.is_empty(), "{sql}: no notice");
}

#[tokio::test]
async fn tools_act_on_tables_views_and_triggers() {
    let Some(session) = common::session().await else {
        return;
    };
    let schema = common::scratch_schema("maintenance");
    let cleanup = [format!("DROP SCHEMA IF EXISTS {schema} CASCADE")];
    cleaning_up(&session, &cleanup, objects(session.clone(), schema)).await;
}

async fn objects(session: Arc<dyn Session>, schema: String) {
    let d = session.dialect();
    let items = TableRef::new(&schema, "order \"items\"");
    let name = d.table_name(&items);
    let table = ObjectRef::Relation(items.clone(), RelationKind::Table);
    let view_ref = TableRef::new(&schema, "stock view");
    let view_name = d.table_name(&view_ref);
    let view = ObjectRef::Relation(view_ref, RelationKind::MaterializedView);
    let trigger = ObjectRef::Trigger {
        table: items.clone(),
        name: "audit \"it\"".into(),
    };
    for setup in [
        format!("CREATE SCHEMA {schema}"),
        format!("CREATE TABLE {name} (id int GENERATED ALWAYS AS IDENTITY PRIMARY KEY, note text)"),
        format!("CREATE TABLE {schema}.child (item int REFERENCES {name})"),
        format!("INSERT INTO {name} (note) SELECT 'n' FROM generate_series(1, 50)"),
        format!("INSERT INTO {schema}.child VALUES (1)"),
        format!("CREATE MATERIALIZED VIEW {view_name} AS SELECT id FROM {name}"),
        format!("CREATE UNIQUE INDEX ON {view_name} (id)"),
        format!(
            "CREATE FUNCTION {schema}.mark() RETURNS trigger LANGUAGE plpgsql \
             AS $$ BEGIN NEW.note := 'fired'; RETURN NEW; END $$"
        ),
        format!(
            "CREATE TRIGGER \"audit \"\"it\"\"\" BEFORE INSERT ON {name} \
             FOR EACH ROW EXECUTE FUNCTION {schema}.mark()"
        ),
    ] {
        exec_ok(&session, &setup).await;
    }

    // ANALYZE fills the estimate of a table never analysed.
    let estimate = d.estimated_rows_sql(&items);
    assert_eq!(scalar(&session, &estimate).await, "-1");
    verbose(&session, &sql(&session, Maintenance::Analyze, &table)).await;
    assert_eq!(scalar(&session, &estimate).await, "50");

    // VACUUM with ANALYZE updates it again; every other option is accepted;
    // FULL writes the table to a new file.
    exec_ok(&session, &format!("DELETE FROM {name} WHERE id > 40")).await;
    let vacuum = |options| sql(&session, Maintenance::Vacuum(options), &table);
    verbose(
        &session,
        &vacuum(VacuumOptions {
            analyze: true,
            ..VacuumOptions::default()
        }),
    )
    .await;
    assert_eq!(scalar(&session, &estimate).await, "40");
    verbose(
        &session,
        &vacuum(VacuumOptions {
            full: false,
            freeze: true,
            analyze: true,
            disable_page_skipping: true,
            skip_locked: true,
            index_cleanup: true,
        }),
    )
    .await;
    let filenode = format!(
        "SELECT pg_relation_filenode({}::regclass)",
        d.quote_string(&name)
    );
    let before = scalar(&session, &filenode).await;
    verbose(
        &session,
        &vacuum(VacuumOptions {
            full: true,
            ..VacuumOptions::default()
        }),
    )
    .await;
    assert_ne!(scalar(&session, &filenode).await, before);

    // A refresh shows the rows added since; WITH NO DATA empties the view
    // until the next refresh; CONCURRENTLY needs data and the unique index.
    let view_count = format!("SELECT count(*) FROM {view_name}");
    assert_eq!(scalar(&session, &view_count).await, "50");
    let concurrently = RefreshOptions {
        concurrently: true,
        with_data: true,
    };
    let refresh = |options| {
        sql(
            &session,
            Maintenance::RefreshMaterializedView(options),
            &view,
        )
    };
    exec_ok(&session, &refresh(RefreshOptions::default())).await;
    assert_eq!(scalar(&session, &view_count).await, "40");
    exec_ok(
        &session,
        &refresh(RefreshOptions {
            concurrently: false,
            with_data: false,
        }),
    )
    .await;
    let unreadable = db_error(&session, &view_count).await;
    assert_eq!(unreadable.code.as_deref(), Some(NOT_POPULATED));
    exec_ok(&session, &refresh(RefreshOptions::default())).await;
    exec_ok(
        &session,
        &format!("INSERT INTO {name} (note) VALUES ('new')"),
    )
    .await;
    exec_ok(&session, &refresh(concurrently)).await;
    assert_eq!(scalar(&session, &view_count).await, "41");
    verbose(&session, &sql(&session, Maintenance::Analyze, &view)).await;
    verbose(
        &session,
        &sql(
            &session,
            Maintenance::Vacuum(VacuumOptions::default()),
            &view,
        ),
    )
    .await;

    // A disabled trigger does not fire; enabled again, it does.
    let state = format!(
        "SELECT tgenabled FROM pg_catalog.pg_trigger WHERE tgname = 'audit \"it\"' \
         AND tgrelid = {}::regclass",
        d.quote_string(&name)
    );
    let insert = |note: &str| format!("INSERT INTO {name} (note) VALUES ('{note}') RETURNING note");
    assert_eq!(scalar(&session, &insert("a")).await, "fired");
    exec_ok(
        &session,
        &sql(&session, Maintenance::DisableTrigger, &trigger),
    )
    .await;
    assert_eq!(scalar(&session, &state).await, "D");
    assert_eq!(scalar(&session, &insert("b")).await, "b");
    exec_ok(
        &session,
        &sql(&session, Maintenance::EnableTrigger, &trigger),
    )
    .await;
    assert_eq!(scalar(&session, &state).await, "O");
    assert_eq!(scalar(&session, &insert("c")).await, "fired");

    // A referenced table is truncated only with CASCADE, which empties the
    // referencing one too; RESTART IDENTITY starts the ids again at 1.
    let truncate = |options| sql(&session, Maintenance::Truncate(options), &table);
    let refused = db_error(&session, &truncate(TruncateOptions::default())).await;
    assert_eq!(refused.code.as_deref(), Some(FEATURE_NOT_SUPPORTED));
    exec_ok(
        &session,
        &truncate(TruncateOptions {
            only: true,
            restart_identity: true,
            cascade: true,
        }),
    )
    .await;
    assert_eq!(
        scalar(&session, &format!("SELECT count(*) FROM {schema}.child")).await,
        "0"
    );
    assert_eq!(
        scalar(
            &session,
            &format!("INSERT INTO {name} (note) VALUES ('x') RETURNING id")
        )
        .await,
        "1"
    );

    // A partitioned table takes its tools at their defaults.
    let parted = TableRef::new(&schema, "parted");
    let parted_name = d.table_name(&parted);
    exec_ok(
        &session,
        &format!("CREATE TABLE {parted_name} (n int) PARTITION BY RANGE (n)"),
    )
    .await;
    exec_ok(
        &session,
        &format!(
            "CREATE TABLE {schema}.part PARTITION OF {parted_name} FOR VALUES FROM (0) TO (10)"
        ),
    )
    .await;
    exec_ok(
        &session,
        &format!("INSERT INTO {parted_name} VALUES (1), (2)"),
    )
    .await;
    let parted = ObjectRef::Relation(parted, RelationKind::PartitionedTable);
    for tool in d.maintenance_tools(&parted) {
        exec_ok(&session, &sql(&session, tool, &parted)).await;
    }
    assert_eq!(
        scalar(&session, &format!("SELECT count(*) FROM {parted_name}")).await,
        "0"
    );

    // Only VACUUM is refused in a transaction block: what `autocommit` says.
    let cascade = TruncateOptions {
        cascade: true,
        ..TruncateOptions::default()
    };
    for (tool, target) in [
        (Maintenance::Vacuum(VacuumOptions::default()), &table),
        (Maintenance::Analyze, &table),
        (Maintenance::Truncate(cascade), &table),
        (
            Maintenance::RefreshMaterializedView(RefreshOptions::default()),
            &view,
        ),
        (Maintenance::RefreshMaterializedView(concurrently), &view),
        (Maintenance::DisableTrigger, &trigger),
    ] {
        let out = d.maintenance_sql(&tool, target).unwrap();
        exec_ok(&session, "BEGIN").await;
        let refused = match run(&session, &out.sql).await {
            Err(kerno_core::Error::Db(error)) if error.code.as_deref() == Some(IN_TRANSACTION) => {
                true
            }
            Err(error) => panic!("{}: {error}", out.sql),
            Ok(_) => false,
        };
        exec_ok(&session, "ROLLBACK").await;
        assert_eq!(refused, out.autocommit, "{}", out.sql);
    }
}

#[tokio::test]
async fn vacuum_and_analyze_run_on_the_connected_database() {
    let Some(session) = common::session().await else {
        return;
    };
    let database = common::scratch_schema("maintenance_db");
    let cleanup = [format!("DROP DATABASE IF EXISTS {database} WITH (FORCE)")];
    cleaning_up(&session, &cleanup, whole(session.clone(), database)).await;
}

async fn whole(session: Arc<dyn Session>, database: String) {
    exec_ok(&session, &format!("CREATE DATABASE {database}")).await;
    let mut params = testing::params(common::ENGINE);
    params.database = Some(database.clone());
    let scratch = common::connect(&params).await.expect("connect");
    exec_ok(
        &scratch,
        "CREATE TABLE t AS SELECT generate_series(1, 30) AS n",
    )
    .await;
    let target = ObjectRef::Database(database);
    let tools = scratch.dialect().maintenance_tools(&target);
    assert_eq!(
        tools,
        [
            Maintenance::Vacuum(VacuumOptions::default()),
            Maintenance::Analyze
        ]
    );
    for tool in tools {
        verbose(&scratch, &sql(&scratch, tool, &target)).await;
    }
    let estimate = scratch
        .dialect()
        .estimated_rows_sql(&TableRef::new("public", "t"));
    assert_eq!(scalar(&scratch, &estimate).await, "30");
}
