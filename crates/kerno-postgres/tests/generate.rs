//! The statements the navigator generates, run on PostgreSQL: the four
//! table templates with their parameters bound, and rename, truncate, drop
//! and create for every kind of object, checked in the catalogs. Runs
//! against the `kerno-postgres` container; skipped with a message when it
//! is down.

mod common;

use std::future::Future;
use std::sync::Arc;

use common::{db_error, exec_ok, rows, run, scalar};
use kerno_core::Session;
use kerno_core::schema::{ObjectRef, RelationKind, RoutineKind, TableRef};
use kerno_core::sql::{
    ParamKey, bind_params, delete_template, find_params, insert_template, select_template,
    update_template,
};

/// SQLSTATE of a syntax error.
const SYNTAX_ERROR: &str = "42601";
/// SQLSTATE of a `DROP` refused because other objects depend on the target.
const DEPENDENT_OBJECTS: &str = "2BP01";

/// Runs the `cleanup` statements, then `body`, then `cleanup` again whether
/// `body` passed or panicked: a failed run leaves nothing on the server,
/// and neither did a killed one once the test has run again.
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

/// `sql` with each `:name` parameter replaced by the SQL given for it.
fn bind(session: &Arc<dyn Session>, sql: &str, values: &[(&str, &str)]) -> String {
    let params = find_params(sql, session.dialect().lex_rules());
    bind_params(sql, &params, |key| match key {
        ParamKey::Named(name) => Some(
            values
                .iter()
                .find(|(param, _)| param == name)
                .unwrap_or_else(|| panic!("no value for :{name} in\n{sql}"))
                .1
                .to_string(),
        ),
        other => panic!("unexpected parameter {other:?} in\n{sql}"),
    })
}

fn cells(row: &[&str]) -> Vec<Option<String>> {
    row.iter()
        .map(|cell| (*cell != "NULL").then(|| cell.to_string()))
        .collect()
}

#[tokio::test]
async fn templates_are_valid_sql_once_their_parameters_are_bound() {
    let Some(session) = common::session().await else {
        return;
    };
    let schema = common::scratch_schema("generate_templates");
    let cleanup = [format!("DROP SCHEMA IF EXISTS {schema} CASCADE")];
    cleaning_up(&session, &cleanup, templates(session.clone(), schema)).await;
}

async fn templates(session: Arc<dyn Session>, schema: String) {
    let d = session.dialect();
    exec_ok(&session, &format!("CREATE SCHEMA {schema}")).await;
    exec_ok(
        &session,
        &format!(
            "CREATE TABLE {schema}.\"order items\" (\
               id integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY, \
               \"order\" integer NOT NULL, \
               \"full name\" text, \
               \"Weird \"\"name\"\"\" text, \
               qty integer NOT NULL DEFAULT 1, \
               doubled integer GENERATED ALWAYS AS (qty * 2) STORED)"
        ),
    )
    .await;
    let table = TableRef::new(schema.clone(), "order items");
    let structure = session.table_structure(&table).await.unwrap();
    assert_eq!(structure.columns.len(), 6);

    // INSERT: the identity and the generated column are left to the server.
    let insert = insert_template(d, &table, &structure.columns);
    let values = [
        ("order", "7"),
        ("full_name", "'Ada'"),
        ("Weird__name_", "'x'"),
        ("qty", "3"),
    ];
    exec_ok(&session, &bind(&session, &insert, &values)).await;

    // SELECT: every column, more than fit on a line.
    let select = select_template(d, &table, &structure.columns, 10);
    assert_eq!(select.lines().count(), 9, "{select}");
    assert_eq!(
        rows(&session, &select).await,
        [cells(&["1", "7", "Ada", "x", "3", "6"])]
    );
    let star = select_template(d, &table, &[], 10);
    assert_eq!(rows(&session, &star).await.len(), 1);

    // UPDATE: the row is addressed by its key.
    let update = update_template(d, &table, &structure);
    let values = [
        ("order", "8"),
        ("full_name", "NULL"),
        ("Weird__name_", "'y'"),
        ("qty", "5"),
        ("id", "1"),
    ];
    exec_ok(&session, &bind(&session, &update, &values)).await;
    assert_eq!(
        rows(&session, &select).await,
        [cells(&["1", "8", "NULL", "y", "5", "10"])]
    );

    // DELETE: another key leaves the row alone, its own removes it.
    let delete = delete_template(d, &table, &structure);
    let count = format!("SELECT count(*) FROM {}", d.table_name(&table));
    exec_ok(&session, &bind(&session, &delete, &[("id", "2")])).await;
    assert_eq!(scalar(&session, &count).await, "1");
    exec_ok(&session, &bind(&session, &delete, &[("id", "1")])).await;
    assert_eq!(scalar(&session, &count).await, "0");

    // A table whose columns are all in its key: the key itself is changed.
    exec_ok(
        &session,
        &format!(
            "CREATE TABLE {schema}.tags (order_id integer, tag text, PRIMARY KEY (order_id, tag))"
        ),
    )
    .await;
    exec_ok(
        &session,
        &format!("INSERT INTO {schema}.tags VALUES (1, 'a')"),
    )
    .await;
    let tags = TableRef::new(schema.clone(), "tags");
    let tags_structure = session.table_structure(&tags).await.unwrap();
    let update = update_template(d, &tags, &tags_structure);
    let values = [
        ("order_id", "1"),
        ("tag", "'a'"),
        ("new_order_id", "2"),
        ("new_tag", "'b'"),
    ];
    exec_ok(&session, &bind(&session, &update, &values)).await;
    assert_eq!(
        rows(&session, &format!("SELECT * FROM {schema}.tags")).await,
        [cells(&["2", "b"])]
    );

    // Without a key the templates stop at WHERE: the server refuses them
    // and every row is still there.
    exec_ok(
        &session,
        &format!("CREATE TABLE {schema}.no_key (a integer, \"b c\" text)"),
    )
    .await;
    exec_ok(
        &session,
        &format!("INSERT INTO {schema}.no_key VALUES (1, 'x'), (2, 'y')"),
    )
    .await;
    let no_key = TableRef::new(schema.clone(), "no_key");
    let no_key_structure = session.table_structure(&no_key).await.unwrap();
    assert_eq!(no_key_structure.row_identity(), None);
    let count = format!("SELECT count(*) FROM {}", d.table_name(&no_key));
    let update = update_template(d, &no_key, &no_key_structure);
    let update = bind(&session, &update, &[("a", "9"), ("b_c", "'z'")]);
    assert_eq!(
        db_error(&session, &update).await.code.as_deref(),
        Some(SYNTAX_ERROR)
    );
    let delete = delete_template(d, &no_key, &no_key_structure);
    assert_eq!(
        db_error(&session, &delete).await.code.as_deref(),
        Some(SYNTAX_ERROR)
    );
    assert_eq!(
        rows(
            &session,
            &format!("SELECT * FROM {schema}.no_key ORDER BY a")
        )
        .await,
        [cells(&["1", "x"]), cells(&["2", "y"])]
    );
    // Its INSERT works like any other.
    let insert = insert_template(d, &no_key, &no_key_structure.columns);
    exec_ok(
        &session,
        &bind(&session, &insert, &[("a", "3"), ("b_c", "'z'")]),
    )
    .await;
    assert_eq!(scalar(&session, &count).await, "3");

    // TRUNCATE empties the table.
    exec_ok(&session, &d.truncate_sql(&no_key)).await;
    assert_eq!(scalar(&session, &count).await, "0");
}

/// Renames `object(old)`, checks it is found under its new name only,
/// drops it and checks it is gone. `count(name)` is a query that counts
/// the objects of that kind called `name`.
async fn rename_then_drop(
    session: &Arc<dyn Session>,
    object: impl Fn(&str) -> ObjectRef,
    old: &str,
    count: impl Fn(&str) -> String,
) {
    let d = session.dialect();
    // A name that must be quoted.
    let new = format!("{old} \"renamed\"");
    assert_eq!(scalar(session, &count(old)).await, "1", "{old} exists");
    let rename = d.rename_sql(&object(old), &new).unwrap();
    exec_ok(session, &rename).await;
    assert_eq!(scalar(session, &count(old)).await, "0", "{rename}");
    assert_eq!(scalar(session, &count(&new)).await, "1", "{rename}");
    let drop = d.drop_sql(&object(&new)).unwrap();
    exec_ok(session, &drop).await;
    assert_eq!(scalar(session, &count(&new)).await, "0", "{drop}");
}

#[tokio::test]
async fn rename_truncate_drop_and_create_do_what_they_say() {
    let Some(session) = common::session().await else {
        return;
    };
    let d = session.dialect();
    let schema = common::scratch_schema("generate_objects");
    let mut cleanup = vec![
        format!("DROP SCHEMA IF EXISTS {schema} CASCADE"),
        format!("DROP FOREIGN DATA WRAPPER IF EXISTS {schema}_fdw CASCADE"),
    ];
    for name in [format!("{schema} made"), format!("{schema}_db")] {
        for name in [name.clone(), format!("{name} \"renamed\"")] {
            let name = d.quote_ident(&name);
            cleanup.push(format!("DROP SCHEMA IF EXISTS {name} CASCADE"));
            cleanup.push(format!("DROP DATABASE IF EXISTS {name}"));
        }
    }
    cleaning_up(&session, &cleanup, objects(session.clone(), schema)).await;
}

async fn objects(session: Arc<dyn Session>, schema: String) {
    let d = session.dialect();
    let lit = |text: &str| d.quote_string(text);
    for sql in [
        "CREATE SCHEMA {s}",
        "CREATE TABLE {s}.t (id integer PRIMARY KEY, v text)",
        "CREATE INDEX t_v_idx ON {s}.t (v)",
        "CREATE TABLE {s}.parted (id integer) PARTITION BY RANGE (id)",
        "CREATE VIEW {s}.v AS SELECT id FROM {s}.t",
        "CREATE MATERIALIZED VIEW {s}.mv AS SELECT id FROM {s}.t",
        "CREATE FOREIGN DATA WRAPPER {s}_fdw",
        "CREATE SERVER {s}_srv FOREIGN DATA WRAPPER {s}_fdw",
        "CREATE FOREIGN TABLE {s}.ft (id integer) SERVER {s}_srv",
        "CREATE FUNCTION {s}.f(integer) RETURNS integer LANGUAGE sql AS 'SELECT $1'",
        "CREATE FUNCTION {s}.f(note text) RETURNS text LANGUAGE sql AS 'SELECT $1'",
        "CREATE FUNCTION {s}.f() RETURNS integer LANGUAGE sql AS 'SELECT 1'",
        "CREATE PROCEDURE {s}.p(IN n integer, INOUT total integer) LANGUAGE sql \
         AS 'SELECT n + total'",
        "CREATE AGGREGATE {s}.total(integer) (SFUNC = int4pl, STYPE = integer)",
        "CREATE FUNCTION {s}.cnt_step(bigint) RETURNS bigint LANGUAGE sql AS 'SELECT $1 + 1'",
        "CREATE AGGREGATE {s}.cnt(*) (SFUNC = {s}.cnt_step, STYPE = bigint, INITCOND = '0')",
        "CREATE SEQUENCE {s}.seq",
        "CREATE TYPE {s}.mood AS ENUM ('sad', 'happy')",
        "CREATE DOMAIN {s}.positive AS integer CHECK (VALUE > 0)",
        "CREATE FUNCTION {s}.trg_fn() RETURNS trigger LANGUAGE plpgsql \
         AS 'BEGIN RETURN NEW; END'",
        "CREATE TRIGGER trg BEFORE INSERT ON {s}.t FOR EACH ROW EXECUTE FUNCTION {s}.trg_fn()",
    ] {
        exec_ok(&session, &sql.replace("{s}", &schema)).await;
    }
    let in_schema = |name: &str| TableRef::new(schema.clone(), name);
    let t = in_schema("t");

    // Anything in pg_class: tables, views, sequences, indexes.
    let class = |relkind: char| {
        let schema = lit(&schema);
        move |name: &str| {
            format!(
                "SELECT count(*) FROM pg_catalog.pg_class c \
                 JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = {schema} AND c.relkind = '{relkind}' AND c.relname = {}",
                lit(name)
            )
        }
    };

    // What others depend on is not dropped: there is no CASCADE.
    let drop_table = d
        .drop_sql(&ObjectRef::Relation(t.clone(), RelationKind::Table))
        .unwrap();
    assert_eq!(
        db_error(&session, &drop_table).await.code.as_deref(),
        Some(DEPENDENT_OBJECTS)
    );

    // Trigger: named within its table.
    rename_then_drop(
        &session,
        |name| ObjectRef::Trigger {
            table: t.clone(),
            name: name.into(),
        },
        "trg",
        |name| {
            format!(
                "SELECT count(*) FROM pg_catalog.pg_trigger \
                 WHERE tgrelid = '{schema}.t'::regclass AND tgname = {}",
                lit(name)
            )
        },
    )
    .await;

    // Index: named within the schema.
    rename_then_drop(
        &session,
        |name| ObjectRef::Index {
            table: t.clone(),
            name: name.into(),
        },
        "t_v_idx",
        class('i'),
    )
    .await;

    for (name, kind, relkind) in [
        ("v", RelationKind::View, 'v'),
        ("mv", RelationKind::MaterializedView, 'm'),
        ("ft", RelationKind::ForeignTable, 'f'),
        ("parted", RelationKind::PartitionedTable, 'p'),
    ] {
        rename_then_drop(
            &session,
            |name| ObjectRef::Relation(in_schema(name), kind),
            name,
            class(relkind),
        )
        .await;
    }

    rename_then_drop(
        &session,
        |name| ObjectRef::Sequence(in_schema(name)),
        "seq",
        class('S'),
    )
    .await;

    // Types: an enum, and a domain, which ALTER TYPE and DROP TYPE accept.
    for name in ["mood", "positive"] {
        rename_then_drop(
            &session,
            |name| ObjectRef::Type(in_schema(name)),
            name,
            |name| {
                format!(
                    "SELECT count(*) FROM pg_catalog.pg_type t \
                     JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace \
                     WHERE n.nspname = {} AND t.typname = {}",
                    lit(&schema),
                    lit(name)
                )
            },
        )
        .await;
    }

    // Routines, addressed as introspection lists them: overloads are told
    // apart by their arguments. Aggregates go first, one is built on a
    // function of the schema.
    let mut routines = session.objects(&schema).await.unwrap().routines;
    routines.sort_by_key(|routine| routine.kind != RoutineKind::Aggregate);
    let listed: Vec<_> = routines
        .iter()
        .map(|r| (r.name.as_str(), r.kind, r.identity_arguments.as_str()))
        .collect();
    for expected in [
        ("total", RoutineKind::Aggregate, "integer"),
        ("cnt", RoutineKind::Aggregate, ""),
        ("f", RoutineKind::Function, "integer"),
        ("f", RoutineKind::Function, "note text"),
        ("f", RoutineKind::Function, ""),
        (
            "p",
            RoutineKind::Procedure,
            "IN n integer, INOUT total integer",
        ),
    ] {
        assert!(listed.contains(&expected), "{expected:?} in {listed:?}");
    }
    assert_eq!(routines.len(), 8, "{listed:?}");
    for routine in &routines {
        rename_then_drop(
            &session,
            |name| ObjectRef::Routine {
                schema: schema.clone(),
                name: name.into(),
                kind: routine.kind,
                identity_arguments: routine.identity_arguments.clone(),
            },
            &routine.name,
            |name| {
                format!(
                    "SELECT count(*) FROM pg_catalog.pg_proc p \
                     JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace \
                     WHERE n.nspname = {} AND p.proname = {} \
                       AND pg_catalog.pg_get_function_identity_arguments(p.oid) = {}",
                    lit(&schema),
                    lit(name),
                    lit(&routine.identity_arguments)
                )
            },
        )
        .await;
    }

    // The table: emptied, renamed, dropped.
    let count = format!("SELECT count(*) FROM {schema}.t");
    exec_ok(
        &session,
        &format!("INSERT INTO {schema}.t VALUES (1, 'a'), (2, 'b')"),
    )
    .await;
    assert_eq!(scalar(&session, &count).await, "2");
    exec_ok(&session, &d.truncate_sql(&t)).await;
    assert_eq!(scalar(&session, &count).await, "0");
    rename_then_drop(
        &session,
        |name| ObjectRef::Relation(in_schema(name), RelationKind::Table),
        "t",
        class('r'),
    )
    .await;

    // Everything was dropped one by one: the schema is empty, and DROP
    // SCHEMA, which does not cascade either, goes through.
    let left = session.objects(&schema).await.unwrap();
    assert_eq!(left, Default::default());
    let schemas = |name: &str| {
        format!(
            "SELECT count(*) FROM pg_catalog.pg_namespace WHERE nspname = {}",
            lit(name)
        )
    };
    exec_ok(
        &session,
        &d.drop_sql(&ObjectRef::Schema(schema.clone())).unwrap(),
    )
    .await;
    assert_eq!(scalar(&session, &schemas(&schema)).await, "0");

    // Create, rename and drop a schema, then a database.
    let made = format!("{schema} made");
    exec_ok(&session, &d.create_schema_sql(&made).unwrap()).await;
    rename_then_drop(
        &session,
        |name| ObjectRef::Schema(name.into()),
        &made,
        schemas,
    )
    .await;

    let database = format!("{schema}_db");
    exec_ok(&session, &d.create_database_sql(&database).unwrap()).await;
    rename_then_drop(
        &session,
        |name| ObjectRef::Database(name.into()),
        &database,
        |name| {
            format!(
                "SELECT count(*) FROM pg_catalog.pg_database WHERE datname = {}",
                lit(name)
            )
        },
    )
    .await;
}
