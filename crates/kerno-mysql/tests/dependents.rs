//! What a drop or a rename reaches on MySQL and MariaDB (C11), against what the server then
//! does: the foreign keys Kerno lists are the ones the server refuses the drop for, the views
//! it lists fail once the table is gone, the objects a rename lists fail once it ran. On
//! scratch databases, dropped at the end. Skipped with a message when the containers are down.

mod common;

use std::sync::Arc;

use common::{exec_ok, run};
use kerno_core::schema::{Change, Dependent, ObjectRef, RelationKind, RoutineKind, TableRef, Tie};
use kerno_core::{Engine, Error, Session};

const A: &str = "kerno_test_k14";
const B: &str = "kerno_test_k14b";

fn lines(found: &[Dependent]) -> Vec<(Tie, String)> {
    found
        .iter()
        .map(|d| (d.tie, format!("{} {}", d.kind, d.name)))
        .collect()
}

fn relation(name: &str, kind: RelationKind) -> ObjectRef {
    ObjectRef::Relation(TableRef::new(A, name), kind)
}

async fn clean(session: &Arc<dyn Session>) {
    exec_ok(session, &format!("DROP DATABASE IF EXISTS {A}")).await;
    exec_ok(session, &format!("DROP DATABASE IF EXISTS {B}")).await;
}

async fn setup(session: &Arc<dyn Session>) {
    clean(session).await;
    for sql in [
        format!("CREATE DATABASE {A}"),
        format!("CREATE DATABASE {B}"),
        format!("CREATE TABLE {A}.k14_parent (id INT PRIMARY KEY)"),
        format!(
            "CREATE TABLE {A}.child (id INT PRIMARY KEY, p INT, \
             CONSTRAINT child_parent_fk FOREIGN KEY (p) REFERENCES {A}.k14_parent (id))"
        ),
        format!(
            "CREATE TABLE {A}.selfref (id INT PRIMARY KEY, up INT, \
             FOREIGN KEY (up) REFERENCES {A}.selfref (id))"
        ),
        format!("CREATE VIEW {A}.v AS SELECT id FROM {A}.k14_parent"),
        format!("CREATE VIEW {A}.vv AS SELECT id FROM {A}.v"),
        format!("CREATE VIEW {B}.x AS SELECT id FROM {A}.k14_parent"),
        format!("CREATE VIEW {A}.other AS SELECT 1 AS k14_parent_archive"),
        format!("CREATE PROCEDURE {A}.p() SELECT count(*) FROM {A}.K14_PARENT"),
        format!(
            "CREATE TRIGGER {A}.trg BEFORE INSERT ON {A}.child FOR EACH ROW \
             SET @n = (SELECT count(*) FROM {A}.k14_parent)"
        ),
        format!(
            "CREATE EVENT {A}.e ON SCHEDULE EVERY 1 DAY DISABLE \
             DO DELETE FROM {A}.k14_parent WHERE id < 0"
        ),
    ] {
        exec_ok(session, &sql).await;
    }
}

/// C11: Drop on a table lists the foreign keys of other tables that point at it (the server
/// refuses the drop while they exist) and the views over it (they stay and fail once it is
/// gone): MySQL from `VIEW_TABLE_USAGE`, with the views over those views; MariaDB by a search
/// of the view definitions, said to be one.
#[tokio::test]
async fn a_drop_lists_the_keys_that_refuse_it_and_the_views_it_breaks_and_a_rename_what_names_it() {
    for engine in common::engines() {
        let session = common::session(engine).await;
        setup(&session).await;
        let found = session
            .dependents(&Change::Drop {
                object: relation("k14_parent", RelationKind::Table),
                cascade: false,
            })
            .await
            .unwrap();
        let key = (
            Tie::Blocks,
            format!("foreign key child_parent_fk on {A}.child"),
        );
        let views = match engine {
            Engine::MySql => vec![
                (Tie::Breaks, format!("view {A}.v")),
                (Tie::Breaks, format!("view {A}.vv")),
                (Tie::Breaks, format!("view {B}.x")),
            ],
            _ => vec![
                (Tie::Mentions, format!("view {A}.v")),
                (Tie::Mentions, format!("view {B}.x")),
            ],
        };
        assert_eq!(
            lines(&found),
            [vec![key], views.clone()].concat(),
            "{engine:?}"
        );

        // The server agrees: refused for that key, and the views fail once the table is gone.
        let refused = run(&session, &format!("DROP TABLE {A}.k14_parent")).await;
        let Err(Error::Db(error)) = refused else {
            panic!("{engine:?}: {refused:?}");
        };
        match engine {
            Engine::MySql => assert!(error.message.contains("child_parent_fk"), "{error:?}"),
            _ => assert_eq!(error.code.as_deref(), Some("1451 (23000)"), "{error:?}"),
        }
        exec_ok(&session, &format!("DROP TABLE {A}.child")).await;
        exec_ok(&session, &format!("DROP TABLE {A}.k14_parent")).await;
        for (_, view) in &views {
            let view = view.trim_start_matches("view ");
            let read = run(&session, &format!("SELECT * FROM {view}")).await;
            assert!(
                matches!(&read, Err(Error::Db(e)) if e.code.as_deref() == Some("1356 (HY000)")),
                "{engine:?}: {view}: {read:?}"
            );
        }

        // A table's key to itself refuses nothing.
        let found = session
            .dependents(&Change::Drop {
                object: relation("selfref", RelationKind::Table),
                cascade: false,
            })
            .await
            .unwrap();
        assert!(found.is_empty(), "{engine:?}: {found:#?}");
        exec_ok(&session, &format!("DROP TABLE {A}.selfref")).await;

        // What is not read is said to be: never an empty list.
        let routine = session
            .dependents(&Change::Drop {
                object: ObjectRef::Routine {
                    schema: A.into(),
                    name: "p".into(),
                    kind: RoutineKind::Procedure,
                    identity_arguments: String::new(),
                },
                cascade: false,
            })
            .await;
        assert!(
            matches!(routine, Err(Error::Unsupported(_))),
            "{engine:?}: {routine:?}"
        );

        renamed(&session, engine).await;
        clean(&session).await;
    }
}

/// C11: a rename lists every view, routine, trigger and event whose source names the table,
/// in any database, as a text search; each of them fails once the table is renamed (none
/// follows a rename on either engine). A longer name that contains it is not listed. (Run by
/// the test above: the two share the scratch databases.)
async fn renamed(session: &Arc<dyn Session>, engine: Engine) {
    setup(session).await;
    let found = session
        .dependents(&Change::Rename(relation("k14_parent", RelationKind::Table)))
        .await
        .unwrap();
    assert_eq!(
        lines(&found),
        [
            (Tie::Mentions, format!("event {A}.e")),
            (Tie::Mentions, format!("procedure {A}.p")),
            (Tie::Mentions, format!("trigger trg on {A}.child")),
            (Tie::Mentions, format!("view {A}.v")),
            (Tie::Mentions, format!("view {B}.x")),
        ],
        "{engine:?}"
    );
    exec_ok(
        session,
        &format!("RENAME TABLE {A}.k14_parent TO {A}.k14_renamed"),
    )
    .await;
    for broken in [
        format!("SELECT * FROM {A}.v"),
        format!("SELECT * FROM {B}.x"),
        format!("CALL {A}.p()"),
        format!("INSERT INTO {A}.child VALUES (1, NULL)"),
    ] {
        assert!(
            run(session, &broken).await.is_err(),
            "{engine:?}: {broken} still runs"
        );
    }
}
