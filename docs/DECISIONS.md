# Decisions

Choices made while building Kerno, and why. Newest last.

## D-001 — GPUI and gpui-component through `gpui-kit =0.9.1`

- The official `gpui` crate on crates.io stopped at 0.2.2 (October 2025). `gpui-component` (longbridge) no
  longer builds against it: since 0.6 it depends on `gpui-pre`, snapshots of Zed's GPUI crates republished
  by the gpui-component maintainer, pinned with `=` requirements.
- Upstream renamed its repository `longbridge/gpui-component` → `longbridge/gpui-kit` and now recommends a
  single facade crate, `gpui-kit`, that pins the matching set: `gpui-pre =0.3.12` (GPUI),
  `gpui-pre-platform =0.3.12`, `gpui-base 0.9.1`, `gpui-component 0.9.1`, `gpui-kit-assets 0.9.1`.
- Kerno depends on `gpui-kit = "=0.9.1"` with features `tree-sitter` and `tree-sitter-sql`. This is the spec's
  "gpui plus gpui-component, pinned to versions that work together", expressed the way upstream publishes it.
- Provenance checked on 2026-11-12 before pinning: crates.io owner of `gpui-component`, `gpui-pre`,
  `gpui-base`, `gpui-kit-assets` is `huacnlee` (the long-standing maintainer, 38 releases since 2025-02);
  `github.com/longbridge/gpui-component` redirects to `longbridge/gpui-kit`; tag `v0.9.1` matches.
- API reference: the pinned sources in `~/.cargo/registry`, and upstream's tag `v0.9.1`.
- Not used: the optional `gpui-fast` backend (a fork of GPUI by the same maintainer). The default backend is
  the upstream GPUI snapshot, which is what the spec names.

## D-002 — Metal shaders compiled at runtime

`xcrun metal` fails on the development Mac (Metal Toolchain component not installed). `gpui-kit` enables
`gpui-pre-platform`'s `runtime_shaders` feature itself, so no build-time Metal compiler is needed. Verified:
`cargo tree -e features` shows `gpui-pre-macos feature "runtime_shaders"`, and the hello window renders
(`docs/screenshots/m0-hello.png`). If the owner installs the toolchain
(`xcodebuild -downloadComponent MetalToolchain`), nothing changes: the feature is set upstream.

## D-003 — Rust pinned to 1.99.1 in `rust-toolchain.toml`

`gpui-pre 0.3.12` uses `std::hint::cold_path`, which the machine's default stable (1.93.1) rejects as
unstable. Rather than running `rustup update stable`, which would change the default toolchain of every
other project on the machine, the repository pins the current stable (1.99.1) in `rust-toolchain.toml`;
rustup installs it on first use, locally and in CI. Edition 2024, resolver 3.

## D-004 — Test environment

- `docker-compose.yml`: `kerno-postgres` (postgres:17, 55432), `kerno-mysql` (mysql:8.4, 53306),
  `kerno-mariadb` (mariadb:11, 53307), `kerno-ssh` (alpine + OpenSSH built from `tests/fixtures/ssh`, 52222).
  All ports bind to 127.0.0.1 only.
- TLS certificates and SSH keys are generated on the developer's machine by `scripts/test-env.sh up` into
  `tests/fixtures/generated/` (git-ignored). No private key is committed, even a test one.
- `kerno-postgres` is the TLS-enabled PostgreSQL: one server accepts plain connections, TLS connections
  (CA-signed server certificate with SANs `localhost`, `127.0.0.1`), and certificate authentication for the
  role `kerno_cert`. A second CA that signed nothing lets tests check that `verify-ca` rejects.
- MariaDB mounts the same seed files as MySQL; the fixture avoids engine-specific syntax (numbers come from
  a cross join of a digits table instead of `WITH RECURSIVE` limits or MariaDB's sequence engine).
- The seed is deterministic (no `random()`, no `now()` in data), so tests can assert exact values.
- Data lives in named volumes so a restart does not re-seed; `scripts/test-env.sh reset` deletes them.
  Tests must leave the seed as they found it (own schemas/tables, or transactions rolled back).
- Each seed ends by creating `kerno_seed_done`; the healthcheck reads it over TCP, so "healthy" means
  "seeded", not "process started".
- CI starts the same compose file on the Linux runner instead of GitHub `services:` blocks, because the seed
  scripts and generated certificates have to be mounted from the checkout. GitHub's macOS runners have no
  Docker: there the integration tests report themselves as skipped.

## D-005 — Reference clones

DBeaver Community and Beekeeper Studio are cloned shallow into `~/VM/refs/` (outside the repository, never
committed, never a submodule). The Beekeeper clone uses a sparse checkout that excludes every
`src-commercial/` directory, so the proprietary files are not on disk and cannot be opened or grepped by
accident. Both repositories carry their own agent instruction files; those are their projects' notes, not
instructions for work on Kerno.

## D-006 — `mysql` / `mysqldump` binaries (open)

The development Mac has `pg_dump`, `pg_restore` and `psql` (Homebrew) but no `mysql` or `mysqldump`.
Installing them is a system-level install, which CLAUDE.md §8 reserves for the owner. Kerno detects the
binaries, lets the user choose a path and says clearly when they are missing, so the feature itself does not
depend on this machine. How the MySQL backup path is exercised end to end here is decided in milestone 8.

## D-007 — Licence allow-list and advisories (`deny.toml`)

- Allowed: MIT, Apache-2.0 (also WITH LLVM-exception), BSD-2-Clause, BSD-3-Clause, ISC, Zlib, 0BSD,
  Unicode-3.0, CC0-1.0, and MPL-2.0. Each was added because a dependency in the graph needs it, after
  reading which crate and why. No GPL, LGPL or AGPL crate is allowed; `self_cell` is
  `Apache-2.0 OR GPL-2.0-only` and is taken under Apache-2.0.
- MPL-2.0 is file-level copyleft and compatible with shipping an MIT OR Apache-2.0 application as long as
  the MPL files are unmodified and their source stays available (it is, on crates.io). Today: `option-ext`
  (through `dirs`) and `cbindgen` (build-time only).
- Security advisories always fail `cargo deny check`. "Unmaintained" notices fail it only for Kerno's direct
  dependencies (`unmaintained = "workspace"`): the six reported on 2026-11-12 (`paste`, `instant`,
  `ttf-parser`, `rustybuzz`, `zvariant`, `zbus_names`) are transitive dependencies of GPUI and
  gpui-component.

## D-008 — SSH tunnels without a standing local port

The spec asks for "local port forward per connection". A listening loopback port is reachable by every
local process for as long as it is open, whoever owns it: it would let any local user reach the database
port behind the bastion. An automated security review of the first tunnel commit flagged exactly that, and
it was right. The tunnel is therefore built like this:

- PostgreSQL: no local port at all. `tokio-postgres` can run over any stream (`Config::connect_raw`), so the
  driver speaks directly into an SSH `direct-tcpip` channel (`Tunnel::open_stream`). Cancel requests open
  another channel the same way.
- MySQL/MariaDB: `mysql_async` can only dial a TCP address or a Unix socket, and its TLS upgrade is
  `unreachable!()` on a Unix socket, so a private Unix socket would silently rule out TLS through a tunnel.
  It gets a single-use loopback port (`Tunnel::forward_once`): opened for one dial, it admits exactly one
  connection and stops listening; it also stops after 10 s unused, and drops everything if two connections
  are waiting (the driver's cannot be told apart).
- Residual risk, stated in the code as well: a local process that connects in the instant between the port
  opening and the driver's own connect takes the slot. Kerno's connect is then refused, so this is noticed
  rather than silent, and the other process holds a raw connection to the database port, where it must
  still authenticate. Loopback TCP gives no portable, `unsafe`-free way to identify the peer process.

Host keys: checked against `~/.ssh/known_hosts` (read-only) and Kerno's own known-hosts file. Unknown host:
the connection fails with the fingerprint, the UI asks, and a retry carrying that fingerprint records the key
in Kerno's file only. Changed key: always refused, with the file and line of the recorded key.

## D-009 — Values travel as server text; PostgreSQL uses the simple query protocol

- Every result value is kept in the server's own text form (raw bytes for binary columns), packed in
  row-major arenas (`RowBatch`): one byte buffer and one `u32` offset per cell, a bitmap for NULL. No
  allocation per cell, NULL distinct from empty. Column metadata carries a type family (`TypeKind`) that
  drives rendering, sorting and literal quoting.
- Why text: it is exact for every type, including extension and user-defined types a client has no decoder
  for, and it is what `psql` shows. Decoding binary formats would mean one decoder per type and a failure
  mode for every unknown one.
- PostgreSQL therefore runs statements through the simple query protocol (`tokio-postgres`'s
  `simple_query_raw`), which returns text. That protocol's row description is exposed by `tokio-postgres`
  without type OIDs, so statements that can return rows are described first (`Parse`/`Describe` through
  `Client::prepare`) to learn the column types. A failed describe is not reported: executing the statement
  gives the authoritative error. Cost: one extra round trip for row-returning statements.
- Paging uses back-pressure rather than cursors: the driver stops reading the socket when the consumer has
  no demand, so the server blocks; no transaction is needed to keep a portal open. A suspended statement
  occupies its session, which is why each query tab owns a session.
- Transaction state: `tokio-postgres` does not expose the ReadyForQuery status byte. The session derives the
  state from each statement's leading keywords and outcome (any error inside a block aborts it; a failed
  COMMIT ends it). An integration test checks it against the server
  (`now() <> statement_timestamp()` is true exactly inside a block opened by an earlier statement).
- Parameters and edited values are written as SQL literals by the dialect, so the SQL shown for review is
  byte for byte the SQL that runs.
- Empty statements: the server's EmptyQueryResponse is reported by `tokio-postgres` as a command affecting
  0 rows; the driver cannot tell the two apart. The app never sends empty statements (the splitter drops
  them).

## D-010 — Statement splitting with Kerno's own lexer; `sqlparser` for analysis

The spec names `sqlparser` for statement splitting and statement-at-cursor. Its tokenizer rejects text that
is still being typed (an unterminated string or comment fails the whole script), knows nothing of the
`mysql` client's `DELIMITER` command, and reports line/column rather than byte offsets. Splitting therefore
uses a small tolerant lexer in `kerno-core` (`sql::lex`, `sql::split`) that never fails. `sqlparser` stays in
the stack for what needs a real parse of a complete statement (classification, completion context).

## D-011 — PostgreSQL introspection choices

- Reads `pg_catalog`, not `information_schema` (which hides objects the role has no privilege on and omits
  materialized views). Written for PostgreSQL 12+ (`prokind`, `attidentity`, `attgenerated`).
- Objects owned by an extension are not listed among a schema's objects, as in `pg_dump`: `pgcrypto`,
  `pg_trgm` and `hstore` alone would add about 150 functions to `public`. Extensions have their own list.
- `serial` is recognised by ownership (`pg_get_serial_sequence`), not by "the default starts with
  `nextval(`". The DDL round-trip test caught the name heuristic rewriting `orders.invoice_number` (an
  integer column defaulting to an independent sequence) as `bigserial`, which creates another sequence and
  adds NOT NULL. Both reference clients use the name heuristic.
- Row identity for editing: the primary key, else the first non-partial unique index whose columns are all
  real NOT NULL columns; otherwise the table is read-only in the data view.
- Administrative views (sessions, roles, grants, sizes, settings) are canned queries shown in the ordinary
  results grid, so sorting, copy and export come for free.

## D-012 — Kerno's own storage and what never goes into it

- One SQLite file (`rusqlite`, bundled) in the platform data directory holds saved connections, query
  history, saved queries, open tabs and settings; migrations are a versioned list in code, and a file
  written by a newer Kerno is refused rather than guessed at. `KERNO_DATA_DIR` overrides the directory:
  tests and checks of the real app use it so they never touch the owner's data.
- Secrets are not in that file by construction: `ConnectionConfig` has no secret field (a test walks its
  serialized keys), and passwords live in the OS secret store through `keyring` (`OsSecretStore`). Without a
  usable secret store, `MemorySecretStore` keeps them for the life of the process and the app asks at
  connect. `KERNO_SECRET_STORE=memory` forces that, and the one test that writes to the real keychain is
  opt-in (`KERNO_TEST_OS_SECRET_STORE=1`), so an automated run never pops a keychain prompt.
- History text is redacted before it is stored (`redact_secrets`). After an automated security review of the
  first version, the rule set covers: the string after any word ending in `PASSWORD` (`MASTER_PASSWORD`,
  `SOURCE_PASSWORD`, `OPTIONS (password '...')`), `IDENTIFIED BY`, `IDENTIFIED WITH plugin BY|AS`, the
  current password after `REPLACE`, and, inside any literal, `password=value` pairs (libpq connection
  strings, `sslpassword`) and the password of `scheme://user:password@host` URLs. A unit test had already
  caught the `IDENTIFIED WITH plugin BY` form slipping through the first rule.
- When a statement contained a secret, its server error message is not kept: MySQL syntax errors quote
  the statement from the error point and can cut a literal in two, so the message cannot be cleaned
  reliably. The status and timing are kept.
- Limits, stated plainly: redaction recognises statements that set or carry a password. A secret used as
  ordinary data (`INSERT INTO api_keys VALUES ('...')`) is query text like any other and is stored. That is
  why the store file is created with mode 0600 in a 0700 directory (its `-wal`/`-shm` siblings inherit
  the mode), and tightened again at every start.

## D-013 — Accepted advisory: RUSTSEC-2023-0071 in `rsa` (temporary)

- `cargo deny check` reports a timing side channel in the `rsa` crate ("Marvin attack"). The fixed version,
  `rsa 0.10.0`, was published on 2026-11-08. `russh 0.65.1`, the newest release, requires
  `rsa =0.10.0-rc.18` exactly, so Kerno cannot move to the fix until russh does.
- Where the affected code runs in Kerno: only when a tunnel authenticates with an RSA private key *file*.
  That is one signature per tunnel the user opens. Keys held by the SSH agent are signed by the agent,
  not by this code; Ed25519 and ECDSA keys do not use it; verifying an RSA host key is a public-key
  operation and is not affected.
- Choice: keep RSA key files working (the spec requires key-file authentication, and `id_rsa` is still
  common) and list the advisory under `[advisories] ignore` in `deny.toml` with this reason. The alternatives
  were dropping russh's `rsa` feature (which also removes RSA host-key verification) or refusing RSA key
  files in Kerno.
- This is a security trade-off made on the owner's behalf: it is listed in `docs/PLAN.md` under open points.
  Remove the ignore entry when a russh release depends on `rsa >= 0.10.0`.

**Owner's decision, 2026-11-12:** the ignore is acceptable for now, and must be gone at the end: GOAL
criterion 1 requires `cargo deny check` clean with no ignored advisory. Planned for milestone 10 (upgrade
russh to a version on the fixed `rsa`, or replace the affected dependency); see `docs/PLAN.md`.

## D-014 — App shell

- Startup: Kerno's own files (store, settings, saved tabs) are read on a separate thread while the platform
  initialises, and joined before the window opens, so the first frame has the right theme and tabs and the
  UI thread never reads the disk. After startup, storage and secret-store calls go through
  `bridge::spawn_blocking`.
- One Tokio runtime with three workers (`bridge.rs`) is the only place GPUI and Tokio meet.
- Commands live in one table (`actions::COMMANDS`): the palette, the default key bindings and the user's
  keymap file are derived from it. The keymap file maps command ids to shortcuts (a string, a list, or `null`);
  overrides replace a command's defaults; unknown ids, unknown key names (GPUI's parser accepts any word as a
  key) and shortcuts bound twice are reported in a notification at startup instead of being applied silently.
- Theme: both modes are defined as tokens in `assets/themes/kerno.json`; a unit test computes WCAG contrast
  for 42 text/surface pairs, the editor and every syntax colour (4.5:1), and tag colours and focus ring (3:1).
  It caught the light-mode warning colour on the status bar at 4.43:1 before any screenshot did.
- Tabs: the central area uses gpui-component's `TabBar` and `resizable` panels rather than its dock, which
  brings split panes and drag-to-dock that v1 does not need. Open tabs, their unsaved text and the active
  tab are stored 400 ms after the last change and at quit.
- `scripts/capture.sh` runs the real app on a scratch data directory with the in-memory secret store and
  captures its window; `KERNO_APPEARANCE` pins the theme for that run only.

## D-015 — MySQL / MariaDB driver

Written by a delegated agent from the PostgreSQL driver as a template, then reviewed: scope checked (only
`crates/kerno-mysql`), tests re-run independently with the containers required, security-sensitive parts read.

- One driver for both engines (`mysql_async`, text protocol, so values arrive as server text; binary
  columns as raw bytes). `Session::engine()` reports what the server says it is (`VERSION()`), not what the
  connection settings claim.
- Cancel is `KILL QUERY <connection id>` on a side connection, the only way the protocol offers. Through
  an SSH tunnel every new connection, the side connection included, gets its own single-use port (D-008).
- `UPDATE` reports rows matched, not rows changed (`client_found_rows`), because applying pending edits
  requires exactly one row per statement and an edit that sets a value to itself must still count.
- Transaction state comes from the server's status flags. There is no "failed transaction" state on MySQL.
- String literals: a quote is doubled. Text holding a backslash or a NUL is written as
  `_utf8mb4 X'<hex>'`. The agent's version followed the session's `sql_mode` (`NO_BACKSLASH_ESCAPES`) by
  re-reading it after statements that start with `SET`; a mode changed any other way (a prepared
  `EXECUTE`, an executable comment) would have left the driver escaping for the wrong mode, and in one
  direction a value such as `\'` then ends its own literal. The hexadecimal form reads the same in both
  modes, so no tracking is needed for safety. A test changes the mode behind the driver's back on both
  engines and checks nine awkward strings are stored exactly. Column defaults go through the same function,
  so a default holding a backslash is shown and re-created in that form (both servers accept it).
- Presentation: `TINYINT(1)` is a boolean with cells `0`/`1`; `BIT` cells are digit strings and written
  back as `b'…'`; `TIMESTAMP` has no zone in its text and is a plain date-time; geometry is binary.
- Catalog differences between the two engines (default quoting, check constraints, JSON as `LONGTEXT` on
  MariaDB, sequences, functional index parts, process list and settings views) are handled per engine; the
  version gates for servers older than the two tested (8.4.13, 11.8.9) come from documentation and are
  not verified.

Known limits, tracked in `docs/PLAN.md`:

- (Resolved in D-018.) `verify-ca` used to behave like `verify-full` on MySQL/MariaDB.
- In a request with several result sets, a result's completion is delivered when the next one starts.
- Warnings are fetched for the last statement of a request only.
- `LOAD DATA LOCAL INFILE` is refused.

## D-016 — Statement splitting follows the `mysql` client for custom delimiters

The agent found that `END$$` under `DELIMITER $$` was not split: `$` is an identifier character, so the lexer
saw one word. The splitter now looks for the current delimiter as plain text at every position outside
strings, quoted identifiers and comments, which is what the `mysql` client does (it will also split
`select 2go` under `DELIMITER go`). A failing test was written first.

## D-017 — Connections: one manager entity, prompts instead of failures

- `Connections` (app crate) is the one owner of the saved list and of each connection's live state: status,
  the tunnel and sessions, the secrets of this run, keep-alive. The sidebar, the dialog and the workspace
  observe it; none of them talks to a driver.
- A connection attempt that lacks something asks for it instead of failing: a password that is not saved,
  an SSH password, the passphrase of an encrypted key (asked again when wrong), an SSH host key seen for the
  first time (fingerprint shown, trusted only on an explicit answer, then written to Kerno's own
  known-hosts file; the user's `~/.ssh/known_hosts` is read, never written).
- "The server wants a password and none was given" is a typed error (`Error::PasswordRequired`) raised by
  both drivers, so a connection whose password is not in the secret store (imported from a file, or the
  entry was removed) prompts too. Asking up front would be wrong: `trust`/socket/certificate logins have no
  password.
- A secret typed at a prompt is kept in memory for the run. It is written to the secret store only when the
  connection says "save password" and the attempt succeeded. A typed secret that fails is forgotten, so the
  next attempt asks again rather than replaying it.
- Auto-reconnect: a 30 s keep-alive ping on the navigator session; a failed ping drops the sessions and
  connects again once, with the secrets already in memory.
- Export writes the settings only. Secret-named driver options are refused at validation (D-012), so an
  export cannot carry one. Imported connections get new ids and no secrets.
- The dialog does not close on Enter (a half-filled form must not be saved by accident); Enter in the URL
  field fills the form from the URL. `Cmd+N` opens it.
- Engine tiles show a two-letter monogram in the UI typeface, not a vendor logo (CLAUDE.md §1).

## D-018 — `verify-ca` on MySQL / MariaDB

`mysql_async` 0.37.1 implements "skip the host-name check" by matching the text of a rustls error; rustls
0.23.46 words that error differently, so the switch never fires and a name mismatch is always refused. The
library does not accept a custom certificate verifier.

Kerno's driver therefore handles `verify-ca` itself: when the handshake fails only because the host name
is not in the certificate (rustls' structured `NotValidForNameContext`), it dials once more under one of the
names the certificate presents. That second handshake verifies the chain against the configured CA in full,
as before, and the name check then passes: the result is exactly "chain verified, name not compared".
Anything the driver cannot read as a plain DNS name or IP address is not guessed at and the connection stays
refused. The names come from rustls' debug form (`DnsName("…")`, `IpAddress(…)`); if a future rustls changes
it, the unit test of `usable_name` and the integration test through the bastion (server reached under a name
its certificate does not carry) fail, and the behaviour falls back to refusing.

The MySQL and MariaDB test containers now present the certificate signed by the test CA, and have a
`kerno_cert` account that requires the client certificate. Covered on both engines: `verify-ca` and
`verify-full` positive paths, wrong CA, system store, name mismatch for both modes, client certificate with
and without its key.

## D-019 — Engine logos (owner request of 2026-11-12)

CLAUDE.md §1 now asks for each engine's real logo wherever an engine is named. Done for the two places that
exist today (engine picker, sidebar connections) through one function, `engine_logo`, which the navigator
root, tab badges and status bar will call when they exist.

- Files: `crates/kerno/assets/engines/` (the app's asset directory, next to the fonts and the theme),
  unmodified, with source URLs, checksums and trademark notices in `THIRD_PARTY_NOTICES.md`. A test checks
  each engine has its file and that the UI kit's own icons are still served by the combined asset source.
- PostgreSQL: the official three-colour elephant from the PostgreSQL wiki, in full colour. Its white
  outline keeps it readable on dark surfaces, so no second variant is needed.
- MySQL: the official logo in its colours (dolphin and wordmark), as served by mysql.com
  (`/common/logos/mysql-logo.svg`, the artwork of the logo page), drawn like the PostgreSQL one, in both
  themes.
- MariaDB: the single-colour sea lion from Simple Icons (a source the spec names), drawn in one flat
  colour: the theme's text colour, so dark on light and light on dark. The vendor publishes black and
  reversed variants of its mark, which is what this reproduces; no brand colour is invented. MariaDB's own
  SVG on mariadb.org is the horizontal lock-up with the wordmark, which does not fit a 16 px square.
- No padding is drawn into the logos; callers leave clear space (4 px in the picker tile).

**MySQL: decided by the owner on 2026-11-12.** Oracle's logo page says: "The original MySQL logo may only
be used by MySQL representatives and companies having a written agreement with Oracle", and that a written
agreement is needed before "displaying any MySQL logos except the Conditional Use Logos" or displaying them
"elsewhere except on a website". The first version of this change therefore shipped the Simple Icons mark
and asked the owner. The owner's answer: keep the MySQL logo and use the normal official artwork in full
colour, as DBeaver, Beekeeper Studio and TablePlus do in their pickers; the owner accepts the risk, knowing
Oracle's policy. If Oracle ever asks for it to be removed, the fallback is to swap that one file
(`crates/kerno/assets/engines/mysql.svg`) or to show the engine's name alone, a one-line change in
`engine_logo`. PostgreSQL's and MariaDB's policies raise no such question for this use.

## D-020 — Schema navigator

- The tree is plain data (`navigator/model.rs`, no UI types): nodes, what is loaded, what is open, the
  filter. The view asks it what to fetch (`Load`), runs that on a session and hands the answer back. The
  widget is gpui-component's virtualized `Tree`; the model is converted to its items after every change.
- Loading is lazy at three levels: the schemas of a database, the objects of a schema (one catalog pass
  gives tables, views, routines, sequences, types and triggers with their counts), the columns and indexes
  of a table. A node that is not loaded yet shows a "Loading…" line, an empty one "Nothing here", a failed
  one the server's message, and opening it again retries.
- PostgreSQL: one session per database, opened when the database is first opened in the tree (the
  connection's own session serves its own database). MySQL and MariaDB: the connection's session sees every
  database, so no other session is opened. The sessions are dropped when the connection closes or the
  tree is reloaded.
- Refresh reloads the nearest node the server answers for (a table inside "Tables" is refreshed with its
  schema) and keeps the folders below it open. Tables that were open close: their columns would have to be
  fetched again.
- The filter matches names of objects, not of folders; a match is shown with the way to it, opened, and a
  matching schema or table keeps everything under it.
- Partitions are not listed among the tables (they are reached through their parent, in the structure
  view of milestone 7). Empty folders are shown, dimmed: they say there is nothing, and carry "Create…".
- Statements (D-021) run only after `sql_confirm`: a dialog that shows the exact SQL, never confirms on
  Enter, and on a `production` connection needs two clicks on the action. The same dialog will confirm
  pending edits and ALTERs in later milestones. On a `read_only` connection the entries that write are
  disabled and say why; so is anything the engine cannot do (renaming a MySQL trigger) and dropping the
  database the connection is in on PostgreSQL.
- "Generate SELECT/INSERT/UPDATE/DELETE", "Show DDL", "New query here", "Create table…" and "Create view…"
  open a query tab and run nothing. Until query tabs are bound to a connection (milestone 5), "New query
  here" starts the tab with `SET search_path` / `USE`; "Create table…" writes a statement to complete,
  and becomes the form editor in milestone 7. "Open data" and "Open structure" arrive with those views
  (milestones 6 and 7).
- Glyphs: sixteen Lucide icons embedded with the kit's `icon_assets!` (the kit bundles only the icons its
  own components use). Engine logo at the root of the tree (D-019).

## D-021 — SQL written for the navigator

Written by a delegated agent to a brief (signatures, rules, tests first), then checked: scope confined to
the dialect and `sql/generate.rs`, tests re-run here with the containers required.

- Rename, drop, truncate, create schema and create database are methods of `Dialect` with standard-SQL
  defaults; each driver overrides what its engine does differently. A new engine starts from the defaults.
- No `CASCADE`, no `IF EXISTS`: the user acts on something they see, and an error is information.
- Every identifier and every new name is quoted by the dialect. On MySQL and MariaDB the new name of a
  table, view, sequence or event is qualified with its database, because a bare name would move the object
  to the session's default database.
- What an engine cannot do is `Error::Unsupported` with a sentence saying what to do instead (MySQL cannot
  rename a routine, a trigger or a database).
- Templates use `:column` placeholders, which Kerno prompts for before running. `UPDATE` and `DELETE` are
  never written without a `WHERE`: a table without a primary or unique key gets `WHERE -- add a condition`,
  which the server refuses as it stands. An all-key table gets `:new_<column>` parameters for `SET`.
  `INSERT` leaves out generated columns and keeps identity-by-default and auto-increment ones, so the
  prompt shows them; deleting a line lets the server number the row.
- Integration tests run every statement against PostgreSQL, MySQL and MariaDB in scratch schemas.

## D-022 — Query tabs: one session per tab, results by pages

- A query tab is bound to a connection, a database and (PostgreSQL) a schema, chosen in its toolbar and
  stored with the tab. It opens its own session at the first run; the navigator's session is never used for
  the user's statements, so a long query does not block the tree. A new tab takes the connection being
  browsed; a tab opened from the tree takes the node's database and schema.
- Cmd+Enter runs the selection or, without one, the statement the cursor is in; Cmd+Shift+Enter the
  selection or the whole text, one result per statement, stopping at the first failure. Statements are
  split by the core splitter with the engine's lexical rules.
- Rows arrive by pages (the "grid page size" setting, 1,000 by default) through the core's back-pressure:
  the statement stays open on the server and the tab is not "running" while it waits to be asked for more.
  The grid asks for the next page when scrolled near its end; "Fetch all" lifts the limit. Running
  something else in the tab, or "Stop", lets the pending statement go.
  In a script, a statement with more rows than a page stops at its first page, because the next statement
  needs the session; the footer says the result is incomplete.
- Cancel sends the engine's cancel request on the side. Whatever the server then answers (an error on
  PostgreSQL, sometimes a normal end on MySQL), the tab knows it asked and shows "Cancelled"; rows that had
  arrived stay.
- Parameters (`:name`, `$1`, `?`) are asked for before anything runs, in one prompt for the run; values
  are written into the statement as literals by the dialect (a number or boolean bare, anything else a
  quoted string, or NULL). What was typed is offered again next time, for the life of the tab.
- Transaction mode per tab: auto-commit (the default), or manual, where the tab opens a transaction with
  the first statement and shows Commit and Rollback while one is open. The state shown is the server's
  (`TxState`), so a `BEGIN` typed by hand shows the same buttons.
- Server messages (NOTICE, WARNING) of a run are kept apart from results, in a "Messages" tab.
- A failed statement is underlined in the editor at the position the server gave, when the statement ran
  as written (not when parameters were written into it: positions would be off).
- Every statement is recorded in the history when it ends (text redacted by the storage layer, duration,
  status, row count, error).

## D-023 — Results grid

- The grid is gpui-component's virtualized `DataTable` with a delegate over the core `ResultStore`: only
  visible rows and columns are laid out, and cells are formatted when drawn (`kerno_core::format::display`
  borrows the server's text when it can).
- `ResultStore` holds its batches behind `Arc`, so a clone is a snapshot. A result over 20,000 rows is
  sorted on a background thread from such a snapshot; smaller ones at once. Sorting is offered when rows
  have stopped arriving, and is done on what was fetched (the footer says when that is not everything).
- Written here because the kit's table falls short of the spec (CLAUDE.md §2):
  - range selection: the table selects one cell, row or column; shift-click stretches a rectangle from it,
    kept in the delegate and drawn by it; Cmd+A selects everything;
  - row numbers: a pinned first column of the delegate;
  - hide and pin columns: the delegate keeps its own layout (order, hidden, pinned first).
- Copy: a single cell is copied as its value; anything larger as TSV (what spreadsheets paste), with
  the other formats in the row menu: TSV with header, CSV, JSON, Markdown, SQL INSERT. They are written by
  the core exporter, the same code that will write files in milestone 8. `INSERT`s name a placeholder table,
  since a statement does not say which table its rows would go back to.
- The value viewer (double click) shows a cell in full as text, indented JSON or a hex dump.
- Formatting, sorting and export (`format.rs`, `sort.rs`, `export.rs`), statement classification and
  EXPLAIN parsing (`sql/classify.rs`, `plan.rs`) were written by two delegated agents to written briefs and
  checked: scope, tests re-run with the containers required, lint. The last two are wired into the app in the
  next step of this milestone.

