//! The Tools submenu (M10 Part 11b): per engine and kind of node, only the
//! entries that can apply to it, each disabled with its reason when the
//! connection's state forbids it; a real right-click; the foreign data
//! wrappers.

use super::tool_wizards::paints;
use super::*;
use crate::navigator::actions::ToolEntry;

impl Fixture {
    /// Tools ▸ of the tree row at `path`, a group per line.
    fn tool_menu(&self, cx: &mut TestAppContext, path: &[&str]) -> Vec<String> {
        let row = self.tree_row(cx, path);
        cx.update(|cx| {
            self.workspace
                .read(cx)
                .navigator(cx)
                .read(cx)
                .tool_menu_at(row, cx)
        })
    }

    /// Does a Tools ▸ entry on the tree row at `path`, as clicking it does.
    fn tool(&self, cx: &mut TestAppContext, entry: ToolEntry, path: &[&str]) {
        let row = self.tree_row(cx, path);
        let navigator = cx.update(|cx| self.workspace.read(cx).navigator(cx));
        self.step(cx, |window, cx| {
            navigator.update(cx, |navigator, cx| {
                let id = navigator.id_at(row, cx);
                navigator.perform_tool(entry, &id, window, cx)
            })
        });
    }

    /// Right-clicks the tree row at `path` and opens its Tools ▸.
    fn open_tools_menu(&self, cx: &mut TestAppContext, path: &[&str]) {
        let row = self.tree_row(cx, path);
        self.right_click(cx, ("node", row), None);
        click_menu_entry(self, cx, "Tools");
    }
}

#[gpui_kit::test]
fn the_tools_submenu_says_what_applies_per_engine_and_kind_of_node(cx: &mut TestAppContext) {
    if testing::available(Engine::Postgres) {
        let _scratch = Scratch::new(
            Engine::Postgres,
            &["DROP SCHEMA IF EXISTS kerno_ui_tools_menu CASCADE"],
            &[
                "CREATE SCHEMA kerno_ui_tools_menu",
                "CREATE TABLE kerno_ui_tools_menu.t (n int)",
                "CREATE MATERIALIZED VIEW kerno_ui_tools_menu.mv AS SELECT 1 AS n",
            ],
        );
        let schema = ["kerno", "kerno_ui_tools_menu"];
        let table = ["kerno", "kerno_ui_tools_menu", "Tables", "t"];
        let matview = ["kerno", "kerno_ui_tools_menu", "Materialized views", "mv"];
        let tail = [
            "Foreign data wrappers…",
            "Open dashboard, Session manager, Lock manager",
            "Create new task…, Tasks",
        ];
        // K27: a database also leads to its extensions manager.
        let database_tail = ["Foreign data wrappers…, Extensions…", tail[1], tail[2]];
        let app = connected(cx, Engine::Postgres);
        app.open_path(cx, &table);
        app.open_path(cx, &matview);
        let database = app.tool_menu(cx, &["kerno"]);
        assert_eq!(
            database[..2],
            [
                "Back up…, Global backup…, Restore…, Execute script…, Copy tables to another connection…",
                "Analyze…, Vacuum…",
            ]
        );
        assert_eq!(database[2..], database_tail);
        // A schema has no maintenance tool of its own: no group for it.
        let schema_menu = app.tool_menu(cx, &schema);
        assert_eq!(
            schema_menu[0],
            "Back up…, Global backup…, Restore…, Execute script…, Copy tables to another connection…"
        );
        assert_eq!(schema_menu[1..], tail);
        // A table lists what applies to a table, nothing greyed for its kind.
        let table_menu = app.tool_menu(cx, &table);
        assert_eq!(
            table_menu[..2],
            [
                "Back up…, Global backup…, Copy tables to another connection…",
                "Analyze…, Vacuum…, Truncate…"
            ]
        );
        assert_eq!(table_menu[2..], tail);
        let matview_menu = app.tool_menu(cx, &matview);
        assert_eq!(
            matview_menu[..2],
            [
                "Global backup…",
                "Analyze…, Vacuum…, Refresh materialized view…"
            ]
        );
        // The Truncate key on a table still runs it, now from Tools ▸.
        app.act(cx, Action::Truncate, &table);
        assert!(app.has(cx, "maintenance-dialog"));
        app.click(cx, "maintenance-close");

        // Read-only: what writes says so; backups and the rest stay.
        let mut config = testing::config(Engine::Postgres);
        config.read_only = true;
        let app = connected_as(cx, config);
        app.open_path(cx, &table);
        let database = app.tool_menu(cx, &["kerno"]);
        assert_eq!(
            database[0],
            "Back up…, Global backup…, Restore… (read-only connection), Execute script… \
             (read-only connection), Copy tables to another connection…"
        );
        assert!(
            database[1].starts_with("Analyze… (read-only connection)"),
            "{database:?}"
        );
        assert_eq!(database[2..], database_tail);
        assert_eq!(
            app.tool_menu(cx, &table)[1],
            "Analyze… (read-only connection), Vacuum… (read-only connection), Truncate… \
             (read-only connection)"
        );
    }

    for (engine, scratch) in [
        (Engine::MySql, "kerno_ui_tools_my"),
        (Engine::MariaDb, "kerno_ui_tools_maria"),
    ] {
        if !testing::available(engine) {
            continue;
        }
        let _scratch = Scratch::new(
            engine,
            &[&format!("DROP DATABASE IF EXISTS {scratch}")],
            &[
                &format!("CREATE DATABASE {scratch}"),
                &format!("CREATE TABLE {scratch}.t (id INT PRIMARY KEY)"),
            ],
        );
        let app = connected(cx, engine);
        let table = [scratch, "Tables", "t"];
        app.open_path(cx, &table);
        let tail = [
            "Open dashboard, Session manager, Lock manager",
            "Create new task…, Tasks",
        ];
        let database = app.tool_menu(cx, &[scratch]);
        assert_eq!(
            database[0], "Back up…, Restore…, Execute script…, Copy tables to another connection…",
            "{engine:?}"
        );
        assert_eq!(database[1..], tail, "{engine:?}");
        assert_eq!(
            app.tool_menu(cx, &table)[..2],
            [
                "Back up…, Copy tables to another connection…",
                "Analyze…, Check…, Optimize…, Repair…, Truncate…"
            ],
            "{engine:?}"
        );
        // Global backup and foreign data wrappers are PostgreSQL's.
        assert!(!database.concat().contains("Global backup"));
        assert!(!database.concat().contains("Foreign data wrappers"));
    }
}

#[gpui_kit::test]
fn a_right_click_opens_tools_and_each_entry_opens_its_tool(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let _scratch = Scratch::new(
        Engine::Postgres,
        &["DROP SCHEMA IF EXISTS kerno_ui_tools_click CASCADE"],
        &[
            "CREATE SCHEMA kerno_ui_tools_click",
            "CREATE TABLE kerno_ui_tools_click.t (n int)",
        ],
    );
    let table = ["kerno", "kerno_ui_tools_click", "Tables", "t"];
    let app = connected(cx, Engine::Postgres);
    app.open_path(cx, &table);

    // A real right-click on a table: Tools ▸ with what applies to a table;
    // what never could is not there at all.
    app.open_tools_menu(cx, &table);
    for label in [
        "Back up…",
        "Global backup…",
        "Analyze…",
        "Vacuum…",
        "Truncate…",
        "Foreign data wrappers…",
        "Open dashboard",
        "Session manager",
        "Lock manager",
        "Create new task…",
        "Tasks",
    ] {
        assert!(menu_entry(&app, cx, label).is_some(), "{label}");
    }
    for label in [
        "Restore…",
        "Restore… (into a database)",
        "Execute script…",
        "Execute script… (into a database)",
        "Refresh materialized view…",
        "Refresh materialized view… (on a materialized view)",
        "Disable trigger…",
        "Disable trigger… (on a trigger)",
    ] {
        assert!(menu_entry(&app, cx, label).is_none(), "{label}");
    }
    // Backup and Truncate are under Tools ▸ only.
    click_menu_entry(&app, cx, "Back up…");
    assert!(app.has(cx, "backup-dialog") && paints(&app, cx, "Back up the database “kerno”"));
    app.tap(cx, "backup-close");
    assert!(!app.has(cx, "backup-dialog"));

    // On the database: the global backup and the script wizards.
    app.open_tools_menu(cx, &["kerno"]);
    click_menu_entry(&app, cx, "Global backup…");
    assert!(paints(&app, cx, "Back up the whole server of"));
    app.tap(cx, "backup-close");
    app.open_tools_menu(cx, &["kerno"]);
    click_menu_entry(&app, cx, "Execute script…");
    assert!(paints(&app, cx, "Run a SQL script in the database “kerno”"));
    app.tap(cx, "backup-close");
    assert!(!app.has(cx, "backup-dialog"));

    // The foreign data wrappers of the database.
    app.open_tools_menu(cx, &["kerno"]);
    click_menu_entry(&app, cx, "Foreign data wrappers…");
    assert!(app.has(cx, "fdw-dialog"));
    app.click(cx, "fdw-close");
    assert!(!app.has(cx, "fdw-dialog"));

    // The administration, on its sessions, and the Tasks view.
    app.open_tools_menu(cx, &["kerno"]);
    click_menu_entry(&app, cx, "Session manager");
    let section = |app: &Fixture, cx: &mut TestAppContext| {
        let tab = app.admin(cx);
        cx.update(|cx| tab.read(cx).section())
    };
    assert_eq!(section(&app, cx), crate::admin::tab::SESSIONS);
    app.tool(cx, ToolEntry::LockManager, &table);
    assert_eq!(section(&app, cx), crate::admin::tab::LOCKS);
    app.open_tools_menu(cx, &["kerno"]);
    click_menu_entry(&app, cx, "Tasks");
    assert!(cx.update(|cx| app.workspace.read(cx).active_tasks_tab().is_some()));
    assert!(app.has(cx, "tasks-view"));

    // Create new task…: the tool first, then its wizard, whose main button
    // saves the task.
    app.open_tools_menu(cx, &table);
    click_menu_entry(&app, cx, "Create new task…");
    app.click(cx, ("task-tool", 0usize));
    assert!(app.has(cx, "backup-dialog") && paints(&app, cx, "Back up the database “kerno”"));
    app.tap(cx, "backup-close");

    // The connection's own Tools ▸, with a real right-click on its row.
    app.right_click(cx, ("connection", 0usize), None);
    click_menu_entry(&app, cx, "Tools");
    for label in [
        "Back up…",
        "Global backup…",
        "Foreign data wrappers…",
        "Tasks",
    ] {
        assert!(menu_entry(&app, cx, label).is_some(), "{label}");
    }
    click_menu_entry(&app, cx, "Open dashboard");
    assert!(cx.update(|cx| app.workspace.read(cx).active_admin_tab().is_some()));
}

#[gpui_kit::test]
fn foreign_data_wrappers_create_a_server_a_mapping_and_import_a_schema(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    // A database of the test's own: the extension, the server and the
    // mapping go with it. The server is this same container, by loopback.
    let database = "kerno_ui_tools_fdw";
    let _scratch = Scratch::new(
        Engine::Postgres,
        &["DROP DATABASE IF EXISTS kerno_ui_tools_fdw WITH (FORCE)"],
        &["CREATE DATABASE kerno_ui_tools_fdw"],
    );
    on_database(
        Engine::Postgres,
        Some(database),
        "CREATE SCHEMA kerno_ui_tools_src; \
         CREATE TABLE kerno_ui_tools_src.items AS SELECT generate_series(1, 2) AS n",
    );
    let app = connected(cx, Engine::Postgres);
    let target = app.target(cx, Some(database));
    let connections = app.connections.clone();
    let dialog = app
        .step(cx, |window, cx| {
            crate::fdw_dialog::open(connections, target, window, cx)
        })
        .expect("the dialog");
    assert!(app.has(cx, "fdw-dialog"));
    let listing = |cx: &mut TestAppContext| cx.update(|cx| dialog.read(cx).listing());
    app.wait_until(cx, "the wrappers", |_, cx| listing(cx).is_some());
    let read = listing(cx).unwrap();
    assert!(
        read.servers.is_empty() && read.mappings.is_empty(),
        "{read:?}"
    );
    // postgres_fdw is offered, not installed: Install… asks first.
    let at = read
        .extensions
        .iter()
        .filter(|(_, installed)| !installed)
        .position(|(name, _)| name == "postgres_fdw")
        .expect("postgres_fdw in the image");
    app.click(cx, ("fdw-install", at));
    assert!(app.has(cx, "sql-preview"));
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "postgres_fdw installed", |_, cx| {
        listing(cx).is_some_and(|read| read.wrappers.contains(&"postgres_fdw".to_string()))
    });

    // The form, its password typed for this run.
    let user = testing::config(Engine::Postgres).user;
    for (id, text) in [
        ("fdw-server", "kerno_ui_tools_loop"),
        ("fdw-host", "localhost"),
        ("fdw-port", "5432"),
        ("fdw-dbname", database),
        ("fdw-user", user.as_str()),
        ("fdw-password", testing::DB_PASSWORD),
        ("fdw-remote-schema", "kerno_ui_tools_src"),
        ("fdw-local-schema", "kerno_ui_tools_remote"),
    ] {
        app.click(cx, id);
        app.type_text(cx, text);
    }
    // The statement is shown with its password masked, here and in the
    // confirmation.
    let shown = cx.update(|cx| dialog.read(cx).shown_script(cx)).unwrap();
    assert!(
        shown.contains("password '***'") && !shown.contains(testing::DB_PASSWORD),
        "{shown}"
    );
    assert!(!paints(&app, cx, testing::DB_PASSWORD));
    app.click(cx, "fdw-create");
    assert!(app.has(cx, "sql-preview"));
    assert!(paints(&app, cx, "IMPORT FOREIGN SCHEMA") && !paints(&app, cx, testing::DB_PASSWORD));
    app.click(cx, "sql-confirm");
    app.wait_until(cx, "the server and its mapping", |_, cx| {
        listing(cx).is_some_and(|read| {
            read.servers.iter().any(|(name, wrapper, options)| {
                name == "kerno_ui_tools_loop"
                    && wrapper == "postgres_fdw"
                    && options.contains("host=localhost")
            }) && read
                .mappings
                .contains(&("kerno_ui_tools_loop".to_string(), user.clone()))
        })
    });
    // The remote table is read through the server.
    assert_eq!(
        on_database(
            Engine::Postgres,
            Some(database),
            "SELECT count(*) FROM kerno_ui_tools_remote.items"
        ),
        "2"
    );
    // The password was for that run only: the field is empty again, and
    // nothing of Kerno's store holds it.
    assert!(cx.update(|cx| dialog.read(cx).form(cx).password.is_empty()));
    assert!(!paints(&app, cx, testing::DB_PASSWORD));
    app.click(cx, "fdw-close");
}
