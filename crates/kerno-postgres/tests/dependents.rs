//! What a drop, a truncate or a rename reaches (C11), against the server's own answer: a
//! `DROP … CASCADE` tells in its notice what it dropped with the object. Every shape is built
//! on scratch schemas inside a transaction, compared, then rolled back: nothing is left on the
//! container. Skipped with a message when the container is down.

mod common;

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use common::{exec_ok, run};
use kerno_core::schema::{Change, Dependent, ObjectRef, RelationKind, RoutineKind, TableRef, Tie};
use kerno_core::{Error, Session};

const A: &str = "kerno_test_k14";
/// After A's objects in every collation: glibc's skips the dot of
/// `kerno_test_k14.v1`, Windows' and ICU's do not (K68).
const B: &str = "kerno_test_k14z";

fn table(schema: &str, name: &str) -> TableRef {
    TableRef::new(schema, name)
}

fn drop_of(object: ObjectRef) -> Change {
    Change::Drop {
        object,
        cascade: true,
    }
}

fn relation(schema: &str, name: &str, kind: RelationKind) -> ObjectRef {
    ObjectRef::Relation(table(schema, name), kind)
}

/// The scratch objects every shape starts from. `search_path` is `pg_catalog` alone for the
/// transaction, so the server names every object with its schema in its notice.
fn setup() -> String {
    format!(
        "SET LOCAL search_path = pg_catalog;
         CREATE SCHEMA {A}; CREATE SCHEMA {B};
         CREATE TYPE {A}.mood AS ENUM ('a', 'b');
         CREATE SEQUENCE {A}.shared;
         CREATE TABLE {A}.t (id serial PRIMARY KEY, n int DEFAULT nextval('{A}.shared'), m {A}.mood);
         CREATE INDEX ON {A}.t (n);
         CREATE VIEW {A}.v1 AS SELECT id, n FROM {A}.t;
         CREATE VIEW {B}.v2 AS SELECT id FROM {A}.v1;
         CREATE MATERIALIZED VIEW {A}.mv AS SELECT count(*) FROM {A}.t;
         CREATE TABLE {A}.child (id int PRIMARY KEY, t_id int REFERENCES {A}.t (id));
         CREATE TABLE {A}.grandchild (id int, child_id int REFERENCES {A}.child (id));
         CREATE TABLE {A}.holder (id int, row_of {A}.t);
         CREATE FUNCTION {A}.f(x {A}.t) RETURNS int LANGUAGE sql AS 'SELECT 1';
         CREATE FUNCTION {A}.g() RETURNS SETOF {A}.t LANGUAGE sql AS 'SELECT * FROM {A}.t';
         CREATE FUNCTION {A}.trg() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RETURN NEW; END';
         CREATE TRIGGER tt BEFORE INSERT ON {A}.child FOR EACH ROW EXECUTE FUNCTION {A}.trg();
         CREATE TRIGGER own BEFORE INSERT ON {A}.t FOR EACH ROW EXECUTE FUNCTION {A}.trg();
         CREATE TABLE {A}.parent_i (id int);
         CREATE TABLE {A}.child_i () INHERITS ({A}.parent_i);
         CREATE TABLE {A}.parted (id int) PARTITION BY RANGE (id);
         CREATE TABLE {A}.part1 PARTITION OF {A}.parted FOR VALUES FROM (0) TO (10);
         CREATE VIEW {B}.on_part AS SELECT id FROM {A}.part1;
         CREATE TABLE {A}.u (k int);
         CREATE UNIQUE INDEX u_k ON {A}.u (k);
         CREATE TABLE {A}.r (k int REFERENCES {A}.u (k));
         CREATE TABLE {A}.\"MixedT\" (id int);
         CREATE VIEW {A}.mixv AS SELECT id FROM {A}.\"MixedT\";
         CREATE TABLE {A}.alone (id int PRIMARY KEY);
         CREATE EXTENSION ltree SCHEMA {B};
         CREATE TABLE {B}.h (x {B}.ltree);
         CREATE VIEW {B}.hv AS SELECT x FROM {B}.h;"
    )
}

/// What the server's notices after `sql` name: "drop cascades to X" when one object went
/// with the target, "drop cascades to N other objects" and one detail line each when
/// several did, "truncate cascades to table \"x\"" once per table.
async fn cascaded(session: &Arc<dyn Session>, sql: &str) -> Vec<String> {
    let collected = run(session, sql)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
    let mut objects = Vec::new();
    for notice in collected.notices {
        let lines = match &notice.detail {
            Some(detail) if notice.message.ends_with("other objects") => detail.clone(),
            _ => notice.message.clone(),
        };
        for line in lines.lines() {
            let object = match line.strip_prefix("truncate cascades to table ") {
                Some(table) => table.trim_matches('"'),
                None => line
                    .strip_prefix("drop cascades to ")
                    .unwrap_or_else(|| panic!("{sql}: unexpected notice {line:?}")),
            };
            assert!(
                !object.contains("see server log"),
                "{sql}: the list was cut"
            );
            objects.push(object.to_string());
        }
    }
    objects
}

/// The server's description of every user object (what its notices say) and the identity
/// Kerno names it by.
async fn identities(session: &Arc<dyn Session>) -> HashMap<String, String> {
    common::rows(
        session,
        "SELECT pg_catalog.pg_describe_object(classid, objid, objsubid), \
                (pg_catalog.pg_identify_object(classid, objid, objsubid)).identity \
         FROM (SELECT DISTINCT classid, objid, objsubid FROM pg_catalog.pg_depend \
               WHERE classid <> 0 AND objid >= 16384) d",
    )
    .await
    .into_iter()
    .map(|row| (row[0].clone().unwrap(), row[1].clone().unwrap()))
    .collect()
}

/// Kerno's list for `change`, after checking it names exactly what `drop` (run in a
/// savepoint, then undone) told it dropped.
async fn compared(session: &Arc<dyn Session>, change: Change, drop: &str) -> Vec<Dependent> {
    exec_ok(session, "SAVEPOINT shape").await;
    let found = session
        .dependents(&change)
        .await
        .unwrap_or_else(|error| panic!("{change:?}: {error}"));
    let known = identities(session).await;
    let told = cascaded(session, drop).await;
    let expected: BTreeSet<String> = told
        .iter()
        .map(|said| {
            known
                .get(said)
                .cloned()
                .unwrap_or_else(|| panic!("{drop}: no identity for {said:?}"))
        })
        .collect();
    let names: BTreeSet<String> = found.iter().map(|d| d.name.clone()).collect();
    assert_eq!(
        names, expected,
        "{drop}: Kerno {found:#?}, the server {told:#?}"
    );
    assert_eq!(found.len(), told.len(), "{drop}: {found:#?}");
    exec_ok(session, "ROLLBACK TO SAVEPOINT shape").await;
    found
}

fn lines(found: &[Dependent]) -> Vec<String> {
    found
        .iter()
        .map(|d| format!("{} {}", d.kind, d.name))
        .collect()
}

/// C11: for each shape the critique names, Kerno's list is the server's own.
#[tokio::test]
async fn a_cascade_lists_what_the_server_drops() {
    let Some(session) = common::session().await else {
        return;
    };
    exec_ok(&session, "BEGIN").await;
    exec_ok(&session, &setup()).await;

    // A table: views on it and on them (in another schema), a materialized view, the
    // foreign key of another table (the constraint goes, not the table), functions taking or
    // returning its row type, a column of that type. Not its index, its own constraints and
    // trigger, the sequence its column owns: they go with it without a word.
    let found = compared(
        &session,
        drop_of(relation(A, "t", RelationKind::Table)),
        &format!("DROP TABLE {A}.t CASCADE"),
    )
    .await;
    assert_eq!(
        lines(&found),
        [
            format!("column {A}.holder.row_of"),
            format!("foreign key child_t_id_fkey on {A}.child"),
            format!("function {A}.f({A}.t)"),
            format!("function {A}.g()"),
            format!("materialized view {A}.mv"),
            format!("view {A}.v1"),
            format!("view {B}.v2"),
        ]
    );
    assert!(found.iter().all(|d| d.tie == Tie::Dropped));

    // A plain DROP is refused for the same list.
    let plain = session
        .dependents(&Change::Drop {
            object: relation(A, "t", RelationKind::Table),
            cascade: false,
        })
        .await
        .unwrap();
    assert_eq!(lines(&plain), lines(&found));
    assert!(plain.iter().all(|d| d.tie == Tie::Blocks));

    // An enum a column is typed with: the column goes, not its table.
    let found = compared(
        &session,
        drop_of(ObjectRef::Type(table(A, "mood"))),
        &format!("DROP TYPE {A}.mood CASCADE"),
    )
    .await;
    assert_eq!(lines(&found), [format!("column {A}.t.m")]);

    // A sequence a default reads.
    let found = compared(
        &session,
        drop_of(ObjectRef::Sequence(table(A, "shared"))),
        &format!("DROP SEQUENCE {A}.shared CASCADE"),
    )
    .await;
    assert_eq!(lines(&found), [format!("default value for {A}.t.n")]);

    // A trigger function.
    let function = |name: &str, arguments: &str| ObjectRef::Routine {
        schema: A.into(),
        name: name.into(),
        kind: RoutineKind::Function,
        identity_arguments: arguments.into(),
    };
    let found = compared(
        &session,
        drop_of(function("trg", "")),
        &format!("DROP FUNCTION {A}.trg() CASCADE"),
    )
    .await;
    assert_eq!(
        lines(&found),
        [
            format!("trigger own on {A}.t"),
            format!("trigger tt on {A}.child")
        ]
    );
    // An overload found by its arguments, as the tree names it.
    compared(
        &session,
        drop_of(function("f", &format!("x {A}.t"))),
        &format!("DROP FUNCTION {A}.f({A}.t) CASCADE"),
    )
    .await;

    // A view on a view; a view on a table named in mixed case.
    let found = compared(
        &session,
        drop_of(relation(A, "v1", RelationKind::View)),
        &format!("DROP VIEW {A}.v1 CASCADE"),
    )
    .await;
    assert_eq!(lines(&found), [format!("view {B}.v2")]);
    compared(
        &session,
        drop_of(relation(A, "MixedT", RelationKind::Table)),
        &format!("DROP TABLE {A}.\"MixedT\" CASCADE"),
    )
    .await;

    // An inheriting table goes with its parent; a partition goes without a word, a view on
    // it is listed.
    let found = compared(
        &session,
        drop_of(relation(A, "parent_i", RelationKind::Table)),
        &format!("DROP TABLE {A}.parent_i CASCADE"),
    )
    .await;
    assert_eq!(lines(&found), [format!("table {A}.child_i")]);
    let found = compared(
        &session,
        drop_of(relation(A, "parted", RelationKind::PartitionedTable)),
        &format!("DROP TABLE {A}.parted CASCADE"),
    )
    .await;
    assert_eq!(lines(&found), [format!("view {B}.on_part")]);

    // A unique index a foreign key relies on.
    let found = compared(
        &session,
        drop_of(ObjectRef::Index {
            table: table(A, "u"),
            name: "u_k".into(),
        }),
        &format!("DROP INDEX {A}.u_k CASCADE"),
    )
    .await;
    assert_eq!(lines(&found), [format!("foreign key r_k_fkey on {A}.r")]);

    // An extension: its own members go without a word, what uses them is listed.
    let found = compared(
        &session,
        Change::DropExtension {
            name: "ltree".into(),
            cascade: true,
        },
        "DROP EXTENSION ltree CASCADE",
    )
    .await;
    assert_eq!(
        lines(&found),
        [format!("column {B}.h.x"), format!("view {B}.hv")]
    );

    // A schema: everything in it, and what another schema built on it.
    let found = compared(
        &session,
        drop_of(ObjectRef::Schema(A.into())),
        &format!("DROP SCHEMA {A} CASCADE"),
    )
    .await;
    assert!(
        lines(&found).contains(&format!("view {B}.v2")),
        "{found:#?}"
    );
    assert!(
        lines(&found).contains(&format!("view {B}.on_part")),
        "{found:#?}"
    );

    // Nothing depends on it: an empty list, and the server says nothing either.
    let found = compared(
        &session,
        drop_of(relation(A, "alone", RelationKind::Table)),
        &format!("DROP TABLE {A}.alone CASCADE"),
    )
    .await;
    assert!(found.is_empty(), "{found:#?}");

    exec_ok(&session, "ROLLBACK").await;
}

/// C11: an object that is gone is an error, never an empty list that would read as "nothing
/// depends on it"; a database has no list.
#[tokio::test]
async fn a_missing_object_is_an_error_not_an_empty_list() {
    let Some(session) = common::session().await else {
        return;
    };
    let missing = session
        .dependents(&drop_of(relation(
            "kerno_test_k14_none",
            "t",
            RelationKind::Table,
        )))
        .await;
    assert!(
        matches!(&missing, Err(Error::Db(error)) if error.message.contains("not found")),
        "{missing:?}"
    );
    let database = session
        .dependents(&drop_of(ObjectRef::Database("kerno".into())))
        .await;
    assert!(
        matches!(database, Err(Error::Unsupported(_))),
        "{database:?}"
    );
}

/// C11: TRUNCATE … CASCADE empties the tables that reference this one, and those that
/// reference them: Kerno's list is the server's notices.
#[tokio::test]
async fn a_truncate_cascade_lists_the_tables_it_empties() {
    let Some(session) = common::session().await else {
        return;
    };
    exec_ok(&session, "BEGIN").await;
    exec_ok(&session, &setup()).await;
    let found = session
        .dependents(&Change::TruncateCascade(table(A, "t")))
        .await
        .unwrap();
    assert_eq!(
        lines(&found),
        [format!("table {A}.child"), format!("table {A}.grandchild")]
    );
    assert!(found.iter().all(|d| d.tie == Tie::Emptied));
    let told: BTreeSet<String> = cascaded(&session, &format!("TRUNCATE {A}.t CASCADE"))
        .await
        .into_iter()
        .collect();
    let names: BTreeSet<String> = found
        .iter()
        .map(|d| d.name.rsplit('.').next().unwrap().to_string())
        .collect();
    assert_eq!(names, told);
    exec_ok(&session, "ROLLBACK").await;
}

/// C11: a rename leaves broken the bodies kept as text (PL/pgSQL, a quoted SQL body) that
/// name the object; views and `BEGIN ATOMIC` bodies follow it. Checked on the server: after
/// the rename the listed functions fail and the others still run.
#[tokio::test]
async fn a_rename_lists_the_bodies_that_name_it() {
    let Some(session) = common::session().await else {
        return;
    };
    exec_ok(&session, "BEGIN").await;
    exec_ok(
        &session,
        &format!(
            "CREATE SCHEMA {A};
             CREATE TABLE {A}.k14_orders (id int);
             CREATE VIEW {A}.v AS SELECT id FROM {A}.k14_orders;
             CREATE FUNCTION {A}.pl() RETURNS bigint LANGUAGE plpgsql AS
               $$BEGIN RETURN (SELECT count(*) FROM {A}.K14_ORDERS); END$$;
             CREATE FUNCTION {A}.sq() RETURNS bigint LANGUAGE sql AS 'SELECT count(*) FROM {A}.k14_orders';
             CREATE PROCEDURE {A}.pr() LANGUAGE plpgsql AS $$BEGIN PERFORM 1 FROM {A}.k14_orders; END$$;
             CREATE FUNCTION {A}.atomic() RETURNS bigint LANGUAGE sql
               BEGIN ATOMIC SELECT count(*) FROM {A}.k14_orders; END;
             CREATE FUNCTION {A}.other() RETURNS bigint LANGUAGE sql AS 'SELECT 1 AS k14_orders_archive'"
        ),
    )
    .await;
    let found = session
        .dependents(&Change::Rename(relation(
            A,
            "k14_orders",
            RelationKind::Table,
        )))
        .await
        .unwrap();
    assert_eq!(
        lines(&found),
        [
            format!("function {A}.pl()"),
            format!("function {A}.sq()"),
            format!("procedure {A}.pr()"),
        ]
    );
    assert!(found.iter().all(|d| d.tie == Tie::Mentions));
    // What the server does after the rename: the listed bodies fail, the rest runs.
    exec_ok(
        &session,
        &format!("ALTER TABLE {A}.k14_orders RENAME TO k14_sales"),
    )
    .await;
    for call in ["SELECT count(*) FROM {A}.v", "SELECT {A}.atomic()"] {
        exec_ok(&session, &call.replace("{A}", A)).await;
    }
    for call in ["SELECT {A}.pl()", "SELECT {A}.sq()", "CALL {A}.pr()"] {
        exec_ok(&session, "SAVEPOINT call").await;
        assert!(
            run(&session, &call.replace("{A}", A)).await.is_err(),
            "{call} still runs"
        );
        exec_ok(&session, "ROLLBACK TO SAVEPOINT call").await;
    }
    exec_ok(&session, "ROLLBACK").await;
}
