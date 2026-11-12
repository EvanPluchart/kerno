//! The statements the navigator generates, run on MySQL and MariaDB: the
//! four table templates with their parameters bound, and rename, truncate,
//! drop and create for every kind of object, checked through introspection
//! and `information_schema`. Skipped with a message when the containers
//! are down.

mod common;

use std::future::Future;
use std::sync::Arc;

use common::{db_error, engines, exec_ok, rows, run, scalar};
use kerno_core::schema::{ObjectRef, RelationKind, RoutineKind, TableRef};
use kerno_core::sql::{
    ParamKey, bind_params, delete_template, find_params, insert_template, select_template,
    update_template,
};
use kerno_core::{Engine, Session};

/// Error number of a syntax error.
const SYNTAX_ERROR: &str = "1064";

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

/// Whether the server refused `sql` as a syntax error.
async fn is_syntax_error(session: &Arc<dyn Session>, sql: &str) -> bool {
    let error = db_error(session, sql).await;
    error
        .code
        .as_deref()
        .is_some_and(|code| code.starts_with(SYNTAX_ERROR))
}

#[tokio::test]
async fn templates_are_valid_sql_once_their_parameters_are_bound() {
    for engine in engines() {
        let session = common::session(engine).await;
        let database = common::scratch_database("generate_templates");
        let cleanup = [format!("DROP DATABASE IF EXISTS {database}")];
        cleaning_up(&session, &cleanup, templates(session.clone(), database)).await;
    }
}

async fn templates(session: Arc<dyn Session>, database: String) {
    let d = session.dialect();
    exec_ok(&session, &format!("CREATE DATABASE {database}")).await;
    exec_ok(
        &session,
        &format!(
            "CREATE TABLE {database}.`order items` (\
               id INT AUTO_INCREMENT PRIMARY KEY, \
               `order` INT NOT NULL, \
               `full name` VARCHAR(50), \
               `odd ``name``` VARCHAR(50), \
               qty INT NOT NULL DEFAULT 1, \
               doubled INT GENERATED ALWAYS AS (qty * 2) STORED, \
               tripled INT GENERATED ALWAYS AS (qty * 3) VIRTUAL)"
        ),
    )
    .await;
    let table = TableRef::new(database.clone(), "order items");
    let structure = session.table_structure(&table).await.unwrap();
    assert_eq!(structure.columns.len(), 7);

    // INSERT: the generated columns are left to the server; the
    // auto-increment key is offered and takes DEFAULT.
    let insert = insert_template(d, &table, &structure.columns);
    let values = [
        ("id", "DEFAULT"),
        ("order", "7"),
        ("full_name", "'Ada'"),
        ("odd__name_", "'x'"),
        ("qty", "3"),
    ];
    exec_ok(&session, &bind(&session, &insert, &values)).await;

    // SELECT: every column, more than fit on a line.
    let select = select_template(d, &table, &structure.columns, 10);
    assert_eq!(select.lines().count(), 10, "{select}");
    assert_eq!(
        rows(&session, &select).await,
        [cells(&["1", "7", "Ada", "x", "3", "6", "9"])]
    );
    let star = select_template(d, &table, &[], 10);
    assert_eq!(rows(&session, &star).await.len(), 1);

    // UPDATE: the row is addressed by its key.
    let update = update_template(d, &table, &structure);
    let values = [
        ("order", "8"),
        ("full_name", "NULL"),
        ("odd__name_", "'y'"),
        ("qty", "5"),
        ("id", "1"),
    ];
    exec_ok(&session, &bind(&session, &update, &values)).await;
    assert_eq!(
        rows(&session, &select).await,
        [cells(&["1", "8", "NULL", "y", "5", "10", "15"])]
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
            "CREATE TABLE {database}.tags (order_id INT, tag VARCHAR(20), \
             PRIMARY KEY (order_id, tag))"
        ),
    )
    .await;
    exec_ok(
        &session,
        &format!("INSERT INTO {database}.tags VALUES (1, 'a')"),
    )
    .await;
    let tags = TableRef::new(database.clone(), "tags");
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
        rows(&session, &format!("SELECT * FROM {database}.tags")).await,
        [cells(&["2", "b"])]
    );

    // Without a key the templates stop at WHERE: the server refuses them
    // and every row is still there.
    exec_ok(
        &session,
        &format!("CREATE TABLE {database}.no_key (a INT, `b c` VARCHAR(20))"),
    )
    .await;
    exec_ok(
        &session,
        &format!("INSERT INTO {database}.no_key VALUES (1, 'x'), (2, 'y')"),
    )
    .await;
    let no_key = TableRef::new(database.clone(), "no_key");
    let no_key_structure = session.table_structure(&no_key).await.unwrap();
    assert_eq!(no_key_structure.row_identity(), None);
    let count = format!("SELECT count(*) FROM {}", d.table_name(&no_key));
    let update = update_template(d, &no_key, &no_key_structure);
    let update = bind(&session, &update, &[("a", "9"), ("b_c", "'z'")]);
    assert!(is_syntax_error(&session, &update).await, "{update}");
    let delete = delete_template(d, &no_key, &no_key_structure);
    assert!(is_syntax_error(&session, &delete).await, "{delete}");
    assert_eq!(
        rows(
            &session,
            &format!("SELECT * FROM {database}.no_key ORDER BY a")
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

/// Drops `object` and checks that `count`, a query counting it, goes from
/// one to zero.
async fn drop_checked(session: &Arc<dyn Session>, object: &ObjectRef, count: &str) {
    assert_eq!(scalar(session, count).await, "1", "{object:?} exists");
    let drop = session.dialect().drop_sql(object).unwrap();
    exec_ok(session, &drop).await;
    assert_eq!(scalar(session, count).await, "0", "{drop}");
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
    // A name that must be quoted.
    let new = format!("{old} `renamed`");
    assert_eq!(scalar(session, &count(old)).await, "1", "{old} exists");
    let rename = session.dialect().rename_sql(&object(old), &new).unwrap();
    exec_ok(session, &rename).await;
    assert_eq!(scalar(session, &count(old)).await, "0", "{rename}");
    drop_checked(session, &object(&new), &count(&new)).await;
}

#[tokio::test]
async fn rename_truncate_drop_and_create_do_what_they_say() {
    for engine in engines() {
        let session = common::session(engine).await;
        let database = common::scratch_database("generate_objects");
        let cleanup = [
            format!("DROP DATABASE IF EXISTS {database}"),
            format!("DROP DATABASE IF EXISTS `{database}-made`"),
        ];
        cleaning_up(&session, &cleanup, objects(session.clone(), database)).await;
    }
}

async fn objects(session: Arc<dyn Session>, database: String) {
    let d = session.dialect();
    let lit = |text: &str| d.quote_string(text);
    let mariadb = session.engine() == Engine::MariaDb;
    for sql in [
        "CREATE DATABASE {db}",
        "CREATE TABLE {db}.t (id INT AUTO_INCREMENT PRIMARY KEY, v VARCHAR(20), \
         INDEX t_v_idx (v))",
        "CREATE TABLE {db}.keyed (id INT PRIMARY KEY, v INT)",
        "CREATE VIEW {db}.v AS SELECT id FROM {db}.t",
        "CREATE FUNCTION {db}.f() RETURNS INT DETERMINISTIC RETURN 1",
        "CREATE PROCEDURE {db}.p() SELECT 1",
        "CREATE TRIGGER {db}.trg BEFORE INSERT ON {db}.t FOR EACH ROW SET NEW.v = UPPER(NEW.v)",
        "CREATE EVENT {db}.ev ON SCHEDULE EVERY 1 YEAR DISABLE DO SET @kerno_noop = 1",
    ] {
        exec_ok(&session, &sql.replace("{db}", &database)).await;
    }
    if mariadb {
        exec_ok(&session, &format!("CREATE SEQUENCE {database}.seq")).await;
    }
    let in_database = |name: &str| TableRef::new(database.clone(), name);
    let t = in_database("t");
    // Objects as introspection lists them.
    let listed = session.objects(&database).await.unwrap();

    // Rows of an information_schema table about one object of the database.
    let info = |table: &'static str, schema_column: &'static str, condition: &'static str| {
        let database = lit(&database);
        move |name: &str| {
            format!(
                "SELECT count(*) FROM information_schema.{table} \
                 WHERE {schema_column} = {database} AND {condition} = {}",
                lit(name)
            )
        }
    };
    let tables = |table_type: &'static str| {
        let named = info("TABLES", "TABLE_SCHEMA", "TABLE_NAME");
        move |name: &str| format!("{} AND TABLE_TYPE = '{table_type}'", named(name))
    };

    // Trigger and routines: dropped; these engines cannot rename them.
    assert_eq!(listed.triggers.len(), 1);
    let trigger = ObjectRef::Trigger {
        table: in_database(&listed.triggers[0].table),
        name: listed.triggers[0].name.clone(),
    };
    assert!(d.rename_sql(&trigger, "x").is_err());
    let count = info("TRIGGERS", "TRIGGER_SCHEMA", "TRIGGER_NAME")("trg");
    drop_checked(&session, &trigger, &count).await;

    let kinds: Vec<_> = listed.routines.iter().map(|r| (&*r.name, r.kind)).collect();
    assert_eq!(
        kinds,
        [("f", RoutineKind::Function), ("p", RoutineKind::Procedure)]
    );
    for routine in &listed.routines {
        let object = ObjectRef::Routine {
            schema: database.clone(),
            name: routine.name.clone(),
            kind: routine.kind,
            identity_arguments: routine.identity_arguments.clone(),
        };
        assert!(d.rename_sql(&object, "x").is_err());
        let count = info("ROUTINES", "ROUTINE_SCHEMA", "ROUTINE_NAME")(&routine.name);
        drop_checked(&session, &object, &count).await;
    }

    // Event: renamed within its database, then dropped.
    assert_eq!(listed.events.len(), 1);
    rename_then_drop(
        &session,
        |name| ObjectRef::Event(in_database(name)),
        &listed.events[0].name,
        info("EVENTS", "EVENT_SCHEMA", "EVENT_NAME"),
    )
    .await;

    // Index: renamed and dropped through its table.
    let indexes = |table: &'static str| {
        let database = lit(&database);
        move |name: &str| {
            format!(
                "SELECT count(DISTINCT INDEX_NAME) FROM information_schema.STATISTICS \
                 WHERE TABLE_SCHEMA = {database} AND TABLE_NAME = '{table}' AND INDEX_NAME = {}",
                lit(name)
            )
        }
    };
    rename_then_drop(
        &session,
        |name| ObjectRef::Index {
            table: t.clone(),
            name: name.into(),
        },
        "t_v_idx",
        indexes("t"),
    )
    .await;
    // The primary key is the index called PRIMARY.
    let keyed = in_database("keyed");
    let primary = ObjectRef::Index {
        table: keyed.clone(),
        name: "PRIMARY".into(),
    };
    drop_checked(&session, &primary, &indexes("keyed")("PRIMARY")).await;
    let structure = session.table_structure(&keyed).await.unwrap();
    assert_eq!(structure.row_identity(), None);

    // View and sequence: renamed like tables, dropped by their own kind.
    rename_then_drop(
        &session,
        |name| ObjectRef::Relation(in_database(name), RelationKind::View),
        "v",
        tables("VIEW"),
    )
    .await;
    let sequence = |name: &str| ObjectRef::Sequence(in_database(name));
    if mariadb {
        assert_eq!(listed.sequences.len(), 1);
        rename_then_drop(
            &session,
            sequence,
            &listed.sequences[0].name,
            tables("SEQUENCE"),
        )
        .await;
    } else {
        assert!(d.rename_sql(&sequence("seq"), "x").is_err());
        assert!(d.drop_sql(&sequence("seq")).is_err());
    }

    // The table: emptied, renamed (its rows and database stay), dropped.
    let count = format!("SELECT count(*) FROM {database}.t");
    exec_ok(
        &session,
        &format!("INSERT INTO {database}.t (v) VALUES ('a'), ('b')"),
    )
    .await;
    assert_eq!(scalar(&session, &count).await, "2");
    exec_ok(&session, &d.truncate_sql(&t)).await;
    assert_eq!(scalar(&session, &count).await, "0");
    for name in ["t", "keyed"] {
        rename_then_drop(
            &session,
            |name| ObjectRef::Relation(in_database(name), RelationKind::Table),
            name,
            tables("BASE TABLE"),
        )
        .await;
    }

    // Everything was dropped one by one; the database goes last.
    let left = session.objects(&database).await.unwrap();
    assert_eq!(left, Default::default());
    let databases = |name: &str| {
        format!(
            "SELECT count(*) FROM information_schema.SCHEMATA WHERE SCHEMA_NAME = {}",
            lit(name)
        )
    };
    let object = ObjectRef::Database(database.clone());
    assert!(d.rename_sql(&object, "x").is_err());
    drop_checked(&session, &object, &databases(&database)).await;

    // Create a database, and drop it as the schema it is on these engines.
    let made = format!("{database}-made");
    assert!(d.create_schema_sql(&made).is_err());
    exec_ok(&session, &d.create_database_sql(&made).unwrap()).await;
    drop_checked(
        &session,
        &ObjectRef::Schema(made.clone()),
        &databases(&made),
    )
    .await;
}
