# Architecture

Kerno is a Cargo workspace of four crates. The dependency direction is one way: the app depends on the
drivers and the core; the drivers depend on the core; the core depends on neither.

| Crate | Role |
| ----- | ---- |
| `crates/kerno-core` | Engine-neutral core, no UI dependency: the `Driver`, `Session` and `Dialect` traits, the value and result model, connection settings, SSH tunnel, secrets, app storage, SQL helpers (splitting, parameters, classification, completion, formatting; all on Kerno's own tolerant lexer, no SQL parser crate, D-010), table browsing and its filter (`browse.rs`), export and import, pending-edit SQL, table drafts and their DDL, the rule that decides whether a query result can be edited (`editable.rs`) and the figures of a selection of cells (`aggregate.rs`). |
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
`MemorySecretStore` keeps them for the life of the process and the app asks again at the next connect.
`KERNO_SECRET_STORE=memory` forces the memory store (tests and checks of the real app). Secrets never reach
the SQLite file, the logs or the connection export; history text is redacted before it is stored.

## Storage and migrations

`kerno-core/src/storage.rs`. One SQLite file, `kerno.db`, in the platform data directory (`directories`
crate); logs and the accepted SSH host keys (`known_hosts`) live beside it, and `keymap.json` in the config
directory (`AppPaths`). `KERNO_DATA_DIR` puts everything under one directory, which tests and checks of the
real app use. It holds connections (without secrets), history, saved queries, open tabs and settings. The API
is synchronous; callers run it through `bridge::spawn_blocking`. Migrations are a versioned list of SQL
strings applied in order, with `PRAGMA user_version` recording how many ran; a database newer than the code
is refused. Add a migration by appending to `MIGRATIONS`, never editing an earlier entry.

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
