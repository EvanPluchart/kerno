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

## D-010 — Statement splitting and analysis with Kerno's own lexer; no `sqlparser`

The spec names `sqlparser` for statement splitting and statement-at-cursor. Its tokenizer rejects text that
is still being typed (an unterminated string or comment fails the whole script), knows nothing of the
`mysql` client's `DELIMITER` command, and reports line/column rather than byte offsets. Splitting therefore
uses a small tolerant lexer in `kerno-core` (`sql::lex`, `sql::split`) that never fails. `sqlparser` was first kept for what needed a real parse of a complete statement
(classification, completion context).

Update, 2026-11-13: nothing ended up needing that parse. Classification, completion context, formatting,
the derived-table wrapping of a result's filter and the single-table rule for editable results are all
written on the same lexer, for the same reason (they must work on text that is not finished, in two
dialects). `sqlparser` had become a dependency no source file used, so it was removed from the manifests.
`CLAUDE.md` §2 still names it as the planned tool; this entry is the record of the difference.

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

**Resolved on 2026-11-13 (milestone 10, part 1).** RUSTSEC-2023-0071 is out of the dependency tree and
`deny.toml` ignores nothing. Every russh release up to 0.65.1 pins `rsa =0.10.0-rc.18` exactly, a release
candidate below the patched `>= 0.10.0`, so no upgrade and no `[patch]` fixes it. Kerno builds russh
without its `rsa` feature instead (`rsa`, `pkcs1` and `crypto-primes` leave `Cargo.lock`). Consequences:

- RSA private key **files** are refused by the built-in client before any login, in every format. Since
  J2 (D-056) they work through the OpenSSH client, and the built-in client's refusal says so: "the private
  key … is an RSA key, which the built-in SSH client cannot use: choose the OpenSSH client on the SSH tab, or
  load the key in your SSH agent". No `rsa` crate came back.
- RSA keys held by the SSH agent still work (the agent signs; covered by a test). Ed25519 and ECDSA key
  files are unaffected.
- An SSH server whose only host key is RSA cannot be reached: Kerno offers Ed25519 and ECDSA host keys and
  such a server fails with "No common Key algorithm". Not tested against a real RSA-only server.
- Restore RSA key files and host keys when a russh release depends on `rsa >= 0.10.0`. Both limits go in the
  README's known limits.

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

## D-024 — DBeaver parity: what is built and what goes to the roadmap

- Owner's decision of 2026-11-13: DBeaver Community is the primary reference for features and
  interactions. `docs/DBEAVER-PARITY.md` inventories its navigator, SQL editor, results, connection,
  object-editor and tool features and its context menus, as far as they apply to PostgreSQL and MySQL,
  from its source at commit `cc7e36f` (418 rows).
- Rows that Kerno lacks and that a daily DBeaver user would miss are gaps, built from
  `docs/plans/PARITY-GAPS.md` (parts G1 to G18) and `docs/plans/CTX.md`.
- 67 further gaps were moved to `docs/ROADMAP.md` on 2026-11-13, each with its reason (rarely used, off
  by default in DBeaver, covered another way in Kerno, or outside CLAUDE.md §4). Four roadmap rows rest
  on a reading of CLAUDE.md and are confirmed: specialised PostgreSQL folders (not in the §4.B tree),
  "search in web" (§1: nothing leaves the machine), editing roles and users (§4.H2 asks for a read
  view), replication tools (outside §4). The owner can pull any row back by naming its ID.

## D-025 — Query tabs, second part

- Statements are classified from their first tokens (`sql/classify.rs`) before they run. A read-only
  connection refuses what writes, `EXPLAIN ANALYZE` of a write included (it runs the write); what destroys
  (DROP, TRUNCATE, `ALTER … DROP`, DELETE or UPDATE without WHERE) is shown and confirmed; on a production
  connection every write is confirmed twice. The "confirmations" setting turns off the destructive question
  only, never the production one. Executable comments (`/*! … */`) count only on MySQL and MariaDB.
- EXPLAIN goes through `Dialect::explain` and `Dialect::parse_plan`, so each engine writes and reads its own
  form; the tree shows cost, rows, own and total time, and the slowest step; the raw output is a click away.
  The history records the statement the user wrote, not the `EXPLAIN` it was wrapped in.
- History and saved queries share one dialog (Cmd+Y): history per connection and searchable, saved queries
  with a name and a folder, a click reopens either in a tab on its connection. Passwords in statements are
  redacted before storage.
- `.sql` files: a tab is named after its file and marked only when its text differs (cursor moves do not
  count); a file already open is shown, not opened twice; file tabs come back after a restart.
- A PostgreSQL result abandoned inside an open transaction is left to finish: a cancel request would abort
  the whole block. Outside a transaction, abandoning cancels.
- The completion catalog is one `Session::completion_catalog` call per tab (four queries on each engine,
  not one structure query per table), loaded when the connection opens and again after a DDL statement. The
  completion code lives in the core and runs on the statement under the cursor only.
- The formatter never changes a token: the output is compared with the input after layout and the original
  is returned if they differ; `DELIMITER` sections and routine bodies are left as written. Checked on a
  corpus and on random token soup.
- `split_statements` is a single pass over the tokens (20,000 statements took 16 s, 22 ms now).

## D-026 — Table data view

- One grid delegate (`Grid`) serves results and table data; table data adds an optional edit layer. The row
  menu is attached to each cell, because the kit's table opens none from a cell in cell-selection mode.
- Sort and filter run on the server. `ORDER BY` always ends with the row identity (primary key, else a
  NOT NULL unique index), so pages never overlap. Values are written as literals by the dialect
  (`browse.rs`); a raw `WHERE` that holds `;` is refused.
- Pending changes (edits, deletions, new rows) live in the page and block moving to another page, a refresh
  or a new filter until applied or discarded.
- Apply order is deletes, updates, inserts, in one transaction; every statement must change exactly one row,
  otherwise everything rolls back and the pending changes stay. Rows are addressed by the values they were
  read with, so a row changed by someone else since is not written over.
- Statements on a tab's session run one after the other; a new load waits for the previous one, and only the
  user cancels. Views and tables without a key are read-only, and the tab says why.
- Counting a very large table: `Dialect::estimated_rows_sql` exists in the core; the tab still counts
  exactly (plans/M6.md Part 2).

## D-027 — Structure

- The form edits a `TableDraft`; `changes()` is a pure function of the stored structure and the draft that
  returns the ordered differences (drops of keys and indexes, then columns, then primary key, indexes and
  foreign keys, the table rename last). Each dialect turns them into statements; the preview is those
  statements.
- PostgreSQL writes one `ALTER TABLE` per change and applies them in one transaction. MySQL and MariaDB send
  at most two statements, the foreign-key drops and then one combined `ALTER TABLE` (the server refuses a
  key or auto-increment change alone); `CHANGE COLUMN` when a column is renamed and modified. MySQL DDL
  commits at once, so the confirmation says a failure leaves earlier statements applied.
- What the form does not show must not be lost by an edit: `ColumnInfo::on_update`, `INVISIBLE` and the
  identity kind (ALWAYS or BY DEFAULT) are carried by the draft and written back; a foreign key on its own
  table follows a rename of the column it references. Column order of an existing table cannot be edited.
- Editing a view, function or procedure opens its definition as a replace script in a query tab
  ("source mode"); Apply source shows the whole script and asks. PostgreSQL replaces in place (a materialized
  view by drop and create inside one transaction), MariaDB uses `CREATE OR REPLACE`, MySQL drops then
  creates a routine.
- PostgreSQL extensions come from `pg_available_extensions` and are installed from a searchable list; enums
  are created, extended and renamed from the Types folder. Both are PostgreSQL-only and absent on MySQL.

## D-028 — Export and import

- Export streams on a session of its own, so the tab stays usable. Rows go through a bounded channel to a
  writer thread (`export_file.rs`); the file is written beside its name and renamed when whole, and cancel or
  failure removes it. Measured in a debug build: about 78,000 rows/s with 18 MB resident for 1,000,000 rows.
  Encodings: UTF-8 with or without BOM, UTF-16LE, Latin-1 (characters outside it become `?`).
- A result is exported by running its statement again; a statement that writes is never re-run, so its
  Export button is absent.
- Import uses a CSV reader of our own (quotes, line breaks, CRLF, any delimiter, encodings), no new
  dependency. One transaction with a savepoint per batch; a bad record rolls everything back and names its
  line. On MySQL and MariaDB `CREATE TABLE` commits at once: a new table is created before the transaction
  and dropped if the import does not complete, and the confirmation says so.
- NULL rule: an unquoted field equal to the NULL text is NULL; a quoted one is always a value, so `""` stays
  the empty string. The preview and the confirmation follow the same rule as the import.

## D-029 — Backup tools and the restore safety rule

- Backup and restore run the engines' own client tools as child processes (`tools.rs`). The password goes
  only in the child's environment (`PGPASSWORD`, `MYSQL_PWD`); what is shown names the variable, and
  inherited `PG*` and `MYSQL*` variables are removed. Through an SSH tunnel the tool connects to a one-use
  local port. MySQL dumps are written with hex blobs.
- A restored SQL file is a script: `psql` and `mysql` run the client commands it holds (shell, files,
  reconnecting elsewhere). Clients that have a sandbox get it: MySQL `--commands=OFF` from 8.0.43, 8.4.6 and
  9.4; MariaDB `--sandbox --binary-mode` from 10.6.18 and the series listed in the module doc; every MySQL
  restore gets `--local-infile=0`. `psql` has no such option, so the file is scanned first, in constant
  memory and on purpose ignoring SQL quoting, and refused with its line and command unless
  `allow_client_commands` is set. The `pg_restore` custom format holds no script and is unchanged.
- The scan is version-aware for psql: before 8.4 a glued form like `\!cmd` runs, so it is refused whenever
  the version is unknown or older; for a psql known to be 8.4 or later, only exact command names are
  refused, which keeps regular expressions (`\s+`) in genuine dumps from false alarms.
- `\connect` in a dump (`pg_dump --create`) is refused by design: the password is in the child's environment
  and would go to whatever server the file names. A dump with carriage returns in its data needs the opt-in
  on a client without a sandbox (the scan cannot tell a `\r` command from a carriage return in a string). The dialog is not built yet (plans/M8.md Part 3).

## D-030 — Navigator and sidebar interactions, as DBeaver

- A click selects. The chevron, Right and Left expand and collapse. A double click or Enter runs the default
  action: table, view and materialized view open data (or structure or expand, by the `table_double_click`
  setting), routines open their source, other objects their DDL, folders toggle. On a connection a double
  click or Enter connects it when closed and shows its tree; it never disconnects.
- Keys are default bindings of named commands, bound per view through a key context ("Navigator",
  "ConnectionList"): F4, F2, Delete / Backspace, F5, Cmd+C, Cmd+Shift+C, Cmd+], Cmd+Shift+A. Each is in the
  palette and open to the keymap file, and unique within its context.
- Connection folders are paths joined by `/` in the connection's `group`, so existing stores, export and
  import carry them. Empty folders and the collapsed state are settings (`connection_folders`,
  `collapsed_folders`). Deleting a folder moves what it holds up one level after a confirmation.
- Sidebar menus are built by a pure function of the subject, the connection status and the folders; entries
  that do not apply are disabled with the reason in the label. Connections are dragged onto a folder or to
  the top level.
- Settings changes are written by one saver, in the order they were made; the last value wins, also on quit.

## D-031 — Packaging

- `scripts/bundle-macos.sh`: release build, `Kerno.app` ad-hoc signed and verified, a launch check on a
  scratch data directory, then a `.dmg` with `hdiutil`. `scripts/bundle-linux.sh`: a tarball with the binary,
  desktop entry, icons and licences; from macOS it runs in the image of `Dockerfile.linux-build`
  (Ubuntu 24.04), which builds the workspace and runs the core tests. CI builds through the same scripts.
- Verified on 2026-11-13: the `.app` signs, launches and quits; the `.dmg` verifies; the workspace builds and
  280 `kerno-core` tests pass in the container; the tarball is written; the Linux binary maps its window under
  Xvfb.
- Not verified: Linux rendering beyond the mapped window, Wayland, amd64, the edited CI file (never run, no
  remote). The scripts are to be run again with the icon at the end (plans/M10.md Part 8). An AppImage and an
  amd64 build from this Mac are on the roadmap. The image `kerno-linux-build` (5.6 GB) is removed at the end
  of the goal.

## D-032 — Brand colours in the theme

- The accent moves from teal to the brand (`assets/brand/BRAND.md`). The UI kit paints `primary` in more
  places than buttons (checkbox and radio fills, the tab marker, outline-primary labels, the selected engine
  card, the plan bars), so the two themes split it: the kit's own `button.primary.*` slots carry the button
  (Lime `#C8F03C` with Ink `#111111`, 14.4:1, in both themes), and `primary.*` is the accent that reads on
  the theme's surfaces: Olive `#4D6B00` with white text in light (6.1:1 on white), Lime with Ink in dark.
- Light: rings, caret, links, keywords and the selected-row marker are Olive (5.3:1 or more on every light
  surface); selected rows and soft accent backgrounds are Lime wash `#F1F8D6`; the sidebar's selected
  connection is a deeper wash `#E5F2BC` because the wash disappears on the grey sidebar (1.02:1); the editor
  and grid selection is lime at 70 % over white (`#D8F477`); hover rows are neutral grey `#F3F5F7`, so hover
  and selection differ in hue. Dark: lime at 5 % (hover), 8 % (accent), 12 % (selected row, sidebar
  selection) and 28 % (editor selection) over the surfaces, written as opaque hex so the contrast tests
  measure what is drawn; rings, links, keywords Lime (12.1:1 or more). Dark surfaces kept: the bars at
  `#0E1012` are already next to Night `#0E1116`.
- Input borders were 1.5:1; they now pass 3:1 (`#878F9B` light, `#646D78` dark), as the plan asks.
- Semantic colours unchanged: success, warning, danger, info, the connection tag colours (the `teal` tag key
  stays: it is stored in users' connections and is not the accent) and the pending-change tints. Success
  green sits about 75 degrees of hue from lime and olive; a test keeps it 60 or more.
- The brand files are embedded byte for byte under `brand/` and drawn with `img()` at their own colours:
  the lockup (reversed on dark, on a Night panel that continues its own background) in the About window at
  240 px, the mark (olive on light, lime on dark) in the empty tab area in an 80 px square that holds its
  clear space. The Linux window icon (`WindowOptions::icon`, X11 only) needs the `image` crate: left to the
  packaging part, the `.desktop` entry and hicolor icons cover it.

## D-033 — Commit mode per connection; production starts in manual commit

- A connection chooses the commit mode its query tabs start in. Left unset it follows the environment:
  manual on `production`, auto-commit elsewhere, as DBeaver's connection types do (`DBPConnectionType`:
  Production off, Development and Test on; `docs/plans/PARITY-GAPS.md` G13). Old stores and exports load
  unchanged. Commit `45b7470`; query tabs read it in `5f38692` ("Query tabs of a production connection
  start in manual commit").

## D-034 — Bootstrap statements are a small grammar, and read-only is applied again after them

- A bootstrap statement runs at every connect without being shown, also for an imported connection. First
  version (`45b7470`): refuse statements that write, change the schema or name a secret. Security review
  (`68033d6`): that could still run code (SELECT, CALL, DO, a function in a value), reach beyond the
  session, change who the session runs as, or switch off the read-only state a read-only connection relies
  on; so only one SET or RESET of a session setting with literal values is accepted, checked when the
  connection is saved, imported, exported, and again where the statement would run. A read-only connection
  re-applies its read-only state after the bootstrap.
- Second review (`d46bc23`): the server and the check did not read a statement the same way (MySQL runs
  `/*! … */`, `sql_mode` changes what a quote or a backslash means, PostgreSQL decodes `U&"…"`, `@@session.name`
  hid a refused name behind its scope). A list of refusals was replaced by what is accepted: `SET
  [SESSION|LOCAL] name (= | TO) value[, …]`, `SET TIME ZONE`, `SET NAMES`, `SET CHARACTER SET`, `SET @variable`
  on MySQL and `RESET name`, with no comment anywhere, a bare ASCII name and plain values; names that reach
  beyond the session or touch the transaction and its read-only state stay refused, compared part by part.
  This is narrower than DBeaver's bootstrap queries, which run anything.

## D-035 — One lock for the processes that talk to the test servers

- Test processes that talk to the `kerno-*` servers take one lock, so two `cargo test` runs started side
  by side no longer trip over each other's scratch objects (`45b7470`, `kerno-core/src/testing.rs`).

## D-036 — A connection closes every session it handed out

- Disconnecting closed only the connection's own session: a result grid still held its tab's session, which
  stayed open (inside a transaction, in manual commit) for as long as the rows were shown. The manager now
  closes every session it handed out; tabs and dialogs close theirs explicitly. A test counts a scratch
  role's sessions after each kind of owner goes away, on PostgreSQL and MySQL (`1aa6f04`,
  `every_owner_of_a_session_closes_it_when_it_goes_away`).

## D-037 — Editor text is the user's document; secrets are what Kerno is given to keep

- Passwords and passphrases of connections live only in the OS secret store; a test reads every file of
  the data directory, the exported connections and the log after a session and finds none
  (`no_secret_is_written_to_the_data_directory_or_the_log`). That test found one leak, fixed with it:
  bootstrap statements were logged with their text at debug level; the log now carries their number only.
- A password written as a literal in a statement (`ALTER ROLE … PASSWORD '…'`) is masked in the history
  and never logged, but stays in the unsaved text of its tab, which is stored so tabs come back after a
  restart (§4 C1). That text is the user's document, as a `.sql` file would be: masking it would change
  what they wrote, and not restoring tabs that hold such a statement would lose work without warning.

## D-038 — Maintenance tools run in their dialog, on a session of their own

- DBeaver runs them as tasks with their own log; the first plan ran them in a query tab. They run in the
  dialog instead: a tab's session may be inside a transaction, where `VACUUM` cannot run and where
  `TRUNCATE` would wait for a commit; the options, the confirmation, the cancel and the server's status
  rows or notices belong together; and a session opened on the object's database serves a database node
  other than the tab's. The session is closed when the statement ends, and closing the dialog cancels it.


## D-039 — A cancel is over when the server has acted on it, and covers a statement not yet sent

- PostgreSQL cancels through a second connection, and the server signals the session's backend from
  another process. tokio-postgres returns once the request is written, so the cancel of a result being let
  go could land on the session's next statement and stop that one (seen as tests that timed out one run in
  ten). The cancel now waits for the server to close the cancel connection, as libpq's `PQcancel` and
  pgJDBC do, at most 10 s. Cost: one more round trip on each cancel, and the session is held meanwhile.
- On both engines `Session::cancel` also stops a statement asked for and not yet sent (waiting for its turn
  on the session, or being described): the server, idle at that moment, would ignore the request and the
  statement would then run in full. Still ignored, as the protocols give no way to tell: a cancel that
  reaches the server in the instant between the statement being written and being read.
- The PostgreSQL table-sizes list leaves out a table with no size: it was dropped while the list was read,
  and `NULL` sorts first in a descending order.
- Tests assert only on objects of their own (`docs/plans/README.md`, "Reliability of the suite"); a failed
  test's cleanup reports what it could not drop instead of panicking a second time, which aborted the run.

## D-040 — Choices of the third visual pass (M10 Part 4)

- **Danger buttons are filled.** The kit's default danger button is a red label on a 20 % red tint, under
  AA in the dark theme. The theme file defines it: filled red with a white label in light (5.4:1), filled
  red with a dark-red label in dark (6.4:1). This holds for every danger button of the app.
- **The dark selected row is `#323B20`** (1.53:1 against a row; text on it 9.7:1, muted text 4.6:1). A test
  asks for at least 1.45:1 between a selected and an unselected row in dark. The light theme keeps the
  brand's Lime wash.
- **The session question has no primary button.** Rolling back and committing are both outline buttons:
  neither is the safe answer, and Enter confirms nothing.
- **The keyboard goes back to the active tab whenever nothing has the focus.** With no focused element the
  window dispatched keys from its root, above the workspace, and Cmd+, or any workspace shortcut did
  nothing.
- **A column is first drawn at the width of what it shows** (its name, its type, the longest value of the
  first page), up to 360 px, measured when the first page arrives and not per frame. A result read again
  (filtered or ordered on the server) keeps the widths it had, by column name.
- **Open Table lists scattered-letter matches only when no name contains the text.**
- **A click on a row number selects the row**, shift and click stretches the selection of rows.
- **A cancelled import is not a refused record**: the batch is not tried again row by row; the import ends
  cancelled and rolls back.

## D-041 — The matching bracket is Kerno's, drawn with the editor's range decorations

- gpui-kit 0.9.1's editor closes brackets but never marks the matching one (§4 C2). Kerno finds it with its
  own lexer (`sql::text::matching_bracket`): the bracket just before the cursor wins, else the one just
  after; depth counts the same kind only; a match never crosses a `;`; brackets in strings, quoted names
  and comments are text, except that a dollar-quoted body is code and is matched as a script of its own.
- Both brackets get a 1 px outline in the editor's text colour (no new token; the existing contrast test
  covers that colour on the editor and on the active line). The error mark stays on the editor's
  diagnostics: brackets there would count as errors and take their style.
- The scan is bounded to 8 KiB each way from the cursor, from a line start, so a long script costs what a
  short one does. Known limit: a block comment or dollar-quoted body that opens more than 8 KiB above the
  cursor and closes before it leaves the brackets after its end unmarked.
- Setting a decoration collection always notifies the editor in this kit version, even with nothing
  changed: Kerno compares before setting, or its editor observer would loop.
- The status bar's summary answers a right-click with "New connection…" or "Disconnect all" (as it says
  "Not connected" or "N connections open") and Copy; the messages count with "Show messages" and Copy.

## D-042 — Owner's test of 2026-11-13: how the ten items are built

- The ten items the owner added to CLAUDE.md §3 are planned in `docs/plans/M10.md` Part 9 and later, in
  the owner's order (1, 2, 5, 8, 9; then 6 and 7; then 3, 4, 10), before the final screenshots.
- **One tree.** `Navigator` becomes the sidebar's single tree (folders and connections as roots, one browse
  state per connection); `Sidebar` keeps its menus and actions and becomes the frame around it. Collapsing
  a connection never disconnects it.
- **One `unsafe` call.** The Dock icon of the bare binary on macOS needs
  `NSApplication::setApplicationIconImage`, which is `unsafe` in objc2-app-kit and has no safe route in
  GPUI. The workspace lint goes from `forbid` to `deny` and that one function expects it, with its safety
  argument written down. Nothing else may use `unsafe`.
- **Folders' looks live with the folders**, in the settings JSON (no SQL migration); a custom image is
  copied, downscaled to 32 px, into `folder-icons/` of the data directory.
- **Connection failures are classified from their text** by one pure function in `kerno-core`
  (`error::explain`): headline, next step, raw message. The markers are the SQLSTATEs, the MySQL error
  numbers and the OS error numbers, which do not follow the user's language.

## D-043 — Wave 1 of the owner's test: what was decided while building it

- **The tree.** Row ids are `f:{path}`, `c:{id}` and `c:{id}␞{node}`; each open connection has its own
  browse state, and one generation counter that only grows drops late answers. A disconnect collapses the
  connection's row; a failure keeps it open with the plain-words headline, the next step, the raw message
  and "Try again" under it. Revealing a table (Link with editor) opens folders and the connection's row but
  never connects. The list's menu is on the tree's root, so every empty area answers a right-click.
- **Default database.** The database the session is in comes first, open, with a "default" badge; the
  others follow closed, user databases before system ones.
- **Failure kinds.** `SshHostKeyUnknown` is its own kind; "the server asks for a password and none was
  given" is a login failure. In a query tab the next step is part of the message, because that view prints
  the detail before the hint. The raw message is selectable text.
- **Database list of the dialog.** Read only when the user presses "List…" beside the Database field:
  listing logs in with what the fields hold, and a first version that listed by itself 600 ms after the
  last keystroke could send the password to a host half typed (found by the security review of the
  commit). Changing host, port, user or password forgets the list. It ignores the database typed in the
  field, and on PostgreSQL goes through `postgres` when there is no database named after the user, as
  DBeaver does.
- **Secret cache.** Around the OS store only; one lock held across the read, so two connects starting
  together prompt once; "no such secret" is remembered, an error is not (a refused keychain prompt can be
  tried again). A secret added outside Kerno while it runs is seen at the next start.
- **F4 on a folder renames it** until "Edit folder…" exists (Part 9c), which then takes the key.

## D-044 — Environments: what the built-ins do, and what is always on

- **Production confirms every run from the editor, reads too**, as DBeaver's production connection type
  does (`confirmExecute`); writes are then asked a second time (CLAUDE.md §1). A read was not asked before:
  it is one rule of the built-in, editable in Settings → Environments. Re-reading a result (filter, order,
  auto-refresh) never asks.
- **"Confirm data edits" is always on** and shown locked: every apply already shows its SQL and asks
  (CLAUDE.md §1), whatever the environment.
- **Built-in is derived from the id** (`local`, `staging`, `production`), not stored: a hand-edited store
  cannot make production deletable or lose it.
- **An idle transaction is rolled back only after it wrote something**, as DBeaver's monitor does; one
  that only read is left alone. The user gets a message in the tab and a notification naming the rule.
- **Text on a tinted surface** is the theme's colour when it reads at 4.5:1, else black or white; the
  toolbar wash is lightened until muted text still reads. A property test covers arbitrary colours.

## D-045 — The update check uses the system's `curl`

- The owner allowed one network request (CLAUDE.md §1, §3 item 12): the releases page of
  `github.com/EvanPluchart/kerno`, at launch and every 6 hours, redirect not followed, nothing else sent,
  with an off switch. Kerno has no HTTP client, and adding one (and its TLS stack) for a single request is
  more code to audit than calling the `curl` every macOS and nearly every Linux already has, with fixed
  arguments. Recto and Vigie, whose rules this follows, also use the system's client. No `curl`: no check.
- The checksum from the release's `SHA256SUMS` catches a damaged or truncated download, not a compromised
  release; signing is the real answer and stays the owner's (it needs their identity).
- No GitHub Actions: the checks run locally (`scripts/check.sh`); a release is built locally
  (`scripts/release.sh`), and publishing it stays the owner's.

## D-046 — Tool wizards and local clients: what was decided while building them

- **Defaults.** PostgreSQL backups default to the custom format, as DBeaver. `mysqldump` defaults to
  `--single-transaction` with `--routines` and `--hex-blob` on (DBeaver leaves them off): a consistent
  dump that restores routines and binary data is the safer default. DBeaver's `--debug-info` log level is
  not offered: current `mysql` clients refuse it.
- **Three steps, not four**: objects, settings (with the output folder and the file name pattern), progress,
  as the owner's follow-up describes. An existing file is replaced only after a confirmation.
- **Extra arguments** must be written `--name=value`. Anything that sets the server, the port, the user,
  the database, TLS, option files, plugins or a password is refused, abbreviations and MySQL's stacked
  prefixes included, and so is anything that undoes a protection of the restore (`--set`, `--command`,
  `--output`, `--local-infile`, `--binary-mode`, `--sandbox`, `--pager`…). The list errs on the strict
  side: an abbreviation of a refused option refuses longer options sharing it.
- **Another user for the tool**: the name is kept with the settings, the password is used for that run
  only and never saved (so never in a task).
- **A local client folder runs only if Kerno detected it or the user added it**, and it never travels with
  an export: an imported file must not choose which binaries run (found by the security review while the
  work was in progress; the first plan said to export it). A stored folder that is neither is ignored, the
  wizard says so and uses the default.
- **A PostgreSQL client older than the server blocks Start** with the reason (`pg_dump` would refuse);
  another MySQL / MariaDB version only warns.
- The old `tool_paths` setting is turned into added folders once.

## D-047 — Folder looks and the app icon: what was decided while building them

- **A folder's picture is untrusted input.** Only `.png`, `.jpg`, `.jpeg`, `.svg`; at most 2 MB read
  whatever the file claims; decoding capped at 4096 px a side and 128 MB; done off the UI thread; a
  decoder that panics counts as an unreadable file. SVG goes through `resvg` with a resolver that never
  reads another file (GPUI's default would follow an `<image href>` to any path). The copy is a 32 px PNG
  named from a hash of its content, in `folder-icons/` of the data directory; the name is never taken from
  the picked path.
- **Unused pictures are removed** after each change, off the UI thread: only files named the way Kerno
  names them, and only those older than the change, so a picture just picked in an open dialog stays.
- **Looks follow their folders** through rename and move; when deleting a folder merges a child into an
  existing folder of the same name, the existing folder's look wins. Export carries colour and glyph, never
  the picture; import drops any picture name and fills only folders with no look yet.
- **F4 on a folder opens "Edit folder…"**; F2 stays Rename.
- **The Dock icon is set at every start**, inside `Kerno.app` too (setting it twice is harmless). The one
  `unsafe` call is in `crates/kerno/src/app_icon.rs`, as D-042 allowed; nothing else in the workspace uses
  `unsafe`. Linux: `app_id` / `WM_CLASS` `kerno`, `StartupWMClass` in the desktop entry, the X11 window icon.

## D-048 — Update check and install: what was decided while building it

- **The request.** `curl --disable --silent --head --max-time 10 --proto =https --max-redirs 0
  --user-agent Kerno/<version> -- <releases/latest>`; `--disable` comes first so a `~/.curlrc` cannot add
  headers, cookies or a proxy. Only a 302 to one of Kerno's own release tags naming a plain, strictly
  newer `X.Y.Z` is an offer; every URL is built from constants and the three parsed numbers, never from
  text the server sent. Downloads are capped (`--max-filesize`: 64 KiB for `SHA256SUMS`, 1 GiB for the
  artifact) and restricted to HTTPS through redirects.
- **Switches.** `KERNO_NO_UPDATE_CHECK` and `KERNO_BENCH` install no updater at all; the setting stops
  the timed checks; "Check for Updates…" still asks when the setting is off (the user clicked it) but not
  when the environment variable is set. Tests never build the real runner.
- **Linux replaces the tarball's files one by one** (each by rename, the old one kept aside for the
  rollback) instead of renaming the install folder: `bundle-linux.sh` installs into `~/.local`, and
  renaming that would move the user's whole `~/.local`. A tarball holding a link, an absolute path or `..`
  is refused before anything is extracted.
- **macOS swaps the bundle by rename** after copying the new one beside it from the read-only mounted
  image; any failure puts the old name back; the old bundle is deleted at the next start. An ad-hoc-signed
  update changes the signature, so the keychain asks again for saved passwords (README).
- **Before installing**, tabs and settings are written and the same question as disconnecting is asked;
  declining keeps the verified download for the next click. A changed table form is not kept, as with Quit.
- The setting lives in its own "Updates" section of the settings, with the line saying what is requested.
- Not verifiable here: the real request and a real install (no repository, nothing published).

## D-049 — Tools submenu, foreign data wrappers, saved tasks: what was decided while building them

- **"Save as task…" is a button of each wizard**, not a Tools entry: a menu entry has no settings to save.
  Tools has "Create new task…" (pick the tool, then its wizard, Save as the main button) and "Tasks".
- **A task stores no client folder and no secret.** Each run chooses the client through `client_for`;
  another user's password is typed again at each run; the kept log is the wizard's masked log (last 5,000
  lines) and goes through `redact_secrets` once more before it is stored.
- **A task's connection is looked up at each run**: its current read-only flag and environment rules
  apply, and a restore or a script asks with its command every time. Deleting a connection keeps its
  tasks, which then say they cannot run.
- **Foreign data wrappers are a dialog**: it lists wrappers, foreign servers and user mappings (never the
  mappings' options, which hold passwords) and creates a server, a mapping and an `IMPORT FOREIGN SCHEMA`
  through a form with the SQL shown (password masked) and confirmed. Listing them in the tree and
  DBeaver's table-by-table wizard are on the roadmap (NAV-10).
- **Only the four client-tool wizards become tasks**; maintenance and export tasks (TOOL-31) and a
  scheduler (TOOL-30: DBeaver Community has none) are on the roadmap.
- **The Tasks view reads its list on events** (opening, a task saved, run or deleted, Refresh), never
  while drawing: a first version reloaded at each frame and never came to rest.

## D-050 — Main toolbar: what was decided while building it

- **The toolbar acts on the active tab's connection**, else on the one selected in the sidebar: the same
  connection its pickers show. (The first plan said the selected tree row first.)
- **DBeaver's order**: Connect, Reconnect, Disconnect; the transaction log sits with the read-only toggle.
  Groups are separated by a gap, not a drawn rule (owner's item 1).
- **Tools is a list**, the sidebar's Tools entries for that connection; it has no command of its own. Two
  commands are new and have no key: Connect, Show Main Toolbar.
- **Isolation level.** PostgreSQL: `SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL …`, read
  with `SHOW transaction_isolation`; MySQL: `SET SESSION TRANSACTION ISOLATION LEVEL …`, read with
  `@@transaction_isolation`; MariaDB reads `@@tx_isolation`, which every version has. The level is the
  session's, never a server default; a change is refused while a transaction is open. A query tab reads
  the level when its session opens: one more round trip before the first run; a failure there only leaves
  the tick out.
- **Overflow.** The pickers shrink first (names cut, whole in the tooltip), then whole groups move from
  the right into a "»" menu. A script that no longer exists leaves the recent list when it is picked: the
  list is not checked against the disk when it opens.
- **Tooltips** take the shortcut from the live keymap through the kit's `tooltip_with_action`.

## D-051 — Fourth visual pass: what changed after seeing the new screens

- **A tinted bar in a dark theme keeps the hue and lowers the lightness until its light text reads at 7:1**
  (D-044 asked for 4.5:1: production was then a loud `#D11322` across the window). Production in dark is
  `#970D19` under `#E6E9ED` (7.22:1); staging `#4F4523` (7.81:1); light is unchanged (production `#C62F3D`
  with white, 5.42:1; staging `#D6C9AA` with `#16191D`, 10.74:1). The property test holds it for any colour.
- **Every item of a tinted status bar takes the bar's text colour**; a warning keeps its weight as its mark.
- **A confirmation asked by an environment's rule names the environment and the connection** in its title
  and carries the environment's band, for every-run confirmations and for the second click of a write.
- **Tools hides what the node's kind can never use** and greys, with the reason, only what the
  connection's state forbids (read-only, not connected).
- **A failed connection's rows keep one line each; "Details…" shows the whole message**, selectable, with
  Copy and "Edit connection…".
- **Restore and Execute script look for their client before a file is chosen**, and the file is a text
  field too: a typed path is read on Enter or when the field is left, never on each key.
- **The settings body has one size** (400 px) and scrolls inside.
- Driving the real app on this Mac: the keyboard layout is French, so a hotkey sent by position can be
  another shortcut (`cmd+a` arrives as Cmd+Q). The "app quit by itself" reports were that.

## D-052 — Machine-load rules lifted by the owner while away (2026-11-13, about 21:05)

- For Kerno only, and until the owner is back on the Mac: up to 4 subagents in parallel, cargo jobs and
  test threads at their defaults (all cores), `nice` optional. An earlier message of the same evening had
  allowed `CARGO_BUILD_JOBS=6` and `--test-threads=6`. `.cargo/config.toml`, the documented commands and
  `scripts/check.sh` keep the strict values: they are what a contributor's machine gets.
- Unchanged: only the `kerno-*` containers (never `studio-*` or `pim-*`), no push and nothing published,
  secrets in the OS store, one Kerno instance driven at a time, the disk rules.
- The benchmark still runs on a quiet machine (`scripts/bench.sh` refuses above a load of 4) with nothing
  else compiling, so its numbers are honest.

## D-053 — Idle memory after the owner's-test work, and what the bench measures

- Idle RSS rose from 138 MB to 170 MB during the evening. Measured causes: the Dock icon set by the app
  (owner's item 4) for about 24 MB (AppKit keeps the icon rendered at twice its size in points, and decoding
  it touches framework pages), the main toolbar about 1.3 MB, the larger binary about 1.7 MB. Nothing on
  the connect path.
- **The Dock icon is the 256 px drawing shown at 128 points** (the Dock's largest tile), and **it is not
  set inside an app bundle**, where the system already shows the bundle's icon. Idle RSS: 150.5 MB for the
  bare binary (150.4 to 151.0 over six runs), 148.5 MB for the same binary inside `Kerno.app`.
- **Not done:** a 16 MB region the system allocator keeps after the editor's highlighter compiled its
  query (tree-sitter's scratch, there since milestone 9) could be returned with
  `malloc_zone_pressure_relief`, a second `unsafe` call: the workspace keeps its single one.
- **The bench refuses to sample a hidden window** (macOS stops its display link: no frame, and the
  footprint reads 54 to 65 MB instead of 200 to 210) and waits for the editable decision before it sorts;
  both had ended runs on the 180 s watchdog. No app bug behind them.
- `docs/BENCH.md` gives both idle figures, the bare binary (what `scripts/bench.sh` runs) and the binary
  inside the bundle (what a user runs), and says which one the verdict is about.

## D-054 — A pending value can be SQL (M11 Part 3, J5)

- **`CellValue::Expression(String)`**, written into the statement exactly as typed (no quoting, no
  trimming); refused in a key and when empty (`edit_statements`, the shared root, so paste and copy are
  covered too). It goes through the usual review and confirmation; nothing runs unseen.
- **"Set default" writes the expression `DEFAULT`** on fetched and new cells, so it draws and edits like
  any expression. `CellValue::Default` stays as the "nothing given" marker of a new row's cells (left out
  of the `INSERT`).
- **⌘E while typing in a cell switches value / expression**: the cell editor catches `ExplainStatement`
  (⌘E's command) and stops it there, so a user's rebinding of Explain moves the toggle with it. The `=`
  button beside the input does the same. The flag lives in `Editor::Text`, so `grid_view.rs` (record view)
  gets the toggle without a change.
- **An enum cell has no expression toggle** (its editor is the list of values); "Set default" covers the
  common case there.
- The key-kind, JSON and expression tests on the three engines found no failure in the literal or
  identity code: nothing there was changed.

## D-055 — v1.1 (milestone 11): how the work is run, and two review notes (main session)

- **Fifteen parts in three waves** (`docs/plans/M11.md`), up to four agents in one working tree (D-052);
  commits are checkpoints of a tree that lints, so one commit can hold finished parts and the first files of
  parts still running.
- **One set of zoom commands.** Part 2 added Zoom In / Out / Reset beside the grid's older "Grid: Larger /
  Smaller Text"; both did the same thing after J7 (editor and grids together). The grid's three commands and
  their Alt+0 / Alt+9 keys are removed; ⌘= / ⌘- / ⌘0 work from anywhere.
- **Known limit of the OpenSSH askpass (D-056).** The destination's `ssh` runs with password and
  keyboard-interactive authentication off, so a server cannot ask a question that looks like the key's
  passphrase prompt. `-J` gives no way to pass those options to the jump host's own `ssh`: a jump host the
  user trusted and that turned hostile could send such a prompt and receive the passphrase of the key file
  (not the key). Same exposure as typing into a terminal `ssh`. Closing it needs a `ProxyCommand` built by
  Kerno instead of `-J`; noted for a later pass.

## D-056 — SSH through the system OpenSSH (M11 Part 1, J2)

- **One `ssh -N` per connection, forwarding a Unix socket** (`-L <dir>/db:<db host>:<db port>`) in a new
  0700 directory under the temp dir, not a TCP port and not `-W` per session: no port is open to other local
  processes (the built-in client's promise, `ssh.rs`), one SSH login serves every session, and readiness is
  the socket appearing (ssh binds it after authentication). Drivers keep their surface: `Tunnel` is an enum of
  the two clients inside (`open_stream` gives the socket, `forward_once` the single-use loopback port over
  it), `TunnelStream` too. `connector::open_tunnel` is the one place a tunnel is opened (connector and the
  tools' dialog). An OpenSSH tunnel leads to the connection's own database host and port only.
- **ssh never prompts on a terminal**: `SSH_ASKPASS` is Kerno itself, forced, for every `ssh` (a jump host's
  too; the app passes `current_exe()`, `main` calls `answer_askpass` first). The question comes back over a
  second socket in that directory, open only while `ssh` starts. Kerno answers the passphrase of the
  connection's own key file (from the secret store, so the manager's existing passphrase prompt is
  "enter it here"; once per asking `ssh`, which the helper names by its parent pid: a jump host's `ssh`
  asks for the same key on its own, and a second question from the same `ssh` means a wrong passphrase),
  says "yes" to a host-key question only when its fingerprint is the one the user trusted,
  and refuses anything else (passwords, other keys' passphrases). `BatchMode=no` because BatchMode stops ssh
  from asking for a passphrase; `PasswordAuthentication=no`, `ControlPath=none`, `ExitOnForwardFailure=yes`.
- **Host keys are OpenSSH's known_hosts**, checked with the real fingerprint: ssh's question carries it, the
  existing trust dialog shows it, and the retry answers "yes" to that fingerprint only (not
  `StrictHostKeyChecking=accept-new`, which would accept whatever key comes and cannot reach a jump host:
  `-J` passes no `-o` to the hop). The key is then written by ssh to the user's known_hosts, which Kerno's
  built-in client still only reads. A changed key fails before any question and is never offered.
- **What the fields mean with OpenSSH**: the host may be an alias; port 0 and an empty user are not passed
  (`-p`/`-l` would override the file); the key file is `-i … -o IdentitiesOnly=yes` and applies to the target
  only (a jump host takes its key from the file); the agent socket is `IdentityAgent`. Password login stays
  with the built-in client; a jump host needs OpenSSH. Host, user and jump hosts are restricted to letters,
  digits and `. - _ @ : [ ]` (and `,` between jump hosts) because ssh hands jump hosts to a shell
  (ProxyJump), checked in `validate` and again where `ssh` is spawned (a store edited by hand skips
  `validate`); the host comes after `--`.
- **Errors**: ssh's stderr is drained into a bounded tail and told in plain words by `error::explain`
  (`SshError::OpenSsh`): host not found, refused, timed out, login refused, unprotected key, unsafe config
  permissions, strict checking, forwarding prohibited, closed connection; a hop named in the Jump host field
  is called the jump host. A forward the server refuses shows only in ssh's messages after the driver failed:
  `Connector::session` turns it into `SshError::Forward`.
- **Detection**: `ssh` on `PATH`, then `/usr/bin`, `/opt/homebrew/bin`, `/usr/local/bin` (a GUI app's `PATH`
  is short). `KERNO_SSH_CONFIG` is passed as `-F`; otherwise ssh reads its own files (`-F` would drop
  `/etc/ssh/ssh_config`). The dialog lists the `Host` names of that file and its `Include`s (relative to the
  including file, `~` expanded, globs in the file name, depth 8, `Match` blocks skipped), read off the UI thread.
- **Tests** pass their own `-F` file (`testing::openssh`) with its own known_hosts, `IdentityAgent none` and
  `IdentitiesOnly yes`, so the developer's `~/.ssh` is never read; the core tests' askpass is the
  `kerno-askpass` bin of kerno-core, the drivers' and the app's `/usr/bin/false` (the test binary must never
  be ssh's askpass). `kerno-ssh-jump` (built from the `kerno-ssh` image, host port 52223) is the jump host;
  tests read each container's host key with `ssh-keyscan` and trust whatever each question shows, so the two
  containers need not share a key.

## D-057 — The editor of v1.1 (M11 Part 2, J1 and J7): what the kit allows, and the choices

- **Tab accepts a completion (J1, DBeaver #2316)** without forking the kit: gpui-kit 0.9.1's completion
  list answers only Enter, Escape, Up and Down, and Tab is the editor's `IndentInline`. The query tab's
  wrapper around the editor catches `IndentInline` in the capture phase while the list is open and the
  setting `tab_accepts_completion` is on (default), sends the kit's own `Enter` to the list
  (`route_overlay_action`), and stops the key only when the list took it; otherwise Tab indents as before.
  The list shows no "Tab" hint: the kit's list has no footer, and its only side area is the selected
  item's documentation, which would open a second box beside every list.
- **Occurrences** (`sql::occurrences`): the word at the cursor as a whole word, or a one-line selection
  anywhere; ASCII case ignored (SQL folds the case of names and keywords), strings and comments included,
  at most 1,000. Computed when the text or the selection changes, never per frame (the editor notifies at
  each cursor blink). The occurrence at the cursor is not highlighted (it is the cursor's). "Select All
  Occurrences" is on ⌘⇧L (VS Code's key: DBeaver's ⌥J types a character on a Mac) and uses the kit's
  `add_selection`.
- **Folding (#17240)**: the kit folds, from its gutter, every tree-sitter node over three lines or more
  (a statement, a subquery, a `do $$ … $$` body); a folded region still runs and saves in full (tested).
  The kit has no public call to fold a line (only `unfold_at` and `set_folding`), so Kerno offers
  "Unfold All" and no "Fold" / "Fold all"; ROADMAP SQL-44 keeps keyboard folding and folds on indentation
  or marker comments.
- **Zoom (#17817)**: one step moves the editor and the grid sizes together, each clamped to its range
  (`zoom_grid`, also behind the grid's own zoom keys, whose labels stay "Grid: …"). ⌘+ / ⌘- / ⌘0 are bound
  in the editor's context, beside the grid's DataTable keys, not globally; the grid's `alt-0` / `alt-9` stay
  local because Option with a digit types a character on a Mac. The View menu carries the new commands.
  ⌘-wheel is a capture-phase listener over the query and data tabs' area (tested over the editor and the
  result grid), one step per wheel notch or per 40 pixels of a trackpad.
- **Formatter options** (`FormatOptions`: keyword case, indent 1–8 spaces or a tab, comma at line end or
  start, line width): the default is the former layout byte for byte; `line_width` 0 (the default) puts
  each list item on its own line, otherwise a SELECT, SET or VALUES list whose clause fits stays on its
  keyword's line, decided from the tokens alone so formatting stays idempotent, and never for a list
  holding a comment, a CASE or a subquery. The token check after layout accepts keywords in the chosen
  case only. The property tests run over four option sets.
- **Format on save** (off by default): a `.sql` file's text is laid out in the editor first (one undo
  step, the cursor kept at its line and column, clamped), then written, so file and editor agree. A script
  that does not lay out cleanly (a statement the safety net keeps, or a string or comment left open at the
  end) is saved as it is.

## D-058 — The grid of v1.1 (M11 Part 4, J6): find, remembered layout, Unix time hint

- **What a find matches** (DBeaver #38133, Beekeeper #524): the text each cell draws (`format::display`,
  cut at the grid's 300 characters: text further in is not found), substring, ASCII case folded without an
  allocation, other letters lower-cased per cell (`ponytail:` in `kerno-core/src/find.rs`). NULL never
  matches; a timestamp's offset counts even when Settings hides it. Pending values count as shown.
- **Where it runs**: a count of matching cells per fetched row (two bytes a row) is made by
  `bridge::spawn_blocking` on a snapshot of the store, stopped when the text changes; rows that arrive later
  are counted on their own; results of up to 20,000 cells, and a table page with changes pending, are
  counted at once. Highlights are worked out for the cells on screen only. Measured: 0.40 s for 1,000,000
  rows × 20 columns in a release build (0.46 s case-sensitive, 1.13 s for a needle outside ASCII;
  `a_million_rows_are_searched`); the UI steps of the 1M-row UI test, frames included, take 24 to 44 ms in a
  debug build alone and up to 137 ms beside the rest of the suite, so the test checks where the work runs,
  not a time.
- **"Matching rows only"** is a row mapping in the order shown, made from the sort's own mapping. Pending
  changes are kept by row as fetched, so it is refused while changes are pending, and a grid shows no edit
  while it is on (`Grid::edit` and `Grid::editable` refuse, the menus say "matching rows only"). Reaching its
  last row asks the server for nothing; "Fetch all" in the bar does.
- **Keys**: ⌘F (`secondary-f`) while a grid has the focus; ⌘G / ⇧⌘G as `cmd-` keys in the bar's own context,
  because `ctrl-g` goes to a row on Linux; Enter / ⇧Enter in the bar; Escape closes and gives every row back.
  The bar stays open over a table page read again (its text and options carried to the new grid). A
  result read again on the server (a header sort while rows are still to come) is a new grid in the query
  tab, which this part does not touch: the bar closes there.
- **Layout of a table's data**: migration 4, `table_layouts(connection_id, table_key, layout_json,
  updated_at)`; the key is `[database, schema, table]` as JSON, the database left empty on MySQL/MariaDB (the
  schema is the database) and taken from the connection's default when the tab names none. By column name:
  a column renamed or dropped is ignored, one added is shown after the others, a sort on a column gone is
  dropped before the page is read. The first page waits for the stored layout (its order), and every page
  gets it (hidden columns used to come back on each page). Once a change is noted, the widths of every shown
  column are kept, and a column hidden or pinned from its menu leaves the others at their widths (the
  table used to give them their first widths back). Written 0.5 s after the last change, off the UI thread, by a detached task: closing the
  tab right after does not lose it. Query results are not remembered (no stable identity).
- **Unix time hint** (Beekeeper #941): an integer column's value from 2000-01-01 to 2100-01-01 in seconds,
  milliseconds or microseconds; Howard Hinnant's `civil_from_days` written out, no date crate; shown in the
  value viewer and in the value panel (a table's cells open the panel).
