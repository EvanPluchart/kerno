# Architecture

Kerno is a Cargo workspace of four crates. The dependency direction is one way: the app depends on the
drivers and the core; the drivers depend on the core; the core depends on neither.

| Crate | Role |
| ----- | ---- |
| `crates/kerno-core` | Engine-neutral core, no UI dependency: the `Driver`, `Session` and `Dialect` traits, the value and result model, connection settings, SSH tunnel, secrets, app storage, SQL helpers (splitting, parameters, classification, completion, formatting; all on Kerno's own tolerant lexer, no SQL parser crate, D-010), table browsing and its filter (`browse.rs`), export and import, pending-edit SQL, table drafts and their DDL, the rule that decides whether a query result can be edited (`editable.rs`), the figures of a selection of cells (`aggregate.rs`), environments (`environment.rs`), the plain-words explanation of connection failures (`error::explain`), the client tools (`tools.rs`) and the update rules (`update.rs`). |
| `crates/kerno-postgres` | `PostgresDriver` on `tokio-postgres` with rustls. |
| `crates/kerno-mysql` | `MysqlDriver` on `mysql_async`; one driver for MySQL and MariaDB. |
| `crates/kerno` | The app binary (GPUI through the `gpui-kit` facade, D-001 in [DECISIONS.md](DECISIONS.md)): windows, views, actions, keymap, theme. |

The pinned toolchain is in `rust-toolchain.toml`; dependency licences are checked by `deny.toml`.

## The driver trait

Defined in `crates/kerno-core/src/driver.rs`. It is the only abstraction an engine plugs into.

`Driver` (one per engine family, registered at startup):

- `engines() -> &'static [Engine]`: the engines it speaks (the MySQL driver returns MySQL and MariaDB).
- `connect(&ConnectParams) -> Arc<dyn Session>`. The params name the host and port to dial; when an SSH
  tunnel is configured, `Connector` (`connector.rs`) has already opened it and the params point at the local
  end, while TLS names are still checked against the original host.

`Session` is one server connection. Its methods take `&self` and serialize internally: one statement at a
time, except `cancel`, which never waits.

- Identity: `engine`, `dialect`, `server` (version), `database` (follows `USE` on MySQL), `tx_state`
  (`Idle`, `Open`, `Failed`, after the last statement).
- `execute(sql, &mut ExecSink)`: runs one statement or a script and writes what it produces into the sink.
- `cancel`, `ping`, `close`.
- Introspection with engine-neutral types from `schema.rs`: `databases`, `schemas` (empty where the engine has
  no schemas inside databases), `objects(schema)`, `table_structure`, `ddl`, `extensions`, and
  `completion_catalog` for the editor.
- Administration: `admin_query(AdminView)` returns the SQL of a view (`Sessions`, `Roles`, `Grants`,
  `DatabaseSizes`, `TableSizes`, `Settings`) or `None`; `cancel_session` and `terminate_session` act on
  another session by id.

There are no transaction methods. A transaction is ordinary SQL sent through `execute`; `tx_state` reports
what the server says. Cancelling works through a side channel of the engine (PostgreSQL cancel request,
`KILL QUERY` on a second connection for MySQL).

`Dialect` (`dialect.rs`, returned by `Session::dialect`) holds everything that is text rather than I/O.
Required methods: `engine`, `has_schemas`, `lex_rules`, `quote_ident`, `quote_string`, `quote_bytes`,
`type_for(TypeKind)`, `explain(sql, analyze)`, `estimated_rows_sql`. Provided defaults cover `LIMIT/OFFSET`,
table names, `literal`, default-values inserts, and many statement builders that an engine overrides or leaves
refusing: rename and drop, cancel and terminate session SQL, maintenance tools, truncate, create schema and
database, replace source, extensions, enums, `create_table_sql` and `alter_table_sql` (from the table
editor's `TableDraft` and `TableChange`), `parse_plan` (EXPLAIN output to the `Plan` tree), and the
read-only session statement. The UI asks the dialect and never branches on the engine name for SQL text.

### Execution and the result model

`exec.rs` and `result.rs`.

A driver writes into an `ExecSink`; the consumer reads `ExecEvent`s from the matching `Execution`:
`Columns`, `Rows(RowBatch)`, `Complete { rows_affected }`, `Notice`, and finally `Finished(Result<()>)`.
Rows flow only as fast as the consumer asks (`Execution::request_rows`, `request_all`). The driver calls
`sink.row()`, appends one cell per column to the returned `BatchBuilder`, then awaits `sink.end_row()`, which
waits when the consumer has not asked for more; the driver must not read the next row from the socket before
it returns. When the consumer goes away or asks to stop, the sink reports `Stopped` and the driver cancels the
statement on the server, leaves the connection ready, and returns `Error::Cancelled`. This is how a 1M-row
`SELECT` stays bounded by what was asked for.

Values are the server's own text form (raw bytes for `TypeKind::Binary` columns), with `NULL` distinct from
the empty string. Rows are packed into `RowBatch` arenas, one byte buffer plus one offset per cell, never an
allocation per cell; `ResultStore` strings batches together and addresses rows globally. Each `Column` has a
name, the server's type name and a `TypeKind` (integer, float, decimal, boolean, text, binary, date, time,
date-time with and without zone, interval, uuid, json, array, enum, bit, other), which decides alignment,
sort order and literal quoting. A column also carries its `origin` (`Option<ColumnOrigin>`): the table column the
server says it shows, as PostgreSQL's table OID and attribute number, or as the database, table and column
names MySQL and MariaDB send, whatever aliases the statement gave. An expression has none.

### Editing a query result

`kerno-core/src/editable.rs` decides, once per result, whether it can be edited as rows of one table, and says why
when it cannot: `check` looks at the statement (one `SELECT` of one source) and the column origins, with no round
trip; `resolve` reads the answer of `Dialect::origin_sql`, which names the table and its kind (a table, not a
view); `decide` takes the table's structure and requires its row identity (the key the table data view uses) among
the result's columns. The app side is `crates/kerno/src/query/edit.rs`, shared by the table data tab and the
query tab: typing, NULL and default, paste, rows added, copied, deleted and reverted, undo, each tab applying the
pending changes on its own session through the same pending-edit SQL (`sql/edits.rs`) in one transaction. In a
manual-commit tab the apply joins the open transaction under a savepoint and commits nothing; a result is
edited only on the connection and database its rows were read on. `browse.rs` also re-runs a `SELECT` as a
derived table to filter, order and count it on the server (a statement that cannot be wrapped says why).

## The async bridge

`crates/kerno/src/bridge.rs` is the one place where GPUI and Tokio meet. Drivers, the SSH tunnel and child
processes need Tokio; the UI runs on GPUI's executors. One multi-threaded Tokio runtime with three worker
threads (`kerno-io`) is created at startup (`bridge::init`). `bridge::spawn` runs a future on it and returns
a GPUI `Task` the UI awaits; `bridge::spawn_blocking` does the same for blocking work (the SQLite store, the
OS secret store). Dropping the task does not stop the work. A panic on the runtime is re-raised in whoever
awaits the task. The UI thread never waits on I/O.

## Sessions and who owns them

`crates/kerno/src/connections/manager.rs`: the `Connections` entity holds the saved connections, their live
`Status` (`Disconnected`, `Connecting`, `WaitingForInput`, `Connected`, `Failed`) and the runtime of each open
connection: its `Connector` (with the SSH tunnel, if any), a navigator "meta" session
(`meta_session(id)`), and weak references to every session it handed out.

- `Drivers` maps an engine to the registered driver (`main.rs` builds it from `PostgresDriver` and
  `MysqlDriver`).
- `connect` asks for what a connection needs (database password, SSH password, key passphrase, trust in an
  unknown SSH host key) through `InputRequest`, answered with `answer`.
- `open_session(id, database)` opens another session on a connected connection. Each query tab, data tab,
  structure view and admin tab uses its own, so one tab's open transaction or running statement never blocks
  another. The tab that opened a session owns it and closes it when it goes away; disconnecting closes every
  session the connection handed out. A 30-second keep-alive checks open connections, and a lost one is
  re-established.

## The sidebar tree

One tree holds everything a user browses (D-042, D-043). `navigator/model.rs` is the tree as plain data (what is
loaded, open and let through by the filter; no UI, no I/O); `navigator/view.rs` draws it, asks for the loads the model
names, runs them on the bridge and gives the answers back; `navigator/actions.rs` is each node kind's menu;
`sidebar.rs` is the frame around it (header, empty state, the menus of the empty area, of folders and of
connections, and their actions). Folders and connections are the roots; opening a connection connects it and shows its
databases (the session's own first, marked default), schemas and objects. Each open connection has its own browse
state, and one generation counter that only grows drops answers that arrive late. Row ids are `f:{path}` for a folder,
`c:{id}` for a connection and `c:{id}␞{node}` for a node under it. A disconnect collapses the connection's row;
a failed connect keeps it open with the failure under it. Collapsing never disconnects. Folder looks (colour, glyph,
or a picture copied at 32 px into `folder-icons/` of the data directory by `folder_dialog.rs`, D-047) are kept in the
settings JSON, not in a table.

## Environments

`kerno-core/src/environment.rs` (D-044). An environment has an id, a name, a colour for each theme, a tint (none,
soft, strong) and rules: auto-commit by default, confirm every execution, confirm data edits, read-only by default,
close idle transactions after N seconds. The three built-ins (`local`, `staging`, `production`) are recognised by
id, can be edited and never deleted; the user adds more in Settings ▸ Environments (`environments_view.rs`). The list
is part of the settings (`settings.rs`) and a connection stores only the environment's id. A connection's own flags
win over its environment's defaults. Confirming data edits is always on: every apply shows its SQL whatever the
environment. `theme.rs::environment_chrome` turns an environment into the colours the window's chrome takes while
that connection is active (its tabs, the editor and grid frame, the status bar), chosen so text on them reads at
4.5:1 in both themes. Ported from DBeaver's connection types (`DBPConnectionType`, Apache-2.0; named in the file).

## Connection failures

`kerno-core/src/error.rs::explain(raw, config)` classifies a failure from its text, using SQLSTATEs, MySQL error numbers
and OS error numbers (which do not follow the user's language), and returns an `Explained`: a `FailureKind`, a headline
in plain words ("Host “studio” was not found."), the next step, and the raw message unchanged. The connection manager,
the connection dialog and the query tab show the headline and next step first and the raw text below, selectable.

## Settings, keymap and the command table

`crates/kerno/src/actions.rs` holds `COMMANDS`, one table of every command: id, palette label and group, default
shortcuts (`secondary` is Cmd on macOS and Ctrl elsewhere) and the key context they work in. The palette, the
menu bar, the key bindings and the keymap override file are all derived from it, so no command exists without a
palette entry, and tests check that every action is a command, ids are unique and every command is in a menu.
`keymap.json` in the config directory rebinds or unbinds a command by id; a bad file is reported at start and the
defaults stay. `settings.rs` stores the user's settings as one value in `kerno.db` and `settings_view.rs` is the
settings tab (appearance, fonts, editor, grid, confirmations, tool paths); each choice applies at once.

## Bench mode

`crates/kerno/src/bench.rs`, behind `KERNO_BENCH=seed|startup|first_rows|scroll`: measures startup, the first rows
of a large result, fetching and sorting it, and scrolling the grid, prints one JSON line per measure and quits. It
refuses to run without `KERNO_DATA_DIR` and `KERNO_SECRET_STORE=memory`, so it never touches real data or the
keychain; without the variable every hook costs one atomic load. `scripts/bench.sh` runs each mode five times.

## Secrets

`kerno-core/src/secrets.rs`. Passwords, SSH passwords and key passphrases are stored per connection
(`connection/<id>/password`, `ssh-password`, `ssh-key-passphrase`) through the `SecretStore` trait.
`OsSecretStore` uses the `keyring` crate (macOS Keychain, Linux Secret Service). When no store is available,
`MemorySecretStore` keeps them for the life of the process and the app asks again at the next connect. Whatever
store is chosen is wrapped in `CachedSecretStore`: each secret is read at most once per run (one lock is held across
the read, so two connects starting together prompt once; "no such secret" is remembered, an error is not). This is
what keeps the macOS keychain from asking for the login password at every connect (D-043).
`KERNO_SECRET_STORE=memory` forces the memory store (tests and checks of the real app). Secrets never reach
the SQLite file, the logs or the connection export; history text is redacted before it is stored.

## Storage and migrations

`kerno-core/src/storage.rs`. One SQLite file, `kerno.db`, in the platform data directory (`directories`
crate); logs and the accepted SSH host keys (`known_hosts`) live beside it, and `keymap.json` in the config
directory (`AppPaths`). `KERNO_DATA_DIR` puts everything under one directory, which tests and checks of the
real app use. It holds connections (without secrets), history, saved queries, open tabs and settings. The API
is synchronous; callers run it through `bridge::spawn_blocking`. Migrations are a versioned list of SQL
strings applied in order, with `PRAGMA user_version` recording how many ran; a database newer than the code
is refused. There are three migrations: 1 the initial schema (connections, history, saved queries, open tabs, settings),
2 the filters last applied to each table's data and query result, 3 saved tasks. Add a migration by appending to
`MIGRATIONS`, never editing an earlier entry.

## Client tools, local clients and saved tasks

`kerno-core/src/tools.rs` builds and runs the command lines of the engines' own tools (`pg_dump`, `pg_dumpall`,
`pg_restore`, `psql`, `mysqldump`, `mysql` and their MariaDB names). The password never reaches a command line: it goes
to the child through its environment, and a command's display only names the variable. A restored SQL file is a
script, so a file holding client commands (`\!`, `system`, `source` and the like) is refused unless the user allows
them. Extra arguments must be `--name=value` and anything that sets the server, user, TLS, option files or a
password is refused (D-046).

`detect_clients` lists every install on the computer (Homebrew `libpq` and `postgresql@N` Cellar versions,
Postgres.app, EDB, `mysql-client`, `mariadb`, `$PATH`), one entry per real folder with its version. `client_for`
picks the client of a connection: its own folder, else the family's default, else the newest found. A client folder
runs only if it is one of the detected installs or one the user added (Settings ▸ Local clients,
`local_clients_view.rs`, or the wizard's Browse…); any other stored folder is ignored and reported, and a client folder
never travels with a connection export, so an imported file cannot choose which programs run. A PostgreSQL client
older than the server blocks the wizard's Start; another MySQL or MariaDB version only warns.

`backup_dialog.rs` holds the step wizards (objects, settings, progress) for backup, global backup, restore and
execute script, drawn from DBeaver's task wizards. Each wizard can be saved as a task: `SavedTask` rows in the `tasks`
table (migration 3) hold a name, the tool, the connection id and the settings, and `tasks_view.rs` lists them (run
again, edit, duplicate, delete, last run status and log). A task keeps no client folder and no password; its
connection is looked up at each run with the rules it has then (D-049). There is no scheduler. The Tools submenu of
databases, schemas and tables and the foreign data wrappers dialog (`fdw_dialog.rs`, PostgreSQL) are built in
`navigator/actions.rs` and the sidebar.

## The update check

The one network request Kerno makes (CLAUDE.md §1, D-045, D-048). The rules are in `kerno-core/src/update.rs` (no
I/O): what counts as newer, the `curl` arguments, the checksum a download must have, what a release tarball may hold.
The steps that run commands are in `crates/kerno/src/update.rs`, behind a `Runner` trait; the real runner is built only
in `main.rs`, after the off switches, so tests never reach the network. The status bar item and "Check for
Updates…" are `workspace/updates.rs`.

- One request through the system `curl`, with fixed arguments (`--disable` first, so a `~/.curlrc` adds nothing;
  `--proto =https`, `--max-redirs 0`, `--head`, a `Kerno/<version>` user agent) to the repository's
  `releases/latest`. A 302 naming one of Kerno's own release tags with a plain `X.Y.Z` strictly newer than the
  running version is an offer. Every URL is built from constants and the parsed numbers, never from server text.
  No `curl`, no check.
- At launch and every 6 hours. Off: Settings ▸ Updates, `KERNO_NO_UPDATE_CHECK=1` (also for "Check for Updates…"),
  and bench mode installs no updater at all.
- Installing downloads `SHA256SUMS` and the artifact (size-capped, HTTPS only), verifies the SHA-256 and never runs
  an unverified file. On macOS it swaps the `.app` bundle by rename and keeps the old one for rollback; on Linux it
  replaces the tarball's files one by one, each by rename. A download that fails verification is deleted; where an install is not possible the release page opens instead.
  Tabs and settings are written and the disconnect question is asked first.

## The main toolbar and isolation levels

`workspace/toolbar.rs` draws DBeaver's quick-action bar under the title bar (D-050). Each item runs a command of
`COMMANDS` or opens a list, on the active tab's connection, else the one selected in the sidebar, and is greyed with
the menus' reasons when it does not apply. Tooltips take their shortcut from the live keymap. Items can be hidden
from its right-click menu and "Show Main Toolbar" hides the bar; when the window is narrow the pickers shrink first,
then whole groups move into a "»" menu.

Isolation levels belong to the session, not the server. `Dialect` supplies `isolation_levels`, `set_isolation_sql` and
`isolation_sql` (PostgreSQL `SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL`, MySQL
`SET SESSION TRANSACTION ISOLATION LEVEL`; MariaDB reads `@@tx_isolation`); a change is refused while a transaction is
open.

## Unsafe code

The workspace lint is `unsafe_code = "deny"`. The one exception is `crates/kerno/src/app_icon.rs`: setting the Dock
icon of the bare binary on macOS calls `NSApplication::setApplicationIconImage`, which objc2-app-kit marks `unsafe`;
the function expects the lint with its safety argument written beside it (D-042). Nothing else may use `unsafe`.

## Tests and checks

Integration tests of the drivers (`crates/kerno-postgres/tests`, `crates/kerno-mysql/tests`) and of the app run
against the `kerno-*` containers of `docker-compose.yml` (`scripts/test-env.sh up`). A test calls
`kerno_core::testing::available(engine)` first and returns with a message when the container is down;
`KERNO_REQUIRE_CONTAINERS=1` turns that into a failure. `testing::exclusive_run` takes a file lock so that one test
process at a time uses the servers. A test creates the objects it asserts on under its own names, through `Scratch`
(`ui_tests.rs`, `common::scratch_schema` in the drivers) which drops them when the test ends, and asserts only on
those: the rules are in `docs/plans/README.md`, "Reliability of the suite".

App tests run in a headless GPUI window (`crates/kerno/src/ui_tests.rs`, split by area into `ui_tests/*.rs`:
sidebar tree, menus, environments, toolbar, tool wizards, tasks, updates and so on) and read the app's state and
storage. They never build the real update runner.

There is no hosted CI and no GitHub Actions workflow. `scripts/check.sh` runs `cargo fmt --check`, clippy with
`-D warnings`, `cargo deny check` and the workspace tests, starting the containers only if they are down and
stopping them only if it started them; it refuses to start when the load average is above the core count.
`scripts/release.sh [version]` builds the macOS `.dmg` and the Linux packages locally, starts each Linux package in
clean containers (`scripts/check-linux-packages.sh`), writes `dist/SHA256SUMS` and prints the `gh release create`
command for the owner to run; it creates no tag and uploads nothing. `scripts/bundle-macos.sh` and
`scripts/bundle-linux.sh` build the packages; the Linux ones (tarball, `.deb`, AppImage for x86_64 and arm64) in the
Ubuntu 20.04 image of `packaging/linux/Dockerfile`, x86_64 cross-compiled (D-066).

## How to add an engine

This is the path for the engines in [ROADMAP.md](ROADMAP.md). The existing MySQL crate is the shortest model
for a second driver; the PostgreSQL crate is the model for an engine with schemas.

1. **Engine value.** Add a variant to `Engine` in `kerno-core/src/config.rs` and complete its methods
   (`label`, `default_port`, `default_user`, URL scheme). The compiler then lists every exhaustive `match`
   that needs the new arm: the core (`testing.rs`, `tools.rs`), the app (`connections/form.rs`,
   `connections/dialog.rs`, `structure/editor.rs`, `backup_dialog.rs`, `engine_logo.rs`).
2. **A crate** `crates/kerno-<engine>` with a driver type implementing `Driver`, and a session type
   implementing `Session` (connect with TLS and the tunnelled host, `execute` honouring the sink contract
   above, cancel from another task, introspection, `ddl`, completion catalog, admin queries). Put the
   engine's SQL in a `Dialect` implementation. Add the crate to `[workspace] members` and
   `[workspace.dependencies]`, and register the driver in `crates/kerno/src/main.rs` and `ui_tests.rs`
   with `Drivers::default().with(...)`.
3. **Tests.** Copy the integration tests of an existing driver into the new crate's `tests/` and adapt
   them: `connect.rs` (plain, TLS, tunnel, read-only), `execute.rs` (streaming, cancel, every type round
   trip), `introspect.rs`, `generate.rs` (statements run on the server), `edits.rs` (pending-edit SQL and
   transactional apply), `alter.rs`, `explain.rs` with plan fixtures, `maintenance.rs`, `tools.rs`,
   `transfer.rs`. The tests call `kerno_core::testing::available(...)` first, so they skip when the
   container is down. Add the engine's port and service name to `kerno-core/src/testing.rs`.
4. **Test container.** Add a service named `kerno-<engine>` to `docker-compose.yml` on a non-standard host
   port bound to 127.0.0.1, with a pinned image, a healthcheck and a seed script in `tests/fixtures/<engine>/`
   (every common type, enums where the engine has them, arrays, JSON, foreign keys, a view, a table without a
   primary key, and the generated 1M-row table). Make `scripts/test-env.sh` wait for it.
5. **Engine logo.** Put the vendor's unmodified SVG in `crates/kerno/assets/engines/<engine>.svg`, register it
   in `crates/kerno/src/engine_logo.rs` (path constant and the `include_bytes!` arm), and record its source URL
   and the vendor's trademark notice in `THIRD_PARTY_NOTICES.md`. Follow the vendor's usage guidelines and
   never use the logo as part of Kerno's own branding (CLAUDE.md §1).
6. **Notices and docs.** If logic is ported from DBeaver, say so in a comment naming the source file and list it
   in `THIRD_PARTY_NOTICES.md`. Add the engine's rows to [FEATURES.md](FEATURES.md) as it passes tests.
   Check `cargo deny check` for new dependency licences.
