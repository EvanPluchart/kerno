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
  connects again once, with the secrets already in memory. Superseded for v1.1 by D-062 (Part 5b: a 5 s check, attempts of 10 s retried until the server answers).
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
  table follows a rename of the column it references. Column order of an existing table cannot be edited
  (since M11, on MySQL and MariaDB it can: D-062).
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
- The Linux half is replaced by D-066 (Ubuntu 20.04 image of `packaging/linux/Dockerfile`, x86_64
  cross-compiled, `.deb` and AppImage); `Dockerfile.linux-build` is gone.

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
- **The askpass cannot be asked a spoofed question (D-056, hardened after review).** `-J` hands `-F` on
  to the jump host's own `ssh` but not the `-o` options, so a jump host could have sent a
  keyboard-interactive prompt worded like the key's passphrase question and received the passphrase. Every
  tunnel now starts `ssh` with its own configuration file (in the tunnel's private directory): no
  keyboard-interactive, no password, `BatchMode no`, then `Include` of the user's file and of
  `/etc/ssh/ssh_config` (first value wins, so they cannot be overridden). The passphrase is given only to
  OpenSSH's exact question for exactly the connection's key (or its 100-character cut).

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
  is short). `KERNO_SSH_CONFIG` replaces `~/.ssh/config` as the user's file; either is read through the
  tunnel's own `-F` file, which includes it and then `/etc/ssh/ssh_config` (D-055). The dialog lists the `Host` names of that file and its `Include`s (relative to the
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

## D-059 — `kerno <url>` (M11 Part 6, J4): arguments, unsaved connections, one instance

- **Order at the top of `main`**: the OpenSSH askpass answer first (`ssh` runs Kerno with its prompt as the
  argument), then the arguments (std only: `--version`/`-V`, `--help`/`-h`, one URL; `-psn_…` from the Finder
  ignored; anything else exits 2 with a message that never holds the URL), then the bench modes, then the
  hand-off of a URL to the running instance, then the data. `--version` and `--help` touch no display, GPU,
  keychain, data directory or network.
- **Matching**: engine (MySQL and MariaDB count as one: one protocol, and `mysql://` is common for MariaDB),
  host (ASCII case ignored), port, user and database (empty is none); a saved connection through SSH never
  matches (its host is seen from the tunnel). The matched connection connects with its own secrets: a password
  in the URL is not used for it. Unsaved connections are matched too, so the same URL twice is one connection.
- **Unsaved connection**: listed in `Connections::saved` with a new uuid and a set of unsaved ids, so the
  tree, tabs and menus need nothing new; `store()` (rename, move, read-only, filters) keeps them in memory,
  `reload()` keeps them, `save_password` is off so nothing reaches the keychain and a URL without a password
  is asked for. "Save connection…" opens the connection dialog (save password on) and `save` inserts it under
  the same id (`Storage::add_connection`), so its tabs, history and live session go on. Its query history is
  stored under that id as any history is; never saved, those rows stay unreachable (no cleanup at quit).
  Tabs on it are stored as other tabs and come back on a connection that is gone, as after a deletion.
- **One instance**: `instance.sock` (0600) in the data directory in use, so `KERNO_DATA_DIR` runs, tests and
  bench never meet the user's instance. A connect refused means a crash left the file: it is replaced; a live
  socket is never taken over (a plain second `kerno` opens its own window without listening, as before). The
  peer writes one line (newlines refused in the argument); the listener reads at most 64 KiB within 2 s on a
  thread of its own and hands lines to the window through a channel; the window opens the URL, selects it in
  the tree and comes to the front. Only the redacted URL (`instance::redact_url`) is logged.
  macOS refuses socket options (EINVAL) on an accepted socket whose peer already closed, the usual case for
  a one-line hand-off: the read timeout's error is ignored, otherwise every line was lost.

## D-060 — Password sources (M11 Part 7, J3)

- **Where**: resolved by `Connector::session` (`kerno-core/src/password.rs`), per session, never in the app:
  the command's output never reaches `Runtime.secrets`, so it cannot be saved to the keychain, duplicated or
  shown in the dialog. A command's output is reused for new sessions for 10 minutes, then the command runs
  again (an RDS token lasts 15 minutes; `op` would otherwise prompt for every tab). `.pgpass` is matched with
  the session's database (libpq's default: the user's name).
- **Environment**: `PasswordEnv { path, home, pgpassfile }` built once in `main` and handed to the connector
  through `HostKeyStore.password`, as Part 1 did with OpenSSH; `None` (the drivers' other tests, the tools'
  dialog) reads no file and runs no command. The program is looked up there (PATH, then Homebrew and the
  system's directories), never through the process's own `PATH`; the child gets that `PATH` and `HOME`.
- **Model**: `PasswordSource` (`stored`, `command`, `rds-iam`, `one-password`) on `ConnectionConfig`, plus
  `password_command_unconfirmed`, which every import sets for a non-stored source whatever the file says and
  export never writes; the core refuses to run an unconfirmed command (test, Run now and connect alike).
  The dialog's "Keychain" and "Ask each time" are `save_password`; the command sources keep it on (SSH
  secrets are still saved).
- **Files**: `.pgpass` with group or other access is ignored (libpq); `.my.cnf` world-writable is ignored
  (MySQL), its `[client]` `password`, last one wins, `!include` not followed. An ignored file turns the
  server's "password required" into an error that names the file and the `chmod`, instead of a prompt.
- **MySQL**: `enable_cleartext_plugin` is on whenever the session uses TLS, off otherwise: RDS IAM needs
  `mysql_clear_password`, and a password then never leaves in clear.
- The backup / restore / script tools' `params_for` (`backup_dialog.rs`) resolves the password the same
  way since M11 Part 5b (D-062).

## D-061 — Tab layouts and stacked results (M11 Part 8, J10 and J11)

- **Tab layouts**: `Settings::tab_layout` (`SingleRow`, `Rows`, `Vertical`). One row stays the kit's
  `TabBar`; the rows are the kit's `Tab` in a wrapping container (each in a wrapper of its own id: a lone kit
  tab takes its index 0 as its id) capped at four rows of 32 px; the side list is the kit's `ListItem` (the
  active row in `list.active`, the owner's rule: no bar) in a nested `h_resizable`, so the workspace's own
  panels never change count. The three draw one `tab_parts` (same ids, menus, tint), and every layout's
  scroll container takes `tab_scroll`, so "scroll to the active tab" works in each (an untracked handle would
  ask for an animation frame forever). The layout is part of what the reveal remembers. No drag to reorder:
  the strip has none.
- **List of every tab**: the kit's own `TabBar::menu` was not used (its items come from `Tab::label`, which
  Kerno's tabs do not set, and it has no filter). A controlled `Popover` (`all_tabs_open`, so the palette can
  open it) with `ListItem`s; past 15 tabs a filter whose focus handle the popover tracks.
- **Stacked results**: a way of drawing the same `results`, not new data; switching back gives the tabs. A
  gpui `list` with a `ListState` on the tab (the tail is spliced as results arrive, a new run resets it), so
  only results in view are laid out (50 results: the 50th is not drawn). A grid is `min(rows, 12) + 1` rows
  tall plus the scrollbar and the find bar, or what its lower edge was dragged to (on the pane). The filter,
  edit bar and footer are the focused result's, above and below the list. Focus follows a mouse *up* in a
  result: moved on mouse down, the first click on a button of a result without the focus was lost (the cause
  was not found; it is not the re-render alone); on mouse up it works. A cell's menu opens on mouse down, so
  its entries give their own result the focus first: they never act on another result.
- **Errors**: a script still stops at its first failure (`statements_run_and_their_rows_fill_the_grid`), and
  the error still gets the focus as in tabs; stacked, it shows in place under the results before it, which
  stay in view; the list scrolls to the error when it is out of view.
- **Open in its own tab** shows the results in tabs, that one on screen, until the next run (`unstacked`);
  nothing runs again. The results menu's toggle is the setting itself (all tabs), appended to the result
  menu in `query/tab.rs` rather than to `tabs::result_menu`, whose entries the unit tests list.
- **Find bar over a server sort** (Part 4 follow-up): carried as the widths are (`next_find` →
  `carried_find` → `find::restore` on the first grid of the run).

## D-062 — Column moves and the J9 benchmarks (M11 Part 5, J8 and J9)

- **Moves**: `TableChange::MoveColumn { name, from, to, after }`, after the other column changes, in the
  table's new order; the fewest (`table_draft::out_of_place`: all but a longest increasing run). A move
  carries the column's whole definition: on MySQL and MariaDB it replaces that column's `ADD`, `CHANGE`,
  `MODIFY` or `RENAME` clause (the server cannot `MODIFY` a column added or renamed in the same statement)
  and ends with `FIRST` / `AFTER x`. One `ALTER TABLE` still: the server places positioned columns in clause
  order, each after a column already placed, by its new name (checked on MySQL 8.4 and MariaDB 11).
- **Generated columns** are written again (`GENERATED ALWAYS AS (expr) STORED|VIRTUAL`, `NOT NULL` only when
  set: MariaDB refuses `NULL` there) only when moved or renamed unchanged; an alter of one is still refused.
  Renaming a column that a generated column uses is the server's refusal, not Kerno's.
- **PostgreSQL**: the default dialect refuses `MoveColumn` with `CANNOT_MOVE_COLUMNS`; the form disables the
  arrows and the menu entries of an existing table with that sentence. No recreate-the-table path (v1.1).
- **Script progress**: `tools::run` feeds a `stdin` file itself (a pipe and a task, counted in
  `ToolCommand::sent`) instead of handing the child the file; `psql` reads `--file=-`, plain-SQL restore
  included, so its error lines say `<stdin>` instead of the file name.
- **Bench**: `BackupDialog::run_unconfirmed` runs a script without its confirmation, for `KERNO_BENCH=import`
  only, on its scratch database: it refuses outside a bench mode (`bench::active`). The navigator's `row_of`, `id_at`, `rows_under` and `set_open` are no
  longer test-only (the bench uses them). `--indicative` lifts the load limit of `scripts/bench.sh` and marks
  every output as such; never for published numbers.
- **Reconnect (Part 5b)**: the keep-alive checks the navigator's session every 5 s with a 5 s timeout; a
  check that times out is tried once more at once (a catalog read longer than 5 s on that session must not
  count as a loss), so a server that stops answering is noticed within 10 s of the next check and at most
  15 s after it froze. A failed check, or a statement of a query tab failing with a closed connection,
  starts the reconnect: each attempt limited to 10 s (a frozen server accepts and never answers), retried
  after 0.5 s, 1 s, then every 2 s for about a minute, then every 5 s, while the error is one of the network
  (`Connect`, `Closed`, `Io`); a refused login or an SSH question stops it and is shown as before. The
  status stays `Connecting`, `Connections::reconnecting` tells it apart ("reconnecting…" in the sidebar,
  "Connection lost: reconnecting…" under the connection, "Reconnecting to …" in a waiting tab);
  Disconnect drops the wait with the attempt. Sessions closed on a loss get 2 s each (a close on a frozen
  server waits otherwise). A capped 2 s cadence rather than 4 s then 5 s: a server back just after an
  attempt is reached within about 2 s, the bench's 5 s budget.
- **Query tab on a loss**: a statement cut off (its own error, or the connection found lost while it ran)
  is reported failed with `LOST_WHILE_RUNNING`, the driver's words as detail, and never run again; an open
  transaction is reported rolled back; the cached session is dropped and the manager told. A statement run
  while reconnecting waits for the connection, as any run on a closed connection does.
- **Tests** break a proxy of their own (`kerno_core::testing::Proxy`: forward, drop, black-hole) in front of
  the shared servers. The bench breaks its own container: `docker kill` + `start`, `docker pause` 15 s, and,
  informative, `docker network disconnect` 10 s (OrbStack keeps the connection through that one).
- **Tools**: `params_for` resolves the password with `password::resolve` and the app's `PasswordEnv`, as a
  session does (command, RDS IAM, 1Password, `.pgpass`, `.my.cnf`; an unconfirmed imported command refused);
  it still reaches the tool through `PGPASSWORD` / `MYSQL_PWD` only.

## D-063 — Copying tables to another connection (M11 Part 10, J13)

- **Mapping** (`kerno_core::transfer::map_type`, principles from DBeaver's `DBStructUtils.mapTargetDataType`,
  `JDBCSQLDialect` and `PostgreDialect.convertExternalDataType`): the same engine keeps the type as printed;
  MySQL ↔ MariaDB keep it too, except MariaDB's `UUID` (→ `char(36)`) and its other own types (refused).
  PostgreSQL ↔ MySQL / MariaDB: integers by width, unsigned widened (`int unsigned` → `bigint`,
  `bigint unsigned` → `numeric(20,0)`), `numeric(p,s)` kept (unbounded → `decimal(65,30)`), `boolean` ↔
  `tinyint(1)`, `varchar(n)` / `char(n)` kept up to the MySQL limits, longer or unbounded text → `longtext`
  (`varchar(255)` when in the primary key: MySQL keys no TEXT), `bytea` ↔ blobs, time and timestamps keep
  their precision (default 6 towards MySQL), `timestamptz` → `datetime` read in UTC (`AT TIME ZONE 'UTC'` on
  the source), MySQL `timestamp` → `timestamp` (the wall clock of the source session), `uuid` ↔ `char(36)` /
  MariaDB `uuid`, json ↔ `json` / `jsonb`, enums → `varchar(longest label)` and sets → `text` with a note,
  `bit(n)` ↔ `bit(n)`, money → `decimal(19,2)` read as a number. **Refused**, with the column named: arrays,
  ranges, geometric and spatial types, network types, `interval`, `time with time zone`, `bit varying`,
  composite and other user-defined types across engines. A refused column blocks Start until skipped or given
  a type by hand; a generated column is skipped by default.
- **Created tables** get the copied columns, their NULL rules and the primary key (when all of it is copied);
  identities become `GENERATED BY DEFAULT AS IDENTITY` / `AUTO_INCREMENT` (MySQL: first key column only) so
  copied values can be written, and a PostgreSQL identity is moved past them (`setval`) at the end. Defaults,
  indexes, foreign keys, checks and triggers are not copied and the report lists them. Tables are created in
  foreign-key order among those picked.
- **Writes**: values are literals of the target dialect for the target's own columns (read back after the
  `CREATE`); one multi-row `INSERT` per batch (1,000 rows), each in its own transaction, rolled back when it
  fails: the batches before it stay (said in the confirmation and the report, with the failing batch's first
  row). Cancel stops between batches (and the statement in flight). A table that exists stops the copy, is
  appended to or skipped (the user's choice); Append needs the table and every target column.
- **Sessions**: source and target each run on a session of their own (`open_session`), never the navigator's;
  the source is read with back-pressure, so memory holds one batch. A read-only target is refused in step 2;
  the statements (the `CREATE`s, one sample `INSERT`, the `setval`s) go through `sql_confirm` on the target's
  environment (production asks twice).
- **Reuse**: `backup_dialog::{SchemaEntry, fetch_objects}` became `pub(crate)`, and the steps column and the
  choice chips were extracted from the backup wizard as `wizard_steps` and `chips`, which both wizards draw.
  The backup wizard's checkbox tree stays its own: it is bound to its `Selection` and to pg_dump's schema /
  table rules; the copy wizard draws a lighter one. The navigator has no multi-selection: the wizard's
  checkboxes are the selection (the table or schema it was opened on comes ticked).

## D-064 — One script on several connections (M11 Part 11, J14)

- **Each target is an ordinary query tab** (`<script> @ <connection>`, inserted after the source tab) run by
  `QueryTab::run_script`, which enters the tab's usual path (`launch` → connect on demand → `execute` →
  `sql_confirm` → `start`): history, notices, cancel, manual commit and the environment's questions are the
  tab's. Two differences only while the tab is a target: on a read-only connection each write is marked
  refused and shown in its place while the reads run (a single tab still refuses the whole run), and "Continue"
  lets the loop go past an error; a cancel always stops. The last result stops at its first page so the run
  ends and frees its session.
- **Declined confirmations**: `sql_confirm::open_or_decline` calls back when the confirmation view is released
  without its action having run (Cancel, Escape, a click beside it); `open` is that with nothing to call.
- **Scheduling** (`multi_run::next_to_start`): at most 4 targets run at once, and a target starts only while no
  started one is still connecting or asking, so two prompts (password, confirmation) are never open at once.
  "Stop every connection" cancels the running targets and skips the waiting ones at the first target that
  stopped at an error; a declined confirmation skips only its target.
- **Refused up front**: a script with no statement or with parameters (`:name`, `$1`, `?`), which a run on
  several connections cannot ask for.
- **The run outlives its dialog**: the workspace keeps the last one (Query ▸ Show Last Run on Connections); a
  new one is refused while it is in progress (its summary is shown instead).
- **Reuse**: the tree is `navigator::model::sidebar_items` drawn with the kit's `Checkbox` (the backup wizard's
  checkbox lists are methods of its own state, D-063); the entries are the editor menu's Execute ▸, the query
  tab's Run dropdown (`tabs::run_menu`: the main toolbar has no run menu), the palette and the Query menu.

## D-065 — XLSX export and the portable connections file (M11 Part 9, J12 and J15)

- **XLSX** (`kerno_core::xlsx`): `rust_xlsxwriter =0.102.0` (MIT OR Apache-2.0), default features off plus
  `constant_memory` (rows go to a temporary file per sheet; 11 MB peak RSS growth for 1,000,000 rows). Its
  tree (`zip`, `zopfli`, `typed-path`, `constant_time_eq`, `tempfile`) passes `cargo deny` with no exception.
  `ExportFormat::Xlsx(XlsxOptions)` goes through `export_file`'s writer thread like the text formats; an
  `Exporter` (the clipboard path) refuses it. Excel dates are computed by Kerno (days from civil, UTC for
  zoned timestamps, the header says "(UTC)", rather than a cell note), not by the crate's parser, so anything
  it cannot read stays text. A number is a number only when a double holds it as written (≤ 15 significant
  digits, < 2^53). Rows per sheet are fixed (1,048,576 less the header), not an option. Tests read the file
  back with `xlsx::read` behind `test-support`, using `zip` already in the tree.
- **Connections file version 2**: connections carry their id (`PortableConnection { id, #[serde(flatten)]
  config }`); only a UUID is kept, once. Environments: the user's and the edited built-ins; the ones the
  exported connections use always go, so a production connection never lands in "local". Tasks carry their
  connection's engine, so `TaskSettings::check` refuses a password in the extra arguments on export and on
  import; a task whose connection is gone is left out and counted. Tasks match by name.
- **Import never writes the secret store behind the user's back**: it bypasses `Connections::save` (which
  rewrote every secret of the id, deleting a linked entry) and writes storage directly, then reloads the
  list. Only "Enter now" (`secrets::save_password`) and a replaced connection left unlinked over an existing
  entry (`delete_secrets`) touch the store. The lookup is `secrets::has_secrets` (any of the three keys, so an
  SSH passphrase counts too), through the cached store. A saved password is linked automatically only to a connection this copy of Kerno already
  has, pointing at the same server (`portability::default_choice`: existing, and not `reaches_another_server`,
  which compares engine, host, port, user and the whole tunnel); a new connection whose id has an entry, or a
  replace towards another server, starts on "Ask when connecting", and linking is an explicit choice whose
  label names the server (security review). A new connection not linked gets a fresh id (its tasks follow),
  so the entry of the data directory it came from is never deleted or overwritten. A replace keeps this
  computer's `client_dir`. The secrets of an id are one set (password, SSH password, key passphrase): the
  link offers all of them, its label naming each destination (database `user@host:port`, SSH
  `user@host:port` and jumps); "same server" includes the whole tunnel (client, host, port, user, jumps, key
  file). Unlinked, a replaced connection loses all three before "Enter now" stores the new password, and a new
  one gets a fresh id, so no old SSH secret can reach another SSH server (second security review).
  `Connections::import` is removed (no caller left).

## D-066 — Linux packages (M11 Part 12, J16)

- **Floor: Ubuntu 20.04, glibc 2.31.** Tried first in the order of the plan, and it built with no backport:
  GPUI's Linux dependencies (libxkbcommon 0.10, libxcb 1.14, clang 10 for bindgen) are enough, so Debian 11
  and Ubuntu 22.04 were not needed. Both binaries need at most `GLIBC_2.30` (`objdump -T`) and no
  `libstdc++`; the README says 2.31, the glibc of the build image, where every package is started.
- **One image, tools only** (`packaging/linux/Dockerfile`, build context `packaging/linux/`, about 1.5 GB):
  no source and no `target/` in its layers. `scripts/bundle-linux.sh` runs it with the source mounted
  read-only, `dist/` writable and the volume `kerno-linux-cache` as `CARGO_HOME` and `CARGO_TARGET_DIR`
  (about 5 GB for both targets), so a second run rebuilds only what changed. Both targets are built with an
  explicit `--target`, so build scripts and proc-macros are compiled once for the arm64 host and shared.
  `scripts/bundle-linux.sh --clean` (or `scripts/release.sh --clean`) removes the image and the volume.
  `Dockerfile.linux-build` and `.dockerignore` are removed; `scripts/install-linux-deps.sh` stays for
  building on a current Debian or Ubuntu.
- **x86_64 is cross-compiled in the arm64 container, never emulated**: `dpkg --add-architecture amd64`, the
  arm64 sources tagged `[arch=arm64]` (ports.ubuntu.com) and `archive.ubuntu.com` / `security.ubuntu.com`
  focal lines tagged `[arch=amd64]`; `crossbuild-essential-amd64` and the `-dev:amd64` libraries;
  `CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc`, `CC_`/`CXX_`/`AR_x86_64_unknown_linux_gnu`,
  `PKG_CONFIG_ALLOW_CROSS_x86_64_unknown_linux_gnu=1` and
  `PKG_CONFIG_LIBDIR_x86_64_unknown_linux_gnu=/usr/lib/x86_64-linux-gnu/pkgconfig:/usr/share/pkgconfig`
  (target-suffixed, so host build scripts keep the arm64 libraries). The image is arm64: on an x86_64 host
  Docker would emulate it (ROADMAP limit). Emulation is used only by the start check.
- **Pinned downloads** (build tools, like crates): base image
  `ubuntu:20.04@sha256:8feb4d8ca5354def3d8fce243717141ce31e2c428701f6682bd2fafe15388214` (the multi-arch
  index); `rustup-init` 1.29.3 aarch64
  `9f608c142e0e38a78957d0661ba1dfaf0b6fb37184cfaa4b9a0aa57af6184282`, which installs the toolchain of
  `rust-toolchain.toml` (rustfmt and clippy too, so runs do not download them) with both targets; the
  AppImage type-2 runtime, `github.com/AppImage/type2-runtime` release `20251108`, `runtime-aarch64`
  `00cbdfcf917cc6c0ff6d3347d59e0ca1f7f45a6df1a428a0d6d8a78664d87444` and `runtime-x86_64`
  `2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d`; apt packages from Ubuntu's signed
  archive. The app gains no network behaviour.
- **Packages per architecture**, one install prefix (`bin/kerno`, the desktop entry, hicolor PNG icons and the
  SVG, licences, notices and README under `share/doc/kerno`) laid out three ways:
  `kerno-<v>-linux-<aarch64|x86_64>.tar.gz` (unchanged layout and names: the updater's `artifact()` and
  `check_tarball` rules, one top folder, no link; ownership 0/0); `kerno_<v>_<arm64|amd64>.deb` (the prefix
  under `/usr`, `dpkg-deb --root-owner-group`, `packaging/linux/control.in`; `Depends` from
  `dpkg-shlibdeps` run with `DEB_HOST_ARCH` set for the foreign binary, plus `libvulkan1` and
  `libwayland-client0`, which GPUI opens with `dlopen` and `dpkg-shlibdeps` cannot see (found with
  `strings`); `Recommends: mesa-vulkan-drivers | vulkan-icd`, `Suggests` the database client tools);
  `Kerno-<v>-<aarch64|x86_64>.AppImage` (AppDir with `AppRun`, the desktop entry and the icon at its root,
  `mksquashfs -comp gzip -all-root`, the runtime prepended). The AppImage bundles no library: the binary
  links only `libxcb`, `libxkbcommon`, `libxkbcommon-x11` and glibc (desktop libraries), and the README
  lists them with the `apt install` line for a system that lacks one. The `.deb` has no `Maintainer` e-mail (none in the repository); an apt repository
  needs one (owner's step, ROADMAP).
- **Start check** (`scripts/check-linux-packages.sh`, run by `release.sh`): the tarballs' listings against
  the updater's rules, then every package in a fresh `--rm` container named `kerno-check-*` of Ubuntu 20.04
  and Debian 13, arm64 natively and x86_64 with `--platform linux/amd64`: `apt-get install ./kerno.deb`
  (Depends resolve), the tarball and the AppImage (`--appimage-extract-and-run`) after installing the
  `.deb`'s Depends; each compares `kerno --version` with the workspace version. Every pull and run names
  its platform, and the base images it pulled are removed by id: a tag holds one platform, so pulling the
  amd64 `ubuntu:20.04` leaves the arm64 one untagged, out of reach of a removal by tag.
- **`scripts/release.sh --linux-only [version]`** builds and checks the Linux packages alone, on a tree
  that may be dirty, and prints the `gh release create` command with every artifact; the full mode keeps
  the clean-tree check and the macOS half unchanged. `KERNO_JOBS` (default 3) and `KERNO_NICE` (default
  10) are the script defaults.
- Measured on 2026-11-14 (Apple Silicon, OrbStack, 11 CPUs, `KERNO_JOBS=8`, while three other agents
  compiled): arm64 release build 4 min 44 s from an empty cache, x86_64 cross build 249 s after it; a
  second `release.sh --linux-only` with the cache warm took 2 min 0 s, start checks included (12 runs).
  Sizes: tarballs 35.3 MB (aarch64) and 36.0 MB (x86_64), `.deb` 21.2 MB and 22.9 MB, AppImages 36.7 MB
  and 37.4 MB.
- Not verified: an AppImage mounted through FUSE (containers have none; `--appimage-extract-and-run` is),
  the app's window on a real Linux desktop from these packages (the check is `--version`), distributions
  other than Ubuntu 20.04 and Debian 13.

## D-068 — The AI assistant (M11 Part 13, J17)

- **The second and last network exception, by construction.** `kerno_core::ai::build_payload` is the
  only way a request body is made and `ai::body` the only way it becomes bytes (pretty-printed JSON);
  the panel's "What is sent…" and the log of Settings → AI show those bytes, and they are what goes on
  the wire (the fake-endpoint tests compare them byte for byte). `ai::request` is the only function that
  opens a socket for the assistant (chat and "List models" both): it calls `ai::gate` first, which
  refuses while the setting is off, for an address that is not `https://` (plain `http://` only to
  127.0.0.0/8, `::1`, `localhost`, compared on the parsed host), for an address carrying a user name,
  password, query or fragment, and for a host other than this computer that is not in
  `AiSettings::confirmed`. No warm-up, no background request: "List models" runs on click only.
- **HTTP client: `ureq` 3.5** (MIT OR Apache-2.0), `default-features = false, features = ["rustls"]`:
  rustls with ring and webpki-roots, all three already in the tree; new crates `ureq-proto`, `base64`
  0.23, `utf8-zero`. No HTTP client was in the tree (GPUI's `http_client` is a trait without a
  transport; the update check uses the system's `curl`, which would need the key in a file or on stdin).
  The agent is built per request with `proxy(None)` (the default reads `HTTP(S)_PROXY`, a host the user
  never configured), `max_redirects(0)`, `https_only` for non-loopback hosts, a 60 s global timeout, a
  4 MB answer cap. `cargo deny check` stays clean; no native-tls, OpenSSL or aws-lc in the tree.
- **The key** is a secret-store entry per endpoint (`ai/<base address>`), read in the blocking task that
  sends; never in settings, storage, logs, error text or a process argument. Previews and the log show
  `Authorization: Bearer ***`.
- **Context** comes from the tab's completion catalog (names, types, foreign keys): the tables the SQL or
  sentence names (a word equal to a table name, or one `s` apart), else the tab's schema (else the first
  schema of the search path); 200 tables with columns, then up to 1,000 names. Row data only through
  `Context::sample_rows`, read (`SELECT * … LIMIT 5`, 10 tables at most, on the connection's meta session,
  not recorded in the history) only while the per-request tick is on; a new panel starts with it off and
  Send turns it off again.
- **The answer is text.** Nothing in it runs or opens: no tool calls, no links, no auto-run. `ai::sql_block`
  picks the first ```sql (or plain ```) block, else the whole answer, for Insert (`EditorState::insert`: one
  undo step, then selected); a fix from "Explain error" goes after the text as a new statement.
- **Stop** drops the waiting task and bumps a generation, so a late answer is ignored; the blocking call
  itself cannot be interrupted and ends when the server answers or at the 60 s timeout, on its thread.
- **UI places**: AI entries appended to the editor menu after its other entries (no new `TabAction`, so
  `tabs.rs` is untouched), four palette commands (group "AI assistant", Query ▸ AI Assistant), "Explain
  with AI…" on a plan and "Explain error with AI…" on an error, shown only while the assistant is on;
  the handlers live on the query tab. While off, a command shows "turn it on in Settings → AI" and opens
  nothing; the menu entries are disabled with that reason. Settings → AI is the ninth section, appended
  last so the other sections keep their indexes.

## D-069 — The ER diagram (M11 Part 14, J18)

- **Core, no GPUI** (`kerno-core/src/erd.rs`): `Diagram` from `table_structure` of the chosen tables (read 8
  at a time with `buffered`, then sorted: the same catalog gives the same diagram), one edge per foreign key
  from `foreign_keys` only (not `referenced_by`, which would count each key twice), keys to tables outside
  the diagram left out. A table that cannot be read (dropped meanwhile) is skipped and named, unless none
  can be read.
- **Layout written for Kerno, not ported** (DBeaver's ERD uses GEF/Draw2D's layouts): ranks by foreign-key
  depth (self-references ignored, other cycles cut by a walk in table order), barycentre ordering, layers
  wrapped at 10 tables, unlinked tables in a grid below. Box sizes come from a per-character estimate and the
  text is cut with an ellipsis in core, so the canvas, the SVG and the PNG show the same boxes. Edges are
  orthogonal with a vertical turn in the gap between two tables; when that would cross another table (a key
  that skips a layer), the edge rises in the free gap left of its column, runs above every table and comes
  down the gap left of the referenced one (columns are left-aligned, so those gaps are free). `ponytail:`
  such edges share the top channel in five lanes; a real router if large schemas get crowded.
- **Canvas**: tables are absolutely placed `div`s (ids, context menus, clicks and tests for free), only those
  in view; edges are painted on a `canvas` with GPUI paths under them. Zoom scales sizes and text; under
  40 % the rows are not drawn.
- **PNG through `resvg` 0.46**, already a dependency of the app (folder icons, D-047): no new crate, no
  `deny` change. The SVG is rasterised at 2× (at most 8,192 px a side) with Inter and JetBrains Mono loaded
  from the embedded font files into usvg's font database, never the system's.
- **Exports use the theme's tokens** (`Palette` built from `cx.theme()`): no colour in core or in the tab.
- **Scope**: a schema (a MySQL database) or a table and its neighbours; the tree has no multi-selection
  (NAV-34), so "a selection of tables" is the table's neighbourhood. Dragged positions last for the
  session; the restored tab keeps its schema or table, not its layout.

## D-070 — Vim mode (M11 Part 15, J19)

- **Pure core** (`kerno-core/src/vim.rs`): `Vim::key(Key, &Buffer) -> Vec<Effect>` over the text, the cursor,
  the clipboard text and the lex rules; the app applies the effects through `EditorState`. Motions scan from
  the cursor to the next line break or word boundary; `%` lexes a 64 KiB window, as the bracket marks do. The
  app keeps a copy of the text, read again only after a change, so Normal-mode keys copy nothing.
- **Routing**: the editor's bindings run before key handlers, so printable keys (and ⌃r) are taken in a
  capture-phase key handler around the editor, and Escape, Enter, Backspace, Delete, Tab and Shift+Tab as
  capture-phase actions (Escape not stopped: the kit still closes its list). Arrows, Home/End, Page keys,
  the mouse and ⌘/Ctrl keys are never touched (Beekeeper #1621, #1636). Off, the tab holds no `VimEditor` and
  the wrapper is not rendered. On Linux `secondary-r` is Refresh Rows, a global Ctrl+R binding: in Normal
  mode in the editor vim takes it as redo; on macOS ⌃r is unbound and reaches the key handler.
- **Undo**: each command is one `Replace` (`3x`, `dd`, `p` with a count: one range), one kit undo step. The
  kit's transaction brackets are `pub(super)`, so `c` + typed text is two steps (deletion, typing); `u`
  dispatches the kit's own Undo. `.` replays a change with the text typed in Insert mode, read back at Esc
  when it is one run before the cursor.
- **Register**: the system clipboard is the only register. Linewise is not stored on the clipboard: vim
  remembers its last yank, and a paste is linewise only while the clipboard still holds that text.
- **Block cursor**: the kit's caret width is a private 1 px constant; Normal mode shades the character under
  the cursor with a range decoration instead (the caret stays visible inside it).
- **Visual mode** keeps its anchor and head; an arrow or a click that moved the editor's cursor moves the
  head at the next key. `:wq` closes the tab once the asynchronous write has cleared `unsaved`; `:q!`
  clears it first, and the close goes through the usual question for transactions and pending changes.

## D-071 — Review follow-ups of milestone 11

- **J4, unsaved connections**: history is still written (saving keeps the id, so it follows); what an unsaved
  connection left is removed instead: `Storage::forget_unknown_connections` deletes the history and table
  layouts whose connection is not stored, at quit and at startup (a crash), only in the instance that holds
  `instance.sock` (a second instance beside it must not remove the first one's; a `kerno <url>` handed over
  exits before reading storage). Saved queries and tasks stay (the user named them). Restored tabs of a gone connection, deleted or unsaved alike: query tabs keep their text
  with no connection, the others are not reopened. The dialog of "Save connection…" is titled so.
- **J11**: the query tab follows the "Stack results" setting in its settings observer, so Settings, the View
  menu and the results menu all scroll the result tab bar to the result on screen.
- **J17, commands outside a query tab**: the window takes the four "AI: …" commands when no query tab has the
  keyboard: the active query tab does the work; with none, "Open a query tab first", or the off notice.
- **J15**: an import that replaces a connection calls `Connections::replaced` (also after a failed import):
  the cached secrets of those whose secrets the import changed or forgot are dropped (a moved server must not
  get the old password; the others keep theirs, no second keychain read) and, once the list is read again,
  an open one goes through the window's reconnect, which asks about what its tabs hold, as saving an edited
  connection does.
- **J6**: a layout key names the database the session is in, known before it opens: a PostgreSQL connection
  without a default database uses the driver's `DEFAULT_DATABASE` (`postgres`), as the tree does.
- **J17, Stop**: `ureq` 3.5 has no way to abort a call; Stop says "Stopped" at once and drops the task that
  waits for the answer (the generation check stays as a backstop); the call ends on its own blocking thread,
  so the next request does not wait for it.
- **J13**: the transfer wizard keeps its plan and `CREATE` preview, worked out again (before the next frame)
  only when an input changed: the target fields, the connection list, the mapping fields, a skip, the table
  shown, the three choices, the mapping read.

## D-072 — Visual fixes after the first v1.1 captures

- **J7 occurrences**: a faint tint plus a 1 px frame (the kit draws one style per decoration: two collections).
  New `theme.rs` tokens: `OCCURRENCE_FILL` `#F6F8F2` / `#1B201B` (every syntax colour stays at 4.5:1 or more on
  it: comment 4.57 / 4.60), `MARK_EDGE` Olive `#4D6B00` / Lime `#C8F03C` (5.74 / 12.6:1 on the editor). The
  selection stays a solid Lime fill with no frame, the bracket match a frame in the text colour.
- **J10 rows**: past four rows the rows scroll with the kit's vertical scrollbar; rows start 8 px in; the
  strip scrolls to the active tab again when its label changes (the unsaved mark widened the last tab and
  pushed its close button half out of the strip: the `>` seen at the strip's end was that clipped `×`).
- **J10 side list**: the panel sits on the sidebar's surface, so the active row's `list.active` wash reads as
  the schema tree's selected row; its title is medium weight. A long title ends in an ellipsis (on the element
  that holds the text) and the row carries the whole title as tooltip and accessible description.
- **J10 tab list**: the check and the label (with the unsaved mark) have ids; the list opens scrolled to the
  active tab; the popup gets a 1 px `input.border` edge, since the kit's hairline ring (the completion list and
  menus have the same one) fades into a dark grid.
- **J6 find**: "No match" instead of "No matches" ("3 of 11" after a step was already there); matches tinted
  `GRID_MATCH_FILL` `#DFE4D1` / `#3C4721` (text 13.6 / 8.2:1, links 4.7 / 7.6:1; not the selected row's
  Lime wash), the current one framed in `MARK_EDGE` (4.7 / 7.6:1 on the tint); the bar's buttons are small,
  not extra small.
- **J6 matching rows only**: the row numbers keep the number each row has without the filter (its place in
  the sort shown when sorted), copied numbers too; the result footer and the status bar say "11 of 30 rows"
  ("fetched rows" when incomplete), from the find's `rows_shown`, as the bar does.
- **J8 column moves**: the arrows are the kit's arrow icons (the `↑`/`↓` text glyphs were too thin to read when
  disabled; disabled now looks like every disabled ghost button). Type is 190 px and Default 170 px wide;
  Comment takes the rest (at least 100 px), still inside a 1280 px window.
- **J3 AWS RDS IAM**: "Region" and "Profile" labels on their fields; a muted line under the command says that
  the token needs TLS and what the SSL mode now is (set to "require", or the verifying mode the user chose).
- **Value viewer**: the JSON view is offered (and first) only for a JSON column or a text holding an object or
  an array; an integer opens on Text. The Unix-time line stays in the secondary text colour, now right under
  the value. The size line is the byte length of the value's text, by design.

## D-073 — Closing milestone 11 (main session)

- **The version stays 0.1.0.** "v1.1" is the name of the scope (CLAUDE.md §4 J); nothing was ever published,
  so the first release the owner makes carries everything. The update check compares `X.Y.Z` tags: bump the
  workspace version before a second release, not before the first.
- **Captures of v1.1** are the `v11-*` files of `docs/screenshots/final/`, light and dark, all looked at.
  Three passes: the first found ten visual defects (D-072), the second three more (the active side tab now has
  the strip's underline, the copy wizard's first column, plain punctuation in the AI suggestions), the third
  confirmed them, and a fourth took the screens behind a file panel (the import preview and its passwords
  step, the Execute script progress) by driving the panel's own window. Not captured: states that need a
  broken server ("reconnecting…") and the zoom, which has no screen of its own; their rows in
  `docs/FEATURES.md` say `no screenshot` and name the UI tests that draw them.
- **The first bench run of the final pass was thrown away**: a compile ran beside it. The published numbers
  come from a second run on the final code with nothing else running (`docs/BENCH.md` says which commit).
- **A second full `scripts/check.sh` after the last code change** is the one the final verification quotes.

## D-074 — The repository and the first release, at the owner's request (2026-11-16)

- The owner asked for the GitHub repository to be created private with the `gh` CLI and, the build being
  judged ready, for a release with the installers. This lifts, for these two actions, the rule that nothing
  is pushed or published by the agent (CLAUDE.md §1 and §8): `EvanPluchart/kerno` is created private, `main`
  is pushed, and release `v0.1.0` carries the seven files of `scripts/release.sh` and `SHA256SUMS`, with the
  notes of `docs/release-notes/v0.1.0.md`.
- **0.1.0, not 1.0.0**: the version built, tested and written in every artifact name (D-073). The owner can
  retag; a release and its tag in a private repository are removed with two commands.
- **No Windows build**: Kerno targets macOS and Linux (CLAUDE.md). The release says so.
- **Public the same day.** GitHub Pages is not offered for a private repository on the owner's plan; told
  so, the owner chose to make the repository public. Before the change the whole history of the two pushed
  branches was searched for private keys and tokens (none; test-only credentials of the `kerno-*` containers
  are in the tree by design). The update check now gets its answer: `/releases/latest` redirects to
  `v0.1.0`, which an installed 0.1.0 does not offer (not strictly newer).
- **GitHub Pages** serves the branch `gh-pages` (the committed `site/` of the `kerno-site` branch, built by
  its `scripts/site-publish.sh`) at <https://evanpluchart.github.io/kerno/>: deploy from a branch, no
  workflow file in the repository. The `kerno-site` branch itself is not pushed.
- **The bundle's launch check** was skipped in the release run (`KERNO_SKIP_LAUNCH_CHECK=1`: the owner's own
  Kerno was open) and done by hand on the same `dist/Kerno.app` once it was closed: window shown, data
  written to a scratch directory, clean quit.
- **The uploaded files were downloaded again** and checked against `SHA256SUMS`: seven of seven.
- **The README is short and has pictures**; the reference text it held (SSH, password sources, the AI
  assistant's payload, vim mode, updates, shortcuts, building, checks) moved unchanged to `docs/GUIDE.md`.
  Each picture follows the reader's light or dark theme. Translations: `README.fr.md`, `README.es.md`,
  `README.ko.md`, same structure, the app's own labels left in English (the interface is in English), the
  deeper documents linked as English-only.

## D-075 — The scripts live beside the owner's own Kerno (2026-11-16)

- The owner now uses the installed app daily. `scripts/bundle-macos.sh`, `scripts/capture.sh` and
  `scripts/bench.sh` used to refuse to run while any process named `kerno` existed (the release run of the
  same day stopped on it). They now find the instance they start by its path or process id
  (`scripts/window-id.swift` takes a pid), and neither wait for nor touch another one.
- **Never stop Kerno by name** (`pkill -x kerno`, `killall`): written in `docs/plans/README.md`. Checks and
  captures keep using a scratch data directory and the in-memory secret store, so the owner's connections,
  keychain and single-instance socket are never involved.
- The look of the disk image is on the roadmap for the next release.


## D-076 — The general critique of DBeaver: triage and the proposed milestone 12 (2026-11-17)

- The owner gave a second-hand list of 22 criticisms of DBeaver (`docs/research/dbeaver-critique.md`). The
  first reading of that file was from memory; this triage checked every point in the code at `496e3a4`, by
  the cited lines, the named tests and probes on hard inputs. Verdict counts: 3 answered, 9 gaps, 3 gaps
  (untested), 3 do not apply, 2 not now; two rows only summarise others (`docs/COMPETITOR-ISSUES.md`).
- **An answered verdict needs a test.** Where the behaviour is right but nothing would catch a regression,
  the verdict is "gap (untested)" with a K10 item, not "answered". That includes the benchmarks:
  `scripts/bench.sh` prints MISSED but exits 0, so C1 and C2 are untested until K10 makes a miss fail.
- **Probes that pass are kept, probes that fail are text.** The passing hard inputs of completion, splitting
  and the formatter are `crates/kerno-core/tests/sql_hard_inputs.rs` (36 tests); the failing ones are written
  as reproductions in `docs/plans/M12.md` (K11, K12), not as failing or ignored tests. The classifier probes
  repeated `classify::tests::statements_are_classified` except two failures, which are K7.
- **The cut line of M12 is data first.** K1-K10 close the ways to lose data or act on the wrong server
  without being told (quit with a transaction, a write stuck behind a lock with no cancel, a tab whose
  session moved with `USE`, a script bound to production on open, three classifier holes, an app that
  vanishes at start, errors that drop the server's hint). Completion of CTE columns (K11) and `BEGIN … END`
  bodies (K12) fail safe today and open the next milestone.
- **Choices inside the plan:** no `lock_timeout` on writes (a user may wait on purpose; the missing part was
  the cancel); `WHERE true` and `WHERE 1=1` stay an explicit `WHERE` (DBeaver agrees); `REPLACE INTO` and
  upserts stay non-destructive (they act on keyed rows); a `WHERE` inside any MySQL versioned comment counts
  as absent, whatever the version (one question too many on old version numbers, never one too few); a
  script opened from a file binds only to a connection whose environment asks nothing (with today's
  built-ins that includes staging, whose rules equal local's: whether staging should ask more by default is
  the owner's call); no "simple mode"
  (C20): the default layout is the calm one, toolbar items can be hidden.
- **Proxy (C13):** documented in `docs/GUIDE.md` rather than built: drivers need no download, and the update
  check's `curl` already honours `HTTPS_PROXY` when Kerno is started from a shell that sets it.
- `CLAUDE.md` §4 K and `docs/GOAL-3.md` are the owner's files: their proposed text is the appendix of
  `docs/plans/M12.md`, and nothing in M12 starts before the owner says so.


## D-077 — Writes are cancelled, not timed out (K2, C3; 2026-11-17)

- **No `lock_timeout`** (nor `lock_wait_timeout` on MySQL) is set on any session. A user may wait behind a
  lock on purpose (a migration, a colleague's transaction); what was missing was the way out. Every write
  of the data tab, the table form and the navigator now has a Cancel that calls `Session::cancel()` and
  stops the statement on the server; Cmd+. and the status bar's Cancel reach the active data tab and table
  form; closing a tab whose statement runs cancels it first. An explicit cancel is needed: dropping the
  future does not reach the driver at all (`bridge::spawn` detaches the Tokio task), and on PostgreSQL an
  abandoned statement inside a transaction block is left to finish on purpose.
- **The navigator's Drop, Rename and Create run on a session of their own**, opened for the statement and
  closed after it (as the maintenance dialog and the table form already did), no longer on the tree's
  session or the connection's meta session. A cancel bumps the session's cancel count, which also stops
  every statement queued behind on that session: on a shared session it would have failed the tree loads
  waiting behind a `DROP` held by a lock, and those loads would have waited with it. The cost is one
  connection per confirmed statement.
- **A cancelled read is not an error.** The data tab says "Cancelled: the rows were not read" with a Reload
  button, not the server's "canceling statement due to user request" followed by "Check the filter".
- **Wording of a cancelled write:** "Cancelled: nothing was changed." when one transaction rolled back (the
  data tab adds "The pending changes are kept."); on MySQL, whose DDL commits each statement, a table form
  stopped after its first statement says "Cancelled at statement N of M; the statements before it were
  applied." A cancel that lands after the last statement (during `COMMIT`) is ignored: the write
  succeeded and says so.
- The row being written shows what runs ("Dropping table t…") in place of its hint, with a "Cancel the
  running statement" entry first in its menu and a notification with Cancel that stays until the statement
  ends. No spinner: the app shows work as text everywhere else ("loading…", "applying…").

## D-078 — A script's connection is the user's pick; the pickers accept a closed connection (K6, C6; 2026-11-18)

- **The rule is the environment's rules plus the connection's read-only flag**
  (`ConnectionConfig::binds_scripts_silently`): a file binds by itself only where no rule asks before a
  statement runs or writes (`confirm_execute`, `confirm_writes_twice`) and the connection does not start or
  stay read-only. The name never counts: the built-in staging, which has local's rules, binds silently; a user
  who wants staging guarded gives it a rule. A read-only connection counts as guarded: it is how a user marks
  a server not to be written to, and a script opened beside it is likely meant for another one.
- **The connection pickers of a query tab (its toolbar's, the unbound bar's) and of the main toolbar accept a
  connection that is not connected**, listed
  "(not connected)": the tab binds to it and its first run connects it, as a tab of a closed connection
  already did (DBeaver connects on execute). Before, such entries were greyed out, which made the connection
  a script last ran on (rarely open when the file is opened again) impossible to pick from its own bar. The
  editor's "Active connection" submenu still greys it out (its entries carry a reason that disables them).
- **Deleting unbinds the connection's query tabs before the connection is closed**, so a statement they run
  is stopped as deleted, not reported lost with "the connection is opened again by itself".
- **A connection's other tabs follow the list, not the delete command**: they close when the connection has
  left `Connections` (`ListChanged`), so a deletion refused by the storage leaves them open, and any later way
  of removing a connection gets the same handling (query tabs left on it are unbound there too).
- **Every unbound query tab shows the bar**, not only a file's: one unbound state, one way out.


## D-079 — What the classifier counts as a WHERE, and versioned comments (K7, C21; 2026-11-18)

- **An explicit `WHERE` is the user's decision, whatever it says**: `DELETE FROM t WHERE true` and
  `UPDATE t SET a = 1 WHERE 1=1` stay non-destructive, as in DBeaver (`SQLQuery.isDeleteUpdateDangerous`
  asks only when the parsed statement has no `WHERE` at all). Judging a condition's meaning would be a guess;
  the question is for the statement that has none. `REPLACE INTO` and `INSERT … ON CONFLICT / ON DUPLICATE
  KEY` stay non-destructive too: they act on keyed rows.
- **A versioned comment may run or not, and the classifier takes the more dangerous reading.** MySQL runs
  `/*!NNNNN … */` only from server version NNNNN, MariaDB runs `/*M! … */` and MySQL skips it, and Kerno lexes
  MySQL and MariaDB with the same rules without knowing the server's version. So the classifier reads a
  statement twice, with every such comment and without any (`classify::without_versioned`), and it writes
  or destroys as soon as one reading does; its kind and whether rows come back are the reading with the
  comments. On top of that, a keyword that makes a statement safer counts only beside the keyword it makes
  safe, in the same comment or both outside one: a `WHERE` for its `UPDATE` or `DELETE`, `NOT` / `DEFAULT`
  after `ALTER … DROP`, `FOR` / `KEY` / `DO` / `THEN` before an `UPDATE` in a `WITH`. Consequences:
  `DELETE FROM t /*!80000 WHERE id = 1 */` asks even on a server that would run the comment (the version is
  never compared), `/*!50000 DELETE FROM t WHERE id = 1 */` does not ask (the statement runs whole or not at
  all), `DELETE FROM t WHERE /*!90000 id = 1 AND */ 1 = 1` does not ask (the `WHERE` outside is the user's).
  The rule needs no server version and makes no statement less dangerous than either reading.
- **A bare `/*! … */` is not versioned**: MySQL and MariaDB both always run it, so `DELETE FROM t /*! WHERE
  id = 1 */` keeps its `WHERE`. A bare `/*M! … */` is versioned (MySQL skips it).
- **`PREPARE name [(types)] AS statement` (PostgreSQL) is classified as its statement** (writes and destroys
  as it does, kind `Other`, no rows): the question comes at `PREPARE`, the only time the text is there.
  `EXECUTE` and MySQL's `PREPARE name FROM '…'` cannot be read: they stay writes, never known to destroy.
- **A `MERGE` with a `THEN DELETE` action destroys** (`WHEN MATCHED`, with or without `AND`, and PostgreSQL
  17's `WHEN NOT MATCHED BY SOURCE`): it deletes with no `WHERE` to review. `THEN UPDATE SET …`, `THEN INSERT`
  and `DO NOTHING` do not: they change or add rows, they remove none (a `MERGE … ON true … THEN UPDATE`
  changes every row without the question, as `INSERT … ON CONFLICT DO UPDATE` does; production and read-only
  still guard it, it is a write).

## D-080 — What Kerno does when it cannot start, and the copy before a migration (K8, C16, C14; 2026-11-18)

- **The start-failure window** (`crates/kerno/src/start_failure.rs`): any failure between the data directory and
  the main window opens a small window instead of quitting: a headline in plain words, the next step, the raw
  error, and the buttons that apply (Open data folder, Start fresh (move data aside), Quit). The text is chosen by
  `start_failure(&anyhow::Error)` from a typed error: `storage::OpenError`'s `OpenProblem` (classified in
  `storage.rs` from SQLite's error code, where the code still exists), `kerno_core::Error::Io` / `Storage`, an
  app-side `NoDataDir`. No new variant in `kerno_core::Error` (K9 owns `error.rs`).
- **"Start fresh" is offered only when the data is the problem and moving it helps**: a newer schema, a damaged
  or non-SQLite file, a migration that failed (rolled back; the copy is in `backups/`, so moving the store aside
  loses nothing), another SQLite error while opening, a list that cannot be read. Never for a store another
  program locks, a data folder that cannot be written (the rename would fail too), a failed backup copy (the
  store is healthy: free the disk), a home folder that cannot be found, or anything after the store opened (the
  theme, the window): moving the data would not help there. It is also not offered while another Kerno answers
  on the instance socket of that data folder, checked when the window opens and again at the click (a plain
  second launch has no URL to hand over, so it reaches the store): moving the store would pull it from under the
  running one. Restoring by hand names the three files: SQLite applies a `-wal` left beside the file put back.
- **"Start fresh" renames, in the same folder, all of the store's files or none**: `kerno.db` and its `-wal`,
  `-shm`, `-journal` become `kerno-moved-<UTC time>-schema<N>.sqlite3` with the same suffixes (SQLite still pairs
  them; a store in use keeps its recent writes in the `-wal`), `N` read through SQLite read-only (it
  reads the log; "unknown" when unreadable). A target that exists refuses the move; a rename that fails undoes
  the ones done. Settings, folders and environments live in the store and move with it; `known_hosts`,
  `folder-icons/`, `logs/`, `keymap.json` and the keychain stay. Kerno then loads a new store and starts in the
  same run; the main window says where the old data went.
- **The copy before a migration**: `VACUUM INTO` (no new cargo feature) into an empty 0600 file created first
  (`backups/` is 0700), then the copy is opened read-only and must read the old schema and pass `PRAGMA
  quick_check`; else the copy is removed and the migration does not run (`OpenProblem::CopyFailed`). Only an
  existing store (schema above 0) about to migrate is copied. **The name puts the time first,
  `kerno-<YYYYMMDD-HHMMSS-mmm UTC>-schema<N>.sqlite3`**, so name order is time order; the plan's
  `kerno-schema<N>-<time>` sorts `schema10` before `schema2`. The three newest are kept; only names matching
  that pattern exactly are ever removed.
- **Every pending migration runs in one transaction**, where each ran in its own before: a failure at the third
  of two pending left the store half-migrated, "as it was" now holds.
- **One unreadable connection row is skipped and counted, never deleted** (`connections_and_unreadable`); no
  write rewrites it (updates and deletes are by id, positions are never renumbered), and an import of a
  connection with the same id fails on the key rather than replacing it. The log names the row's id and where
  the JSON fails, never serde's message (it quotes values).
- **The logger starts first**, in `main()`, with only the paths; a log directory that cannot be made leaves the
  run without a log (stderr says so) and is not a start failure. A failure before the platform runs (no
  display, no GPU) cannot have a window: stderr and the log are all there is.
- **A failed settings save is shown once** (`Notification::error`, not hidden by itself): one notification for
  any number of failed saves (the same one, not pushed again: the kit restarts a toast pushed under the same id),
  removed by the next save that succeeds. The settings apply until Kerno quits.

## D-081 — One wording for the server's errors, and when an error notification stays (K9, C14; 2026-11-19)

- **The wording lives in `kerno-core`**: `describe(&Error) -> Described`, whose `Display` is the message (the
  error's own `Display`, the server's code in brackets after it as every surface showed it: `[23505]`, `[1062
  (23000)]`), then `Detail: …`, then `Hint: …`, one per line; a detail or hint that is empty, blank or the
  message again is left out. The core needs it too (the transfer report, the CSV import's refused record), so the
  plan's app-side `error_lines(&Described) -> Vec<SharedString>` was not written: GPUI draws a `\n` as a line
  break in any text, notifications included, and `Described::lines()` serves the one panel that styles its lines
  (the query tab's result, message in the danger colour). A surface with plain words says them first, then the
  described error: on its own line ("The table “t” was not dropped.", "The commit failed: …") or before a colon
  where the surface already read "X could not be read: msg"; Kerno's advice after a message goes before the
  detail (`crate::advice_after`).
- **`Display` of `DbError` and `Error` is unchanged.** It is what `tracing` writes (`%error`) and what
  `explain`'s markers match; a detail can quote row values ("Key (email)=(a@b.c) already exists."), which is the
  user's data shown to the user, never logged. `Described` is never logged; the quit path's `ROLLBACK` failures,
  the one place a described text reached a log line, log their first two lines (outcome and message).
- **An error notification with more than one line stays until it is closed** (`crate::error_notification`): the
  hint is often the fix, and the kit hides an error after 5 s. Its first line (Kerno's plain words, or the message)
  is the notification's title; the server's lines below are the selectable text the connection-failure
  notification already uses (`connections::dialog::raw_message`, one element id per notification), so a hint
  such as `DROP … CASCADE` can be copied. A one-line error stays as the kit draws it and hides itself.
- **A row of a tree shows one line**: a failed node load shows "Could not read the objects of shop: msg [code]"
  and its detail and hint in the row's tooltip; the connection's detail row (the database list, a connection
  failure's raw text) shows its first line, all of it in its tooltip.
- **A failed COMMIT or ROLLBACK says what came of the transaction from the session's state after the failure**,
  never from the statement: PostgreSQL's driver marks a failed `COMMIT` idle (the server ended the block), MySQL
  reads the server's in-transaction flag with a ping after any failure. A `COMMIT` cut by a lost connection says
  that whether it was saved is not known, as a statement cut while running does.

## D-082 — The bench fails on every missed target, the known idle-memory miss included (K10, C1, C2; 2026-11-19)

- **`scripts/bench.sh` exits 1 when any median misses its target**, after printing the whole table. No target
  is exempted: the bare binary's idle RSS, 152.1 MB on the last run for a 150 MB target (`docs/BENCH.md`, D-053),
  makes a full run fail today. Exempting it, or raising the target, is the owner's decision on idle memory, which
  K10 leaves separate; until then a red run says something true. `--only` steps without that measure are not
  affected.
- **The verdict can be read again without measuring**: `scripts/bench.sh --report FILE` prints the table of a
  raw file and exits as a run would; a run keeps its raw lines in `target/bench-raw.tsv`. This is how the exit
  code was tested, on hand-written files, without building or starting the app.
- Nothing calls the script (`scripts/check.sh` and `scripts/release.sh` do not), so no other step changes.

## D-083 — What completion knows of a CTE or a subquery, and what it leaves out (K11, C8; 2026-11-19)

- **One resolver, extended**: a relation of the statement (`Relation` in `sql/complete.rs`) carries either its
  catalog entry or the output columns of the CTE or subquery it is (`Out`: a name or none, a type or none, or
  "any number of unknown columns" for a `*` nothing can expand). They are computed on the tolerant token stream
  the file already uses, not with `sqlparser`: the statement under the cursor is unfinished by definition. The
  CTEs of every `WITH` around the cursor are computed once, outermost first and in list order, each with the
  ones before it in scope, and kept on one stack an inner `WITH` pushes onto and truncates back; a reference
  reads the stored columns and never walks the body again, so ten chained CTEs cost what one does.
- **Output names** are the ones a user writes: an item's alias (`AS c` or bare `c`), else a column's own name
  (`t.d` gives `d`); PostgreSQL folds an unquoted name to lower case, MySQL keeps it as written, a quoted one is
  kept exactly and inserted quoted. **Engine-made names are not offered** (`count(*)` is "count" on PostgreSQL and
  "count(*)" on MySQL, `1 + 1` is "?column?" or "1 + 1"): they differ per engine, they are rarely what the user
  means to write, and offering them would invite `x."?column?"`. Such an item still takes its place, so a column
  list renames the right columns. `*` and `t.*` expand through the catalog or the inner derived relation; one
  that cannot (a function) ends what is known of the positions.
- **An explicit column list** (`x(a, b) AS (…)`, `(…) AS s(a, b)`, `VALUES … AS v(a, b)`, `f() AS g(a, b)`)
  always names the first columns, whatever the body; the later columns keep their names only when their places
  are known. Set operations: the first branch names the columns. Data-modifying CTEs: the `RETURNING` list.
- **Type detail**: a derived column shows the type of the catalog column it passes through (`id`, `t.total AS
  t`, through any number of CTEs), nothing otherwise. No guessing from an expression.
- **Scope**: a CTE is visible in the main query and in the CTEs after it; with `RECURSIVE` in all of them and
  in itself (its first branch names it). A CTE whose body holds the cursor hides itself and the later ones
  unless the list is `RECURSIVE`. An inner CTE or alias shadows an outer one; a subquery in `WHERE` or `SELECT`
  sees the outer relations, and so does any subquery in FROM, `LATERAL` or not (the server refuses a
  non-`LATERAL` one that uses them; offering them costs nothing). `WITH x AS (…) SELECT x.` without a FROM yet
  offers the CTE's columns, as a table of the search path is offered today.
- **`USING` names relations only after `DELETE` or `MERGE`** of the same query, never followed by `(`: `CREATE
  INDEX … USING btree (…) WHERE …` would otherwise make `btree` a relation; `JOIN … USING (id)` and `ALTER …
  USING expr` stay as they were.
- **Bounds**: eight queries deep (`DEPTH`); deeper, the columns are unknown. Matching parentheses are computed
  once per call, so no walk scans forward for its `)`. Work per call grows with the statement's length times the
  depth; the K10 timing test has a ten-chained-CTE input and stays where it was (0.4 ms a call).

## D-084 — When a routine body stays one statement without `DELIMITER` (K12, C9; 2026-11-19)

- **Where**: in the one pass of `split_statements` (`Blocks` in `sql/split.rs`), only while the separator is
  `;`. Under any `DELIMITER xx` the delimiter alone decides, as before: a body cut by its own delimiter stays
  cut, and mysqldump's `DELIMITER ;;` files split exactly as they did (their `/*!50003 CREATE … END */` are
  opaque comments anyway, K7). Written from the grammars (PostgreSQL's `routine_body_stmt_list`, MySQL's
  compound statements, MariaDB's `BEGIN NOT ATOMIC`); DBeaver's `SQLScriptParser.java` was read, not ported
  (it counts every `BEGIN` and `CASE` and pops on any `END`; its MySQL dialect declares no block words).
- **When a block opens**: only in a statement that starts `CREATE [OR REPLACE] [DEFINER = user] [AGGREGATE]
  PROCEDURE | FUNCTION | TRIGGER | EVENT` (a few tokens read ahead; `CREATE TABLE event (…)` is not one), or,
  on MySQL rules, with `BEGIN NOT ATOMIC` (MariaDB's anonymous block; MySQL refuses it, one error). On
  PostgreSQL at `BEGIN ATOMIC`, once. On MySQL and MariaDB at a `BEGIN` outside parentheses, nested ones too,
  unless it reads as a name: after a punctuation other than `)` or `:` (`NEW.begin`, `a, begin`, `(begin`), after
  one of `SELECT RETURN DECLARE AS ON FROM WHERE AND OR NOT BY SET INTO WHEN IN IS LIKE DISTINCT JOIN`, or before
  a punctuation or `;` (`begin.t`, `BEGIN;`). `lbl:BEGIN` (one token to the lexer) opens too. A bare `BEGIN`,
  `BEGIN WORK`, `BEGIN TRANSACTION`, `BEGIN ISOLATION LEVEL …` is never in a routine's statement: it splits.
- **What closes**: `END` after a `;` or after the `BEGIN` / `ATOMIC` of an empty block, unless followed by `IF`,
  `LOOP`, `WHILE`, `REPEAT`, `CASE` or `FOR`. Every statement of a body ends with `;` in both grammars, the `END`
  of a `CASE … END` expression follows an expression, and `end` as a name follows something else (`start, end`,
  `AS end`): so no `CASE`, `IF`, `LOOP`, `WHILE` or `REPEAT` tracking, and labels after `END` change nothing.
  Chosen over counting `CASE` because MySQL does not reserve `END` (`SELECT start, end FROM t` is legal) and a
  counter would close the block there. The price: a body whose last statement lacks its `;` (`… SELECT 2 END`) is
  never closed; the server refuses it anyway.
- **After the final `END`**: the statement ends at the next `;`. Text between them stays in the statement (the
  server refuses it on PostgreSQL; `END lbl` is MySQL's label). The body never reopens.
- **A body never closed** (still being typed) runs to the end of the text, one statement: nothing after it runs
  on its own, never an inner `DELETE` without its routine. Run script and run at cursor send it; the server
  answers with a syntax error ("syntax error at end of input", MySQL's "near ''"), shown on that one result. A
  runaway block stays linear (20,000 lines in about 45 ms in a debug build).
- **Classify**: `BEGIN NOT ATOMIC …` is no longer a transaction `BEGIN` (it would have shown a transaction that
  is not there): kind `Other`, a write, destructive as its statements are. A `CREATE` with a body stays a
  write, not destructive, no rows (unchanged: the body is stored).

## D-085 — Settings' search, the "what wins" lines, and which menu entries are commands (K13, C4, C5, C20; 2026-11-19)

- **One declaration per row**: `settings_view::ROWS` (a static table: kind, section, element id, label,
  description, keywords) is what the view draws, section by section, and what the search reads; `render_row`
  draws a row from its kind, with its label, its description and its override line. The sub-views
  (Environments, Local clients, AI) are one row each: drawn as the view, found by their keywords, and in the
  search's results a line with **Open**. The environments' rule labels are one const list
  (`environments_view::RULE_LABELS`) the form draws and the search reads. Chosen over a rewrite into a generic
  settings model: the controls stay as they were (ids kept, so every earlier UI test still clicks them).
- **Search**: each word typed must start a word of the row (section name, label, description, keywords), so
  "rap" does not find "wrap" and "font size" finds the three sizes and not ligatures. The tab bar stays and the
  body keeps its 400 px, so nothing moves while rows hide; the results are grouped under their section's name.
  Settings opens with the keyboard in the search; Escape empties a search that holds text (the kit's
  `clean_on_escape`, switched on only while text is typed) and closes Settings on an empty one; ⌘F is a command
  (`search_settings`, "Find a Setting…") bound in a new `Settings` key context, so it is in the keymap file and
  the palette, and the menu bar carries it through `RunCommand` (as the other local commands) so ⌘F elsewhere
  keeps its meaning.
- **Override lines** say "<who> wins: <what it does>." and are worked out from the code that decides, not
  written from the plan: `confirm_destructive`'s names the environments whose rules ask anyway
  (`confirm_execute` or `confirm_writes_twice`, `QueryTab::execute`) and those where these statements run without asking with it off,
  from the environments as they stand (the static line it replaces said "production asks twice … whatever is
  chosen here", false once the user unticks production's rule). Only the settings something overrides carry one
  (pinned by `only_what_can_be_overridden_has_a_line`). Nothing changes behaviour.
- **The connection dialog's line** is `environment::describe`, a pure function of the rules (with the name):
  "no rule of its own; only Settings ▸ Confirmations asks" rather than the plan's "statements run without
  asking", which is false while that setting is on.
- **Menu entries and commands**: a menu entry gets a command when it acts on the tree's selected node or
  folder, the active tab, the result or grid on screen, and its handler is one line (`on_tree`, `on_subject`,
  `on_grid`, `perform_on_active_result`) and its target refuses what the menu greys: 51 commands added (and
  Find a Setting…). An entry that only makes sense where it is (one of a
  list, the value or column under the pointer, the text field's own Cut/Copy/Paste, a status bar item's own
  text) is an exception with its reason in `menu_palette.rs`; three wait for a check their menu has and the target
  does not ("Run again" only on an incomplete result; "Filter by this value" and "Exclude this value" greyed on a
  binary or JSON value). Menus built from closures are named in that file,
  not walked.
- **No "simple mode"** (C20): the default layout is the calm one and the toolbar's items can be hidden; a second
  mode would double what has to be tested.

## D-086 — What a drop, a truncate or a rename reaches, read before the user confirms (K14, C11; 2026-11-19)

- **One driver method**, `Session::dependents(&Change) -> Result<Vec<Dependent>>`; `Change` is `Drop { object,
  cascade }`, `DropExtension { name, cascade }` (extensions are not `ObjectRef`s, and adding a variant there
  would touch every exhaustive `match` on it), `TruncateCascade(table)` or `Rename(object)`. Each `Dependent`
  carries its kind, its qualified name and its `Tie`: dropped with it, emptied with it, blocks the change,
  stays broken, or mentions the name. The default is `Error::Unsupported`, never `Ok(vec![])`: an empty list
  means "nothing else", and the dialog shows nothing where nothing was read. A missing target is an error ("the
  object was not found …"), never an empty list.
- **PostgreSQL drop: the server's own rule, from the catalog only.** A recursive query over `pg_depend` walks,
  from the target, every object that depends on it at any depth and by any kind of dependency, and from each
  object reached up to what it is an internal part of or the extension it belongs to (`'i'`, `'e'`: a view is
  reached through its rewrite rule). Listed: what was reached only through normal dependencies or that way up,
  which is what `findDependentObjects` (`catalog/dependency.c`) reports in its "drop cascades to" notice and
  what a plain `DROP` is refused for. What was also reached by an automatic, internal, partition or extension
  dependency goes with its owner without a word and is not listed: a table's indexes, its own constraints and
  triggers, the sequences its columns own, a partition, an extension's members. A column whose whole table goes
  is not listed apart. A foreign key of another table is listed as itself ("foreign key child_t_fk on
  shop.child"): the cascade drops the constraint, not the table. Names come from `pg_identify_object` (always
  qualified, not translated by the server's `lc_messages`, unlike `pg_describe_object`). Written from the
  PostgreSQL docs and source, not ported (DBeaver's `PostgreDependency` reads one level of `pg_depend` for a
  Dependencies folder of its tree, not for its delete confirmation), so no notice in `THIRD_PARTY_NOTICES.md`. The product never runs the drop to find out (no drop-and-rollback:
  it would take an `ACCESS EXCLUSIVE` lock on a live database); only the tests do, on scratch objects, to
  compare with the server's notice.
- **The same list for a plain drop**, under "The server refuses the drop while these depend on it": it costs
  nothing more and tells before the click that the drop will fail.
- **TRUNCATE … CASCADE** (the Truncate tool with Cascade on): the tables holding a foreign key to it, and to
  those, recursively (`heap_truncate_find_FKs`), read on a session of its own in the table's database, opened
  when the confirmation opens and closed once read.
- **MySQL and MariaDB**: no cascade. A table's plain Drop lists the foreign keys of other tables that point at it
  (`REFERENTIAL_CONSTRAINTS`; a table's key to itself does not refuse anything) and the views over it: MySQL from
  `VIEW_TABLE_USAGE`, with the views over those views, as "These stay but stop working"; MariaDB has no such table,
  so its view definitions are searched for the name and said to be a text search. Other kinds of objects:
  nothing read (Unsupported), nothing shown.
- **Rename: only where a rename really leaves them broken, checked on the servers.** PostgreSQL views, defaults,
  constraints, triggers and `BEGIN ATOMIC` bodies keep the object's oid and follow it: not listed. Bodies kept as
  text (PL/pgSQL, a quoted SQL body) do not: `pg_proc.prosrc` of the current database is searched, `c` and
  `internal` languages, system schemas and extension members left out. MySQL and MariaDB views, routines,
  triggers and events follow nothing: their definitions in every non-system database are searched. The search
  is a `LIKE … ESCAPE '!'` prefilter (the same on both engines whatever they do with backslashes; never a server
  regex built from an identifier) and a word-boundary check in Rust (`schema::mentions`: no letter, digit, `_` or
  `$` on either side, any case). Labelled "These mention its name (a text search: Kerno did not parse them)".
  A column rename gets no list (a column name such as `id` is everywhere).
- **The dialog.** A second entry point, `sql_confirm::open_with_reach`, rather than a field on the fourteen
  `Confirmation` literals. The list loads off the UI thread on the tree's session of the database (so it is
  quick and needs no new connection; the tree's loads of that database wait behind it). Under the explanation:
  each tie under its heading with a count per kind ("Also dropped: 2 views, 1 foreign key"), at most 8 objects
  drawn in all (with a production band and a full SQL preview the buttons stay about 110 px above the bottom of
  an 800 px window, 40 px with 12 lines; 100 would push them out, as `a_long_list_keeps_the_buttons_in_the_window` shows), the rest "and N more", and **Copy the list** for all of it. Empty: the caller's sentence ("Nothing
  else depends on it.", "No other table references it.", for a rename what the engine leaves to follow; on MySQL
  and MariaDB "Nothing this user can see depends on it.": their `information_schema` shows only the objects the
  user has a privilege on, and a routine's body only to its definer or with `SHOW_ROUTINE`, while
  PostgreSQL's `pg_depend` and `prosrc` are readable by every role). A
  failed read: "Kerno could not read what depends on it, so nothing is listed here." then the described error
  (K9), and the button works. **The button waits only where the list is what the statement destroys** (Drop with
  cascade, TRUNCATE … CASCADE): "Reading what depends on it…" with the button disabled, for at most 10 s, then
  "Still reading what depends on it after 10 s. You can go on without the list." and it works; the list still
  comes in when it arrives. A plain drop's and a rename's list are information (the server refuses a blocked
  plain drop itself; K9 shows why): nothing is drawn while they load (most arrive in milliseconds; no line that
  would vanish) and the button never waits, so K2's cancellable drop and K9's refused drop behave as before.
  `read_only`, the environment's questions and the SQL preview are unchanged. A database's drop or rename reads
  nothing: a session of the tree's inside it would keep PostgreSQL from dropping it.

## D-087 — The other long operations: how reads are stopped, the 30 s bound, large script files (K15, C3; 2026-11-19)

- **One way to stop a read on a server session** (`bridge::Stop`): the read runs as a Tokio task whose abort
  handle is kept; stopping aborts it where it waits, so a read queued behind it on the same session is never
  sent, then closes the session, which stops the statement on the server (PostgreSQL: the cancel request in
  `close`; MySQL, MariaDB: `close` now kills a statement whose caller dropped it midway, since the server does
  not see the client leave while that statement waits for a lock). A plain `session.cancel()` was not enough
  for reads: the introspection queries do not go through the drivers' cancel counter, and a cancel request
  landing between two of a read's queries is ignored by an idle server, so the next query would be sent and
  wait again; and a closed session alone does not stop a read queued on its lock (Tokio's mutex is first come,
  first served). Used by the structure tab's read and its "Set comment", the table form's read, the ER
  diagram's read and every read of the tree.
- **The tree has sessions of its own** (one per database on PostgreSQL, one per connection on MySQL), no longer
  the connection's meta session. A read waiting for a lock on the meta session also held its keep-alive ping
  (two 5 s timeouts, `connections/manager.rs`), so the connection was declared lost and reconnected after about
  10-15 s, and a stop could not close that session. The cost: one more server connection per connected
  connection, from the moment it connects (the list of databases and the default database's schemas are read
  on it), held while it stays connected; `ui_tests::restored_tabs_open_a_session_once_shown_and_structure_tabs_close_theirs`
  and `ui_tests::every_owner_of_a_session_closes_it_when_it_goes_away` now count two sessions for a connected
  connection, not one, each owner's on top. The idle memory of D-082 (152.1 MB for a 150 MB target,
  one connection open) was not measured again: the app was not launched for K15, and a second session's
  buffers can only add to it. Two reads that open the same key's session at once keep one; the other closes
  once read.
- **Timeout: a client-side clock, not `statement_timeout` / `max_execution_time`.** 30 s per read of the tree
  (`LOAD_LIMIT` in `navigator/view.rs`), counted on the UI's clock from the moment it is asked for (its wait
  behind other reads of the same session included), then the same stop as Cancel. Every read of the tree has
  it: the rows' loads and the one-off reads (Generate, Show DDL, Edit source, Call, Properties), which end with
  their usual notification and "the server took longer than 30 s. Try again." Server settings would change the session's state, cover only `SELECT` on MySQL, and need a reset
  after each read; the clock costs nothing and the tests advance it (`advance_clock`), so no knob is added.
  One constant, not a setting: no setting covers statement timeouts today, and Settings would gain a number few
  would touch. The other tabs' reads have no clock: their Cancel (and Cmd+.) is on screen while they read.
- **Messages, plain words first, on the row that was being filled**: "Could not read the objects of shop:
  cancelled. Refresh to try again." and "Could not read the objects of shop: the server took longer than 30 s.
  Refresh to try again." (the `reading` strings the row already used for failures; the list of databases says
  "the databases", under "The databases could not be listed."). Every read queued on the same session ends
  with it and says the same. The next read opens a new session.
- **Where Cancel is**: a small "Cancel" button on the "Loading…" line (and on "Loading databases…"), "Cancel
  loading" first in the row's menu, and Cmd+. with the row (or its line) selected; the tree takes Cmd+. only
  when its selected row is being read, else it goes on to the active tab. The menu entry is built in the
  closure of the row's menu, like K2's "Cancel the running statement": `menu_palette.rs` names both among the
  closure-built entries (the palette's Cancel is Cmd+.); the K13 test is unchanged.
- **K14's "what it reaches" list is read on a session of its own** (not the tree's): a slow read holds no
  tree read behind it. The task holds the sending end of a channel; the dialog going (Cancel, the answer)
  drops it, and the read is stopped on the server and its session closed. Its 10 s clock running out does
  not stop it: the dialog still shows the list if it comes.
- **Structure tab, table form, diagram**: Cancel on their "Loading…" screen, Cmd+. (now routed to the
  structure tab and the diagram as well), and closing the tab stop the read on the server; a structure
  already shown stays, else "Cancelled" with "Read again". "Set comment" marks the tab running; Cmd+. stops it
  ("Cancelled: the comment was not changed.", the structure not read again).
- **Data apply**: `apply_edits_until` reads the tab's Cancel before each statement and before `COMMIT`; a
  cancel landing between two statements rolls the transaction back instead of letting the next ones run.
- **AI Stop closes the connection**: `ureq` 3.5's `Agent::shutdown` (no new dependency), through
  `ai::Stopper`; a request asked after the stop is not sent.
- **CSV preview**: one flag per scan, set when a field changes (before the next scan starts) and when the dialog
  closes; the count reads it at each record.
- **ER layout** runs on Tokio's blocking pool, not on one of the runtime's three workers the drivers share, and
  is skipped when the tab was closed or reloaded meanwhile; once started it runs to its end (it has no check
  inside).
- **A `.sql` file over 20 MB** (`LARGE_SCRIPT`, one constant) is not read before the user answers "“big.sql” is
  34 MB": Cancel opens nothing; "Open in the editor" reads and opens it as any file; "Run with Execute
  script…" opens the Tools wizard (Execute script) on the active tab's database, else the default connection's,
  with the file chosen, and the wizard asks before it runs as it does from Tools (K6's rule: nothing binds
  silently). Enter answers nothing. The threshold is lowered by the tests through a test-only field.

## D-088 — A tab whose own server session died: found without touching it, said once (K16, C15; 2026-11-20)

- **No ping on the tab's session.** The plan's "ping its own session every 30 s" would defeat the guards a DBA
  sets against forgotten transactions: any command restarts the server's idle timers (PostgreSQL
  `idle_in_transaction_session_timeout`, `idle_session_timeout`; MySQL and MariaDB `wait_timeout`, which a
  `COM_PING` restarts too), so the transaction and its locks would be kept forever. Proved on the three servers
  with a 1-2 s timeout set on the session itself: pinged more often than the timeout, the session stayed open
  long after it (`kerno-postgres`, `kerno-mysql` `execute::watching_a_session_does_not_keep_it_alive`).
- **One driver method, `Session::gone() -> Option<DbError>`**, a one-shot question that sends nothing on the
  session; default `None`. Once it says `Some`, the session's `tx_state()` is `Idle` (the server rolled the
  transaction back), so K4's badge, K5's idle rule and K1's quit question agree with it.
  - PostgreSQL: the task that drives the connection reads the socket all the time, so the server's FATAL (or the
    end of the stream) is seen the moment it comes; it now keeps it, with its SQLSTATE (25P03, 25P04, 57P05,
    57P01), in a slot the session holds (`connect::Gone`); `gone()` reads the slot, and `tx_state()` says `Idle`
    once it is filled. Kerno's own `close` fills it first ("the session was closed"). Cost: none. The `Closed`
    error a later statement gets now reads "terminating connection due to idle-in-transaction timeout [25P03]"
    instead of "db error: FATAL: …".
  - MySQL, MariaDB: `mysql_async` reads nothing while idle and keeps its socket check private
    (`Endpoint::check` is `pub(crate)`), so the client cannot see the close without sending. `gone()` opens a
    short connection of its own, as a cancel does (`kill_query`'s route: same tunnel, same TLS), and asks
    `SELECT @@hostname, (SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE ID = <its id>)` (a user
    always sees its own threads); only an answer of 0 from the same server (`@@hostname`, read at connect)
    says gone: behind a load balancer or a cluster's endpoint the check's connection may reach another node,
    where the id means nothing, and taking that for gone would close a live session and lose its transaction;
    any failure says nothing; while a statement holds the session it
    says nothing (that statement finds out). Not the connection's meta session: a check queued there would hold
    its keep-alive (K15). Cost: one TCP + login round trip per check (one SSH channel through a tunnel), only
    while a tab holds a transaction, every 15 s, plus one before each Commit.
    A check whose connection cannot be opened is the last one for that session: a login the server refuses (a
    rotated password, an expired token) is not repeated every 15 s against a production server.
- **The tab asks every 15 s (`SESSION_CHECK`), only while it holds a transaction and nothing runs**, on the UI's
  clock (tests advance it, no knob). Armed on the tab's own record of its transaction (`tx_started`, K4), not
  on the session's state, which turns `Idle` the moment a PostgreSQL client reads the close. A tab without a
  transaction is not watched: nothing of it can be lost, and its next statement finds out.
- **What the tab does**: it says once, in its messages and a notification, "The server closed this tab's session
  while its transaction was open: the server rolled it back, and nothing of it was committed. The next
  statement opens a new session." then, when the server said why, plain words (for PostgreSQL's 25P03, 25P04,
  57P05, 57P01) and "The server said: …"; on MySQL "it was idle for longer than its wait_timeout, or it was
  killed". It drops the session (so the badge, "transaction open", Commit and Roll back go); the transaction's
  log is kept, headed "The server closed the session: it rolled these statements back", until the next
  statement; the edits waiting in a result are kept and said so ("Apply runs them in a new transaction"); the
  next statement opens a new session in the tab's database and schema (the target follows the session, C6),
  in the same commit mode. Session settings made with `SET` are not put back.
- **A statement that finds the session gone** (a run, the manual-commit `BEGIN`, a result's Apply) drops that
  session only and asks the connection's own session (`Connections::verify`, the keep-alive's two-try ping):
  the connection is taken for lost, and every tab's session closed, only if that does not answer. Before, any
  such statement called `connection_lost` at once, which closed every session of the connection and rolled back
  the other tabs' transactions; a dead session found by the manual-commit `BEGIN` or by an Apply was kept and
  failed every run after it. Whether the tab held a transaction is its own record, not the session's state
  (which a closed session reports `Idle`): before, a statement that found the session gone never said the
  transaction was rolled back.
- **Commit asks `gone()` first** (3 s at most): on a session the server closed, the COMMIT is not sent and Commit
  says "The commit was not sent. The server closed this tab's session …" instead of "whether the server saved
  the transaction is not known". **A ROLLBACK that finds the session closed succeeds** (the server rolled
  back): the idle rule and K1's "Roll back and quit" no longer fail on a dead session.
- **Behaviour changed on the way**: RefreshConnections now pings each connection with the keep-alive's limit
  (two tries of 5 s) instead of an unbounded ping, so a connection's own session busy for more than 10 s at
  that moment is taken for lost; a statement that meets a closed session now waits for that ping (up to 10 s
  when nothing answers) before the connection is reopened, where it reopened at once.
- **No second notice**: whichever learns first (the tab's check, a statement, the connection's keep-alive)
  drops the tab's session and its record of a transaction, so the others find nothing to say.

## D-089 — Shortcuts changed while Kerno runs, recorded and checked for conflicts; the theme switch; more defaults (K20; 2026-11-20)

The owner's request of 2026-11-20: three theme modes with the system's by default (already there), a key to
switch between light and dark, a Settings page that lists and changes the shortcuts, and more shortcuts.

- **Rebinding without a restart.** GPUI (`gpui-pre` 0.3.12 under `gpui-kit` 0.9.1) has `App::bind_keys`, which
  adds bindings (at one depth of the key contexts the later binding wins), and `clear_key_bindings`, which
  empties the whole keymap: the kit's own bindings (text inputs, lists, dialogs, a popover's `NoAction`s)
  included. Kerno tags every binding it adds with one `KeyBindingMetaIndex` (`actions::KERNO_BINDING`);
  `actions::apply_keymap` keeps the bindings without that tag, clears, puts them back in their order, then binds
  Kerno's anew, after them as before. The meta matters only to `NoAction` (one of meta m hides the bindings of
  meta ≥ m at its depth): the kit's carry none (0), so Kerno's tagged bindings are hidden exactly where its
  untagged ones were. `bind_keys` at start goes through the same function, so a second call replaces Kerno's keys
  instead of stacking them (`a_bad_keymap_file_is_reported_and_the_defaults_stay` relied on the stacking; it now
  pins the replacement).
- **What follows at once**: the keys (the keymap is read at each keystroke); every place that shows a shortcut
  (`workspace::shortcut`, the kit's `Kbd::binding_for_action` in tooltips, the palette's hints, which the kit
  reads at each render and measures again when they change); the native menu bar's shortcut column:
  `App::set_menus` reads the keymap only when it is called, so `apply_keymap` calls it again (`crate::menus()`).
  The menu bar is checked by code, not by a test: the test platform keeps the menus, not their key equivalents.
  A change typed in `keymap.json` by hand applies when Kerno starts or when Settings opens (its Shortcuts page reads
  and applies the file); there is no file watcher.
- **Recording** uses `App::intercept_keystrokes`: interceptors run before any binding or field, and
  `stop_propagation` there keeps the keystroke from every action and from text input (Escape too, which cancels
  rather than closing Settings). The page owns the subscription and holds itself weakly in it: Settings closed
  while recording drops it and the keyboard is back (a test). A lone modifier released is ignored. What is
  written is GPUI's own `Keystroke`, modifiers and key (never `key_char`), in the file's spelling
  (`actions::file_form`): Cmd on macOS and Ctrl elsewhere as `secondary-` first, as the defaults are; Fn left
  out (macOS sets it on the arrows and the F keys, which the defaults bind without it); it reads back as the
  same keystroke on both systems (a test). Shown with `Kbd::format`, as the tooltips.
- **What can be recorded** (`actions::recorded_key`, said on the page): a key that types (letter, digit, sign)
  needs Cmd or Ctrl: Option or Shift alone types a character on a Mac and the global binding would take it from
  the editor; a grid starts an edit on a letter. A named key (Enter, Tab, an arrow, Delete, Home…) needs Cmd,
  Ctrl or Alt, unless the command's keys work only in a part of the window that takes no text (the tree, a
  grid). F1 to F24 work alone. Escape alone cancels, so it cannot be recorded from the page (the file takes it).
- **The kit's keys stay the kit's.** GPUI ranks a binding with no key context with the deepest context, and
  ties go to the binding added later: Kerno binds after the kit, so a command bound everywhere wins over a
  text field's own key in every field (⌘C would stop copying). A command with no context cannot take a key the
  kit binds (`actions::kit_binding_of`, read from the live keymap, Kerno's tagged bindings left out): "⌘C is the
  text fields' Copy: a command that works everywhere would take it from them." A command of the tree or a grid
  may (the field inside it is deeper and keeps its key). Two global defaults already win over a kit key, by
  design and pinned by a test: ⌘↵ (Run Statement over the kit's Confirm) and ⌘. (Cancel over the editor's
  ToggleCodeActions); those keys may be given to another command, the fields having lost them already. The same
  live check pins the new defaults. A recording
  stops when Settings shows another section or its search changes, so no keystroke is taken with nothing on
  screen saying why.
- **Conflicts** (`actions::conflicts`): two commands conflict when one shortcut resolves to the same keystrokes
  on the system and their key contexts can both apply: one has no context, both have the same root context
  (`QueryTab > mode == editor` is in `QueryTab`), or one is drawn inside the other, from an explicit table
  (`actions::NESTED`: `DataTable` and `GridFind` in `QueryTab`, `GridFind` beside `DataTable`), since nesting
  cannot be read from the strings. The tree, the settings and a grid are apart: F4 stays the tree's, the
  sidebar's and the editor's. The page asks ("⌘B is already the shortcut of “Toggle Sidebar” (View). Replace
  takes it from it and gives it to “About Kerno”.") with Replace or Cancel; Replace takes the key from the other
  commands in the same write. The check of the file at start now uses the same rule (it caught only two commands
  of one context before): a hand-written file giving one key to a global and a grid command is reported. A test
  checks that no two defaults conflict under this rule on macOS and on Linux.
- **One file.** A change edits `keymap.json` in place (`actions::changed_keymap`; serde_json keeps the order):
  the command's entry is set (one key as a string, several as a list, none as `null`) or removed when it equals
  the defaults, so "Changed" is exact; Reset all empties `bindings`; `about`, `commands`, the other entries and
  keys Kerno does not know stay. No file yet: the example is written first, with the change. A file that does
  not parse (or whose `bindings` is not an object) is said on the page with Open keymap file and is neither
  applied nor written. Writes run off the UI thread, one writer, in the order made, each reading the file again
  and making its change there (a hand edit made meanwhile stays), through a temporary file renamed over it; while
  writes are pending, Settings opened again takes the page's last text instead of the file. A failed write is a
  notification; the change works until Kerno quits or Settings reads the file again. Keys added beside an older
  default come before it (Commit, Roll back, Next and Previous Tab): the last key bound is the one the menus and
  the tooltips show, so they still say ⌃4, ⌃8, ⌃⇥.
- **The theme switch**: `toggle_theme`, "Theme: Switch Light and Dark" (View menu beside the three theme
  entries, the palette), ⌘⇧D / Ctrl+Shift+D: free in Kerno, the kit's inputs and macOS, a letter (AZERTY), D for
  dark. It reads the theme on screen (`cx.theme().is_dark()`), not the setting, and stores the opposite as Light
  or Dark: from Follow system showing dark, Light. Follow system comes back from Settings or its command. It
  works with Settings open too (the Theme row redraws at once). The system's appearance is followed in Follow
  system only (`workspace::system_appearance_changed`, the observer's body; GPUI's `simulate_appearance_change`
  is crate-private, so the test calls it as the window would).
- **New defaults**, each with its source beside it in `actions.rs`, all with Cmd or Ctrl and a letter, an arrow
  or Page Up/Down (no digit or sign: AZERTY puts digits behind Shift), none on a key of the kit's inputs, macOS,
  the Linux desktops' usual ones or vim mode (`the_new_defaults_are_bound_and_take_no_reserved_key`), none bound
  by the kit in the live keymap, each reaching its command in the SQL editor and in a text field
  (`the_new_defaults_win_in_the_editor_and_in_a_text_field`):

  | Command | Key | Source |
  |---|---|---|
  | Theme: Switch Light and Dark | ⌘⇧D, Ctrl+Shift+D | new (owner's request) |
  | Next / Previous Tab | ⌘⌥→ / ⌘⌥←, Ctrl+PageDown / Ctrl+PageUp | Safari and Chrome; Linux browsers |
  | Close Other Tabs | ⌘⌥W, Ctrl+Alt+W | Safari |
  | Reopen Closed Tab | ⌘⇧T (Super+Shift+T on Linux) | browsers; not `secondary`: Ctrl+Shift+T is the query tab's Maximize Results Panel |
  | Commit / Roll Back Transaction (second keys) | ⌃⌥⇧K / ⌃⌥⇧R | DBeaver's `core.commit` / `core.rollback` second keys |
  | Show Messages of the Last Run | ⌃⇧O | DBeaver's `ui.editors.sql.show.output` |
  | Server Administration | ⌃⌥⇧B | DBeaver's `ui.dashboard.open` |
  | Refresh Schema Tree | ⌘⌥Y, Ctrl+Alt+Y | DataGrip's Synchronize (DBeaver has none) |
  | Set Selected Cells to NULL | ⌘⌥N | DataGrip's Set NULL |
  | Revert Selected Rows | ⌘⌥Z | DataGrip's Revert Selected |
  | View ER Diagram | ⌘⌥⇧U | DataGrip's Show Diagram |

  Fifteen keys on thirteen commands, fewer than the "few dozen" aimed at: of the 145 commands without a key, most
  act on one selected thing from a menu (create, generate, tools, one result tab) and have no key in DBeaver,
  DataGrip or macOS; inventing keys for them would take keys users may want and add conflicts. Not taken, with
  the reason: DBeaver's Ctrl+S / Ctrl+R for Apply / Discard (⌘S already applies in a grid; ⌘R refreshes),
  Ctrl+Shift+D for Open Table (that is the theme switch on Linux), Alt with a letter alone (types a character on
  a Mac, and vim mode reads it), ⌃F2/⌃F3 (macOS focus keys). The page is where "plenty of keybindings" comes
  from: every command takes one there.
- **Kept**: Open Keymap File (command, Kerno and Help menus, the button at the bottom of Settings, and the page's
  button when the file cannot be used): the file stays the user's to edit and to copy to another computer. A new
  command "Keyboard Shortcuts…" opens Settings on the page (Kerno menu, palette). The page is a section of
  Settings, appended last so the sections before it keep their places, and one row of `settings_view::ROWS`
  (found by "keymap", "shortcut", "hotkeys"…).

## D-090 — The value panel: one per tab, a global switch, F7, Set or Cmd+Enter (K17; 2026-11-20)

- **Where it lives**: one `ValuePanel` per query tab and per data tab (not per grid), made when the setting
  shows it and dropped when it hides it, so a hidden panel costs one setting read per frame of the tab and the
  1M-row scroll bench is untouched (off by default). Per tab rather than per grid: a query tab's stacked results
  (J11) get one panel beside the list that follows the focused result, not one squeezed beside each short grid;
  and text not set can outlive its grid (the page read again) long enough to be kept. The grid holds a weak
  handle to the panel following it, for the double click and Shift+Enter.
- **One switch for every grid, stored with the settings** (`value_panel`, `value_panel_width`), as the
  selection statistics' switch: DBeaver remembers its panels per presentation, not per table, and J6's
  per-table layouts would leave a query's results (no table) without a place. The width is written when a drag
  of the kit's `h_resizable` handle ends (`ResizablePanelEvent::Resized`), clamped to 240-900 pt; a panel already
  made keeps its width until it is made again.
- **F7**, DBeaver's key for its panels (`org.jkiss.dbeaver.core.resultset.grid.togglePreview`, also Ctrl+7):
  free in Kerno, the kit's inputs and editor, and vim mode; a function key, the same on AZERTY; on a Mac
  keyboard with media keys it is Fn+F7 unless the function keys are set as standard keys. Context-less, so it
  works with the keyboard in the editor; the command is "Toggle Value Panel", last of the Data group.
  Review addition: a second default, Ctrl+Alt+V, bound last so that menus and tooltips show it: the key that
  works as is on a Mac keyboard.
- **The editor**: the kit's code editor (`EditorState`, the SQL editor's component; the dialog's plain
  `Textarea` has no highlighting), `language("json")` for JSON (tree-sitter JSON is in the kit's `tree-sitter`
  feature; folding on), plain text otherwise, soft wrap except for hex, no line numbers, read-only (still
  selectable and copyable) where the cell takes no value. Focus: the grid keeps the keyboard and its arrows while
  the panel only shows; a double click or Shift+Enter gives it to the panel's editor; Escape gives it back. Tab
  stays the editor's (it indents JSON).
- **Commit and abandon**: Set or Cmd+Enter writes the text as one pending change (the dialog's path,
  `Grid::set_value`); JSON is checked first (`viewer::saved_value`: refused with the parser's line and column,
  nothing written). Cmd+Enter is taken by the panel before "Run statement" (which, bound without context, wins
  over the kit's own Cmd+Enter in an input). Blur never writes: half-typed text is never written because the
  selection moved. While the text differs from the value the panel stays on its cell and says so; Escape (or
  Discard) drops it, as Escape drops a cell's inline edit. If the rows it was for go, it is put on the clipboard
  and a notification says so, rather than dropped silently or written.
- **Refusals** are the tab's words, not the grid's generic "These rows cannot be edited.": the data tab's
  `read_only_reason`, a result's `ReadOnly` (or "Wait a moment…" while its table is read), handed to the panel as
  the tab draws; `Grid::editable`'s own sentences where it has one.
- **Layout**: the grid keeps at least 240 pt; when the area is narrower than the grid's minimum plus the panel's,
  the panel is not drawn (and the double click and Shift+Enter go back to the inline editor and the dialog) until
  the window is wider again. A text column holding JSON is shown indented when read-only but as stored when it
  can be edited, so that Set never re-indents a user's text.

## D-091 — The row a foreign key points to: read on the panel's own session, after a rest, cached per grid (K18; 2026-11-20)

- **The session**: one of the value panel's own, opened through `Connections::open_session` in the tab's
  connection and database at the first key to read, kept for the next ones, and closed when the panel goes
  (hidden, its tab closed: a `oneshot` sender in the panel; its drop wakes the task that closes the session),
  when its connection or database changes, after a failed read, and when a read is stopped. Not the
  connection's meta session: K15 found that a read waiting there (behind a lock) also holds the keep-alive
  ping, and the connection is then declared lost and reopened. Not the tab's session: a data tab's runs its
  pages and counts, and a query tab's holds the user's transaction and statements; a preview's read there would
  wait behind them, a stop would close the user's session, and on PostgreSQL a failed read would abort the
  user's transaction. Cost: one more server connection per tab whose panel has shown a key.
- **Inside an open transaction**: the panel's session is in auto-commit, so it sees committed rows only. A key
  the user's own open transaction inserted is "No row with this key", and the panel then adds "Rows of this
  tab's open transaction are not seen here: only committed rows are." A pending key (edited, not applied) is
  previewed by its pending value, said under the heading ("The row of the pending value, not applied yet.");
  a pending default or expression is not known until applied, and the panel says so.
- **The statement** is the cell menu's jump's (E3): `data::tab::reference` builds the referenced table and the
  filter on its key from the row's values (pending ones included, every column of a composite key, nothing
  when one is NULL or binary), and `browse::page_sql` writes `SELECT * FROM <table> WHERE <key> = <literal> …
  LIMIT 1` through `Filter::where_sql`, its literals typed by the columns that hold the key here (no catalog
  read of the referenced table). `SELECT *` rather than the columns shown: one read serves Kerno's choice, the
  user's and "Show all columns", and gives the column list "Choose columns…" offers. Bounded to 10 s on the
  UI's clock, then stopped (`bridge::Stop`: the task dropped and the session closed, so the server stops the
  statement): "Not read: the server took longer than 10 s." with Try again; Cancel on the "Reading…" line says
  "Not read: cancelled.".
- **Debounce, then stop**: a read starts once the selection has rested 250 ms on a key, so moving through the
  cells reads nothing; a read in flight is stopped when the selection moves to another key or off the keys
  (another column of the same key, or a row with the same key, keeps it). Cancel-and-replace alone would open a
  session per cell walked through, since a stop closes the session. "Reading the row of …" shows only after
  300 ms: a fast answer does not flicker.
- **The default columns** (`default_columns`, a pure function): names matched case-insensitively without `_`,
  `-` or spaces, in this order: name, full name, display name, first name, given name, last name, surname,
  family name, title, label, email, email address, mail, username, login, nickname, handle, code, slug; then
  any other column whose name ends in name, title or email; else the text columns, short types before long
  ones (`text`, `longtext`…); else the first columns. Never the key itself, nor binary or JSON columns; at most
  eight.
- **The choice of columns** is stored per connection and referenced table (J6's key: database, schema, table)
  in J6's `table_layouts` table under a key of its own (`fk-preview:` before the layout's key), not as a field
  of the table's layout: an open data tab of that table writes its layout from what it holds and would
  overwrite a choice made elsewhere. No migration (no copy of the store at the next start, no refusal by an
  older Kerno); it goes with the connection as the layouts do. "Kerno's choice" in the menu removes it.
- **The cache**: per panel, by referenced table and key values, rows and "no row" only (never a failure or a
  stop), cleared when the panel follows another grid: a refresh, an Apply, another page, a re-run each make a
  new grid, and rows may have changed. At most 256 keys, then cleared.
- **Query results**: an editable result of one table (K17's `ReadOnly` resolved) knows that table's foreign keys
  already (`Grid::structure`): its panel previews them, its columns mapped to the table's through the result's
  labels; no catalog read is added for any other result. "Open the row" is a data tab's only (a result's cell
  menu has no jump).

## D-092 — The split tab area: Kerno's own pane tree over its strip, four groups at most (K19; 2026-11-20)

- **(a), a pane tree of Kerno's own, not the kit's `DockArea`.** The kit's dock (`gpui_base::dock`: `DockArea`,
  `TabGroup`, `PaneTree`, a `PanelRegistry` for its own persisted schema) owns its panels, their order, their
  lifetime and its strip, drawn by a `TabGroupRenderer`. Moving the tabs into it would have meant redoing on its
  renderer J10's single row, rows and side list and the list of every tab, the environment tint on titles, K4's
  transaction badge, the unsaved mark, the tab menus, the middle click, and the restore format, and handing over
  the order of the tabs that K1's quit question, K2/K15's stop on close, K5/K6's transaction and connection
  checks, `open_query_after` and dozens of tests rely on. Kerno keeps its flat `tabs` list (every tab of every
  group, kept in the layout's order by `sync_tabs`, so an index into it still reads left to right) and adds
  `workspace/panes.rs`: `Layout<K>` (a tree of `Pane::Tabs(Group)` and `Pane::Split { axis, children, shares }`,
  and the focused group), pure and unit-tested; only the drop zones are the dock's rule (the outer 35 % of a
  side splits, the middle joins, as `gpui_base::dock::split_placement_at`). The tab subscriptions are keyed by
  tab id instead of running beside the list by index, so a tab can move without its subscription.
- **Dividers**: the kit's `resize_handle` (its look and cursor), dragged with GPUI's `on_drag_move` on the split,
  the shares kept as fractions in the tree and laid out as relative flex bases. Not the kit's
  `h_resizable`/`v_resizable`: their state is in pixels, set at the first frame, so a stored size could not be
  given back to a window of another size. A divider stops at 160 px for either group; a window too small for the
  groups shares what there is in their proportions (nothing is hidden).
- **How deep**: any tree, at most four groups (`MAX_GROUPS`: a 2 × 2 grid already). A split along its parent's
  axis becomes a sibling (three columns, not nested pairs); a split left with one child folds away. A fifth
  group is refused in words: the tab's menu greys Split right / Split down with "four groups at most: close the
  tabs of one first", the commands say it in a notification, and a drag over an edge only offers "Move to this
  group". A tab alone in its group is not split from it ("the tab is alone in its group"): the group would
  only move; dragged to an edge of its own group it shows no indicator.
- **The focused group** is the last one clicked (a capture-phase mouse down on the group, before the click does
  anything, which also gives the group's tab the keyboard: a scrollbar or a greyed button keeps the click and takes
  no keyboard, and the keyboard left in another group would have taken the focus back) or the one the keyboard is in (checked on each frame: Focus Next Group, a dialog closing, Tab), so
  ⌘Enter, Commit, ⌘. and ⌘W never act on a group other than the one the user types in. It is marked without a
  bar (CLAUDE.md §3 item 1): its shown tab keeps the accent line under its title, another group's shown tab has
  the line in the muted text colour. The toolbar, the status bar and the chrome's environment colour follow it;
  a production tab keeps its title tint and its frame in its own group whatever is focused (item 7). New tabs
  open in it, at the end of its strip.
- **Per group**: J10's single row, rows or side list (the setting applies to every strip), the "All open tabs"
  button (it lists the tabs of every group, group after group, so the list still reaches everything), its own
  scroll. Close others, to the left, to the right and all act on the tab's group; Close All Tabs from the
  palette closes the focused group's. Next / previous tab go round the focused group. Every group's shown tab is
  revealed (a restored tab there reads its rows), and only tabs shown in no group stop refreshing on their own;
  the administration tab's auto-refresh runs while it is shown in any group.
- **Moves**: Split Right, Split Down, Move Tab to the Next / Previous Group (with one group, Move splits right;
  with two, the menu says "Move to the other group"), Focus Next / Previous Group. Dropped on a strip, a tab goes
  before the tab it is dropped on (in its own strip that reorders, which Kerno could not do before); after the
  tabs, last.
- **Keys** (letters with ⌘⌥⇧, Ctrl+Alt+Shift on Linux; all pass K20's conflict and reserved-key tests, and none
  is a digit or a sign, for the AZERTY keyboard): L Split Right, I Split Down, M Move Tab to the Next Group, N and
  P Focus Next / Previous Group. Not D (⌘⇧D is the theme switch: the search of Settings ▸ Shortcuts for ⌘⇧D would
  list both), not J (the shortcuts page's own test records it as a free key), not K, R, W, B (Ctrl+Alt+Shift+K
  commits, +R rolls back, +W wraps, +B opens the administration on Linux), not the arrows (GNOME moves windows
  between workspaces with Ctrl+Alt+Shift+arrows).
- **Stored**: the setting `tabs.layout`, written with the tabs: the tree with each tab as its index among the
  stored tabs (table forms and the Tasks view are not stored, so a group of only those does not come back) and
  each group's shown tab; the focused group is the one of `tabs.active`. It is read off the UI thread with the
  tabs (`Startup`); none (before K19) or one that does not parse is one group, and a layout that no longer fits
  is mended, never refused (`Layout::restore`: unknown and repeated tabs dropped, empty groups closed, the tabs
  it does not place added to the first group, nonsense shares made even, past four groups one group). No start
  error is added (K8).

## D-093 — The first outside user's feedback: URL paste, the fetch at the end, a light grid with structure (K21; 2026-11-20)

- **Fill takes the clipboard when the field is empty** (the owner's test of the build, 2026-11-20: "if I click
  it with nothing, it does not paste what my clipboard holds, and if I paste, the rest fills without my
  clicking: what is the button for?"). One click now pastes and fills; with no URL in the clipboard it says
  "Nothing to fill from: paste or type a connection URL here (the clipboard holds none)." instead of doing
  nothing. The hint became "Paste a URL, or click Fill to use the clipboard's: it fills the fields below."
  (`ui_tests::feedback::fill_on_an_empty_field_takes_the_url_in_the_clipboard`, seen failing with the old
  return on an empty field).
- **The URL field: a hint and fill-on-paste, both.** Beside the field's label, at first: "Paste one to fill the fields below; a typed one fills on Fill or Enter." (on the label's line, so the dialog grows no taller); after a
  fill: "Filled from the URL: check the fields below." A paste fills at
  once: one edit of the field (`InputEditsEvent`) that put in more than one character, after which the field
  holds a URL with a host, or text with `://` that does not parse, whose reason the field then shows as Fill
  would (`fills_on_paste`, a pure function). Typing puts in one character per edit, so a URL typed key by key,
  partial or whole, never fills by itself (a half-typed `mysql://root@d` parses, with host `d`). The kit's
  `Paste` action is not seen from outside the input (it handles it), hence the edits event. Its placeholder is
  the engine's (`postgres://…:5432`, `mysql://…:3306`, `mariadb://…:3306`), set at open (an edited MySQL
  connection too) and on every engine change. `ConnectionForm::apply_url` is unchanged: it switches the engine
  to the URL's, replaces the fields the URL names, keeps the others (name, folder, colour, environment, a user
  or database the URL leaves out) and puts the engine's standard port when the URL has none, which may replace a
  typed port: a URL without a port means that port (libpq, MySQL), so this is kept and said in the guide.
- **The fetch at the end already existed** (the kit's `TableDelegate::load_more`, called while the visible rows
  are within 20 of the last; `Grid::load_more` asks one page; since M5, pinned by
  `scrolling_to_the_end_of_a_result_fetches_the_next_page`). What was missing was seeing it. Trigger kept as is:
  the kit's 20-row threshold, a cheap comparison in the list's layout callback; the delegate's `has_more` is
  asked only past it. One page per reach of the end: once a page is asked, `Grid::has_more` is false until its
  rows are here (`row_count >= requested`), so the end reached again meanwhile asks nothing, and when they are
  here the end has moved 1,000 rows down. The page size is at least 100 (`PAGE_SIZES`), more than a screen, so
  the loop "until the viewport is full" does not happen. DBeaver's trigger (`SpreadsheetPresentation`, the last
  row painted and `resultset.autofetch.next.segment`) is the same idea; no code ported.
- **Not by itself** while the find shows matching rows only (as before), nor while the rows are sorted in Kerno
  on an incomplete result (only where the server cannot read it again in another order, a `SHOW` for instance):
  a new page would land after the sorted rows. Fetch more and Fetch all still do.
- **A setting, as DBeaver has one** ("Auto-fetch next page", on by default): Settings ▸ Grid ▸ "Fetch the next rows
  when scrolling reaches the end of a result", on by default, `fetch_on_scroll`; found by "load more", "auto
  fetch", "next page". It gates only the delegate's `has_more`, never `Grid::has_more` (the buttons, the summary,
  `incomplete()`). The hot path is unchanged: no new per-frame work before the kit's threshold comparison; past
  it, one read of a global and a field. The 1M-row and 300-column benches were not run.
- **The footer says it**: "1,000 rows fetched, more on the server" (was "first 1,000 rows; more on the server"),
  "1,000 rows fetched, fetching more…" while a page is on its way (rows asked, statement still open), and "N rows
  so far" kept for a result whose statement let go (a script's statement after its first page).
- **A table's data keeps its pages** (§4 E1). Its pager said "Page 1  Previous  Next" in small ghost buttons at the
  far right, after Record, Value and the selection figures. Now "Page 1 of 13" ("of about 13" on an estimate,
  "Page 1" when not counted), "Previous 1,000 rows", and "Next 1,000 rows", outlined while there is a next page (before the last page of a counted table, what it
holds: "Next 50 rows");
  the footer wraps in a narrow group of a split (K19) instead of cutting the pager off.
- **The light grid.** Tokens only, light theme only; dark unchanged (WCAG ratios):

  | token (light) | before | after | on `table.background` #FFFFFF, before → after | dark, for reference |
  |---|---|---|---|---|
  | `table.even.background` (stripe) | `#FAFBFC` | `#F4F6F8` | 1.04 → 1.08 | 1.03 |
  | `table.row.border` (grid lines) | `#E8EBEF` | `#DDE1E6` (the `border` token) | 1.20 → 1.31 (1.21 on the stripe) | 1.21 |
  | `table.head.background` | `#F6F7F8` | `#EEF0F2` (the `muted.background` token) | 1.07 → 1.14 | 1.04 |
  | `table.hover.background` | `#F3F5F7` | `#EBEEF1` | 1.09 → 1.17 (1.08 on the stripe) | 1.11 |

  The stripe had to move, so hover moved below it to stay distinct. Text: foreground 16.3:1 on the stripe,
  muted 5.5:1, header text 5.2:1 on the header, Olive 5.7:1 on the stripe: AA everywhere. Ratios that match the
  dark theme's were not enough on a white field (the dark ones read because the lines are lighter than a dark
  surface); light is now above dark on every pair. The selected cell keeps its Lime wash `#F1F8D6` (brand rule):
  1.01:1 on the new stripe, so a selected cell on a striped row is told by its Olive frame (5.7:1 on the stripe)
  and its hue, which the test pins, not by its fill. The find's match tint `#DFE4D1` is 1.20:1 on the stripe
  (1.30 on white, its pinned floor) and keeps its hue and the current match's frame. `table.head` also paints the
  find bar and the ER diagram's table headers (and their SVG/PNG export): slightly darker there too.
- **Every grid is striped.** `Grid::render_tr` striped a result only when it was no table's (`self.table.is_none()`,
  since `5f38692`, whose reason was the kit's filler rows under a short result, which the grid's own stripes do
  not draw): a table's data tab, the grid a first user most likely opened, and an editable one-table result were
  plain white. The condition is gone, so results and table data look alike; one comparison less per row. The
  structure and administration tables (their own delegates) take the new lines, header and hover from the same
  tokens and stay unstriped, as before.

## D-094 — Legacy MySQL servers: one engine that adapts to the server's version (K22; 2026-11-20)

- **One engine, no "MySQL 5 (Legacy)" entry.** DBeaver has a separate legacy driver because its modern JDBC
  driver (Connector/J 8) dropped old servers; its model (`MySQLDataSource`, `MySQLUtils`, `MySQLPlanAnalyser`)
  is one, gated by `isServerVersionAtLeast`. Kerno's client library (`mysql_async` 0.37) already speaks to
  5.1: it picks `utf8_general_ci` below 5.5.3 in the handshake and supports the pre-4.1 authentication switch.
  Nothing has to be chosen before the connection exists, so the user picks "MySQL" whatever the release and
  `MysqlSession::connect` reads `VERSION()` (it always did). `Capabilities::of(version, is_mariadb)`
  (`crates/kerno-mysql/src/capabilities.rs`) names every fact that came with a release (26 of them: utf8mb4,
  `@@transaction_isolation`, read-only transactions, JSON plans, EXPLAIN ANALYZE, `PARAMETERS`, generated
  columns, JSON, check constraints, functional indexes, `VIEW_TABLE_USAGE`, RENAME COLUMN / INDEX, invisible
  columns, roles and the `mysql.user` columns, the performance_schema tables, the InnoDB lock tables…), unit
  tested at each boundary; the dialect holds it (`MysqlDialect::for_server`; `MysqlDialect::new` stays "a
  current server", for the app's offline uses), and the introspection, the DDL writer and the administration
  queries read it. The version checks scattered before (generated columns, checks, functional indexes,
  database comments, the session list) moved into it; the generated-columns gate moved from 5.7.0 to 5.7.6,
  the release that brought them. MariaDB uses the same struct with its own releases (untested below 11). A
  version string without a patch number (Aurora's `8.0.mysql_aurora.3.05.2`) is judged as the newest of its
  release (`session::capability_version`), so such a server keeps what Kerno did before K22.
- **Oldest version claimed: MySQL 5.1 (5.1.73 tested).** 5.0 lacks `information_schema.PROCESSLIST` (5.1.7),
  `EVENTS` (5.1.6) and `SHOW CREATE TRIGGER` (5.1.21), which the navigator and the session check rely on.
  Tested servers: `mysql:5.7` (5.7.44, `sha256:4bc6bc96…`), `mysql:5.6` (5.6.51, `sha256:20575ece…`),
  `mysql:5.5` (5.5.62, `sha256:12da85ab…`), all Docker's official images, amd64 only, run under emulation on
  Apple Silicon; 5.1 has no official image and no publisher we could vouch for, so `tests/fixtures/mysql51`
  builds one on `debian:bookworm-slim` (pinned by digest) from Oracle's archived generic build
  (`cdn.mysql.com/archives/mysql-5.1/mysql-5.1.73-linux-x86_64-glibc23.tar.gz`), checked against the MD5 on
  Oracle's archive page and a SHA-256 recorded on first download. The legacy services are in the compose
  profile `legacy`: `scripts/test-env.sh up` stays as it was, `up --legacy` adds them, `scripts/check.sh`
  starts them (they are up and seeded in about 10 s under Rosetta; their thirteen tests take about 2 s).
- **TLS on servers older than rustls accepts.** MySQL 5.1 and 5.5 (yaSSL) speak TLS 1.0; 5.7.44's official
  build speaks TLS 1.2 with DHE key exchanges only, which rustls does not implement; 5.6.51's OpenSSL build
  works as 8.x does. `prefer` already went on without TLS when the server offers none; it now also does when
  the handshake fails for these reasons (`connect::old_server_tls`: an incompatible peer, an unknown protocol
  version, a handshake-failure or protocol-version alert, matched on the TLS library's typed error). `prefer`
  never checks the certificate, so the plain connection is no weaker than the one it already accepted; the TLS
  of modern servers is unchanged (no lower version, no other cipher is enabled). `require` and `verify-*`
  refuse with "This server only offers an old TLS that Kerno does not accept." and the next step (prefer or
  disable on a trusted network, an SSH tunnel, a server update).
- **Pre-4.1 password hashes refused.** One captured login is enough to find such a password, and the server
  itself refuses them from 5.6.5; the client library refuses them unless told otherwise, and Kerno does not
  tell it otherwise. The error says it in words ("The account … has an old password hash that Kerno does not
  accept.") with the administrator's fix (`old_passwords = 0`, then `SET PASSWORD`).
- **Read-only before 5.6.5.** `Dialect::read_only_session_sql` returns an `Option`: `None` on a server without
  read-only transactions. The connection still opens (refusing it would forbid the safest way to look at an
  old production server); Kerno's own refusal of writes stays, and the app shows a warning on connect that the
  server would accept what Kerno does not recognise as a write, suggesting an account that may only read.
- **What an old server lacks is said, not failed.** `Dialect::explain_unavailable` gives the sentence ("A query
  plan (EXPLAIN FORMAT=JSON) needs MySQL 5.6.5 or later; this server is MySQL 5.5.62."), checked by the query
  tab before anything runs; renames the server cannot do are refused with the release they need; lock waits on
  5.1 are "not supported"; lists read only the columns the release has. A failed apply whose rollback the
  server could not complete (a MyISAM table: warning 1196) says that the statements before it stayed applied.
  The test-connection line and the status say "MySQL 5.1.73" (`ServerInfo::named`).
- **A value the server would change is not applied.** MySQL before 5.7 (and any server whose `sql_mode` is not
  strict) stores a value too long, out of range, or with a character its column cannot hold, changed, with a
  warning only (1264, 1265, 1292, 1366): the statement "changed one row" and an apply would have reported
  success while the row held something else. `run_each` treats these warnings as the statement's failure, so
  the apply rolls back and says what the server would have stored; a strict server answers with an error as
  before, and PostgreSQL's notices (five-character SQLSTATEs) never match.
- **Not done** (D-094's limits, in `docs/ROADMAP.md`): routine arguments on 5.1 (`mysql.proc.param_list`,
  readable only with SELECT on `mysql`), a plan tree from the classic `EXPLAIN` table before 5.6.5, a column
  renamed alone before 8.0.3 (needs the column's definition in `TableChange::RenameColumn`), and the tools:
  `mysqldump` 8.4 dumps 5.5 to 5.7 as it is but finds no columns in a 5.1 table, so 5.1 needs a 5.x client
  chosen as the local client.

## D-095 — The AI assistant's endpoints: saved, detected, imported; the system's trust; reasoning left out (K23; 2026-11-20)

- **Settings: no schema bump.** The settings stay one JSON value read with `serde(default)` (unknown keys
  ignored). `AiSettings` keeps `enabled`, `endpoint`, `model`, `confirmed` as they were and gains `endpoints`
  (name, base, model, the models with the labels a tool gave them), written only when not empty. `endpoint` and
  `model` are the active endpoint's, kept equal to its entry by the only functions that change them
  (`activate`, `set_model`, `save_endpoint`, `remove_endpoint`), so every request, `ai::gate` and the panel read
  what they read before. An endpoint is its base address: one per base, its key the same secret-store entry as
  before (`ai/<base>`), its confirmation the same `confirmed` entry. `Settings::sanitized` calls
  `AiSettings::adopt_legacy`: an address not in the list (the settings of before K23, or of an older Kerno since)
  becomes a saved endpoint under the base `ai::endpoint` gives it (byte for byte what the client used, never the
  new normaliser's `/v1`, so its key and confirmation are found again), named after its local server
  ("Ollama on this computer") or its host; an older Kerno's model wins for its endpoint. Removing the active
  endpoint moves the two fields to the next one or clears them, so nothing brings it back. **An older Kerno** reads `endpoint`, `model` and
  `confirmed` and works with the active endpoint; it ignores `endpoints`, and drops the list the next time it
  saves its settings; the active endpoint survives in the two fields and comes back as a saved endpoint.
- **Trust: the system's roots instead of `webpki-roots`.** ureq 3.5's `RootCerts::Specific` takes full
  certificates and `webpki-roots` holds trust anchors, so the two cannot be merged. `https://` requests use the
  certificates `rustls-native-certs` loads (macOS keychains and trust settings, the Linux CA bundle), read once
  per run at the first `https://` request; `RootCerts::WebPki` only when the system gives none. The macOS
  system roots are a superset of the public authorities, and an internal authority the computer was told to
  trust is trusted: never less verification than the system's, no "accept invalid certificates". No new crate
  (`rustls-native-certs` is the drivers'), no `platform-verifier` (it would add `rustls-platform-verifier`).
  `cargo deny check` clean.
- **A refused certificate names its issuer.** On `UnknownIssuer` (ureq gives the rustls error inside an I/O
  error), `ai/tls.rs` makes one more TLS handshake to the same host and port, after the same gate, with a
  verifier that notes the server certificate's issuer (`x509-cert`, already in the tree through
  `tokio-postgres-rustls`) and refuses it: the handshake fails, nothing but the TLS greeting is sent, no key,
  5 s at most. It is the one connection besides `ai::send`. The issuer's name comes from a certificate nobody
  verified, so the message shows it as what the server claims and never as a reason to trust it (security
  review of the commit): "{host} presented a certificate this computer does not trust. It names “{issuer}” as
  its issuer, a name Kerno could not verify: whoever answers at this address chose it. If {host} belongs to
  your organisation and has an authority of its own, get that authority's certificate from its administrator,
  not from this server, and add it to the system's trust store… If you did not expect this, the connection may
  be intercepted: send nothing." Not done: the certificate's fingerprint for a check out of band, and reading
  the issuer on the very connection that failed (the second handshake resolves the host again).
- **omp's YAML: `serde-saphyr` 1.3** (MIT OR Apache-2.0, maintained, `#![forbid(unsafe_code)]`), already in
  `Cargo.lock` with the same default features through gpui-component's i18n macro; now also linked into the app.
  No `unsafe-libyaml`, and not `serde_yaml` (deprecated) or a hand parser (the file is the company kit's: flow
  sequences, quotes, comments). It parses into `serde_json::Value`, so omp's YAML and pi's JSON share one reader.
  The format, from both tools' docs: `providers.<id>.{name, baseUrl, apiKey, api, models[].{id, name, api}}`;
  only `api: openai-completions` is taken (a model with another `api` too is left out), anything else is a note
  with its reason; unknown keys are ignored; a file that cannot be parsed is a note, not a stop. omp reads
  `models.yml`, then `models.yaml`. Keys: `!command` is never run (imported without a key, said); `$NAME` and
  `${NAME}` (pi, `$$` a literal `$`) and an exact variable name (omp, and older pi) are read from the
  environment at import; a value shaped like a variable name whose variable is unset is said, not taken as the
  key (omp would send the name itself). `OPENAI_BASE_URL` / `OPENAI_API_KEY` are one more candidate (an
  `OPENAI_API_KEY` alone means `https://api.openai.com/v1`). A GUI app started from the Finder does not see the
  shell's variables: said in GUIDE by naming the variables, not worked around.
- **Where Import and Detect look** is a global, `ai_settings_view::AiSources`, that `main` sets to `HOME`, the process's
  environment and the default ports; without it (every UI test) it is no home, an empty environment and no
  port, so a test that forgets it reads no file of the developer and asks no local server. The core functions
  take the home, the environment and the ports as parameters.
- **Detect** asks `GET http://127.0.0.1:<port>/v1/models` on Ollama 11434, LM Studio 1234, llama.cpp 8080, vLLM
  8000, LiteLLM 4000, Jan 1337 (each tool's default), in parallel (scoped threads), 1.5 s each, through
  `ai::send` with built loopback addresses only: loopback by construction, never at start, never without the
  click. A port that answers with a model list, or 401/403 (a LiteLLM proxy that wants its key: "needs a key"),
  is listed; a refused port, a 404 or a body that is not a model list (another program on 8080) is not.
- **The switch covers everything that connects.** Detect and Test are refused while the assistant is off (the
  page's "Off, no request is ever made" stays true); Import and Add by hand work while off: they read files and
  what is typed.
- **Test is `GET /models` only**, timed: no tiny chat completion, since a reasoning model takes long to answer
  even "hi", and a chat body would have to appear in "What was sent". Its result fills the endpoint's model list.
  Its failures and every request's say the plain words first, then ureq's message: host not found ("Is this
  computer on the network that serves it, or on its VPN?"), refused on port N, no answer within the timeout
  (named), the certificate, 401/403 ("check the API key saved for it"), 404 ("check the address (most servers end
  in /v1). If only the model list is missing, type the model's name.").
- **Where an endpoint is** (`endpoints::reach`) decides only the warning's words, both cases asking first: "on
  your network" for 10/8, 172.16/12, 192.168/16, 100.64/10 (VPNs such as Tailscale), 169.254/16, fc00::/7,
  fe80::/10, a single-label name, a name under `home.arpa`, or a name with a label `lan`, `local`, `internal`,
  `intranet` or `localdomain` anywhere (a company proxy is often `host.lan.company.tld`); "on the internet"
  otherwise. No DNS lookup to decide.
- **Reasoning is dropped, not folded.** A separate `reasoning_content` / `reasoning` field was never read and
  still is not; `ai::answer_text` takes `<think>…</think>` out of the content, the text up to a lone `</think>`
  (a template that wrote the opening tag: Qwen3, DeepSeek-R1), and what follows a `<think>` never closed (cut
  short); the panel, Copy and Insert get the answer only, and SQL inside the reasoning can never be inserted. An
  answer that is only reasoning says "the model gave its reasoning but no answer (it may have run out of tokens)".
  `ponytail:` no "show the reasoning" fold; add one if a user asks to read it.
- **Timeouts**: a chat request 5 minutes (`CHAT_TIMEOUT`, was 60 s: the answer is not streamed, and a reasoning
  model on a LAN server thinks for minutes before its first token); List models and Test 15 s; Detect 1.5 s a
  port. Stop still closes the connection at once (K15's `Stopper`), whatever the timeout.
- **UI.** Settings → AI in four parts: what it is for (`ai_settings_view::PURPOSE`) and the switch; the endpoints (Use or
  Active, name · model, address · where, Test, Remove, the question before another host per row), the key of
  the active one, Detect / Import / Add by hand and which files and ports they read; the model (typed, or picked
  from the endpoint's list filtered by what is typed, by label or id, remembered per endpoint); What was sent.
  The panel opens with an "endpoint · model" picker (the kit's dropdown menu: endpoints, then the active one's
  models), disabled while a request is on its way; the panel reads the new endpoint's key flag when the active
  endpoint changes. The off notice of the "AI: …" commands says what turning it on gives (the panel never opens
  while off). The keymap footer of Settings is K20's and on every page: left alone.

## D-096 — A release builds on every core (the owner, 2026-11-20)

`scripts/release.sh` sets `CARGO_BUILD_JOBS` and `KERNO_JOBS` to the number of cores when they are not set, so
the three release compilations (macOS, Linux arm64, Linux x86_64) no longer run on 3 jobs: the 0.2.0 release
took 40 minutes and the owner asked for it ("lève la limite pour les releases"). Everything else keeps the cap
of `.cargo/config.toml` (`jobs = 3`) and `nice -n 10`: tests, `scripts/check.sh`, the test bundles
(`scripts/bundle-macos.sh` and `scripts/bundle-linux.sh` run by themselves), agents. The Mac is slow while a
release builds; nothing else heavy runs beside it. `CLAUDE.md` §1 still reads "jobs = 3; keep it": the
exception is the owner's to write there.

## D-097 — A new version opens a dialog at launch, once per run, until updated or skipped (K24; the owner, 2026-11-20)

The owner opened his 0.1.0 after 0.2.0 was published and saw the update only as a small text at the end of the
status bar: "next times, when there is an update we should show a popup, because this is not visible enough;
we should do as DBeaver does: a window that opens at every launch unless one clicks to skip the version."

- **When it shows.** A timed check (3 s after the window opens, then every 6 hours: D-045, unchanged) that finds
  a strictly newer version opens the dialog, unless the dialog was already shown in this run (for any version),
  or that version is the skipped one, or the status bar item is no longer about it (a download or an install
  under way). A later check of the same run, of the same version or a newer one, only updates the status bar
  item. Help ▸ Check for Updates… keeps its notification and opens no dialog: it was asked for, and its answer
  points at the item. Nothing changes when the check is off (Settings ▸ Updates, `KERNO_NO_UPDATE_CHECK`, a
  bench run, a test build), offline, or when the version is not strictly newer.
- **What it says.** "Kerno X.Y.Z is available", "You have A.B.C.", and what Update does on this copy
  (`workspace::updates::what_update_does`): for an installed `Kerno.app` or tarball prefix, that it downloads
  the release from GitHub, checks it against the release's SHA-256 checksums, replaces that bundle or those
  files and starts Kerno again, asking first if a tab holds an open transaction or pending changes; for any
  other copy (`Install::Page`), that it cannot replace itself and Update opens the release page in the browser.
- **The buttons.** Update (primary): closes the dialog and runs the status bar item's own path
  (`update_action(TabAction::Update)`), checksum, progress, errors and the quit question unchanged. Later: closes,
  stores nothing; the next run asks again. Skip this version: stores the version in the settings
  (`Settings::skipped_update`, the setting "Skip this version" of the item's menu already used: no migration);
  no later run asks about it, a newer version asks again. What's new: opens the release page in the browser and
  leaves the dialog open. Escape, the close button and a click beside the dialog are Later.
- **The item stays.** Reverses D-045's skip: a skipped version keeps its status bar item (before, the item went
  away and the timed checks kept quiet about it), so it can still be installed; its tooltip says it was skipped.
  Settings ▸ Updates shows "Kerno X.Y.Z was skipped" with "Ask again", which clears the skip.
- **Keys.** The check returns a few seconds after the window opens, when the user may be typing. The dialog
  opens at once (waiting for a pause in the typing would leave it hidden behind a timer for nothing it protects)
  and no key typed into it skips, installs or opens anything: Enter answers nothing (`on_ok` returns false); the
  keyboard goes to the dialog itself, not to a button, so Space clicks nothing and letters go nowhere; Escape is
  Later and gives the keyboard back to the editor. Tab reaches the harmless answers first (`tab_index`):
  the dialog's close button, Later, What's new, Skip, and Update last, so an indented line typed into it (Tab, a
  word, Space) closes it as Later, and installing from the keyboard takes five Tabs and Space. In the kit's own
  order (the order drawn) Tab then Space opened the release page and a third Tab reached Skip. The first version
  of this made Later the only tab stop; the review changed it: every answer must be reachable without a pointer
  (`CLAUDE.md` §3, keyboard first). A key typed as the dialog arrives is lost, never acted on.
- **Other dialogs.** It never opens over another dialog, confirmation, alert or the palette: the version waits
  in `Workspace::pending_update_offer`, and the first frame with none open (`keep_focus`) offers it again.
- **No new request.** The release notes are not fetched: "What's new" is a link the browser opens. The check is
  still the one `HEAD` request of D-045.
- **Stored**: only the skipped version, in the settings store as before. Whether the dialog was shown this run
  is `Updates::offered`, in memory.
- DBeaver's `VersionUpdateDialog` (Upgrade, More info, Close, a "don't show again for this version" checkbox) is
  the behaviour reference; no code was ported: the dialog is the kit's, three buttons and a link.

## D-098 — Settings: a side list of sections, panels of rows, option cards (K25; 2026-11-20)

The owner asked for a Settings that reads as designed, with "small card buttons" instead of lists of radio
buttons. Choices:

- **A side list instead of ten tabs.** Ten text tabs filled the 720 px dialog's width already, had no room for
  an eleventh, and the kit's dialog narrows itself to a small window, where a tab row overflows. A list at the
  left (184 px, a glyph and the name each) holds any number of sections, reads top to bottom, and is what the
  operating systems' own settings do. The dialog widened to 860 px so the page keeps about 620 px. The list
  comes after the page in the keyboard's order (`flex_row_reverse`): Tab from the search reaches the setting
  shown or found, which K13 promised. No bar marks the chosen item (`CLAUDE.md` §3, item 1): the accent's wash,
  a medium weight and the glyph in the accent.
- **Option cards are Kerno's, on Base's radio.** The kit's styled `Radio` draws its own circle beside its label
  and cannot hold a preview above it; its group has no arrow keys. `OptionCards` builds on `gpui_base::Radio`
  and `RadioGroup` (role, checked state, Space and Enter, the element ids `<group>/<index>` the earlier tests
  click) and adds a roving Tab stop and the arrows. Its frame keeps one width (2 px) whatever its state: only
  colours change, so nothing shifts. Chosen: `primary` frame on the `accent` wash with a check in the mark;
  hovered or focused: `input` frame on `list_hover`. Every pair is in the theme's contrast tests.
- **Previews are drawn, not pictures.** A miniature window, a grid, a tree: divs in the theme's colours, so
  they follow the theme and the brand without a file. The theme previews must show the light theme while the
  dark one is on: `theme::preview` reads both modes from the theme file itself, tested against it.
- **Switches for on/off rows.** A row's name at the left and its control at the right needs a control that
  stands alone; the kit's `Switch` keeps the row's id (the tests click it as they clicked the checkbox). Its
  default track (the kit's `secondary_active`) was 1.35:1 on white: `switch.background` is now a token,
  `#7A828E` light (3.96:1 on white, 3.45:1 on the muted surface) and `#646D78` dark.
- **Motion through the kit's own helpers**: `gpui_base::transition` for a card's colours, GPUI's
  `with_animation` for the check and the page, both 120 ms (`motion_tokens().duration_fast`) and both read
  `App::reduce_motion`, which the kit sets from the system at start. Only colour and opacity move; a page
  fades in from 40 %, so a frame where the clock has not moved (the tests) still shows it.
- **A height that follows the window**: up to 560 px, never more than the window less 140 px for the dialog's
  title and margins; every page has the same size, the page scrolls with the kit's scrollbar.
- Left as they were: the chips of the tab width and the formatter (a segmented choice whose effect the sample
  shows), the Shortcuts and Local clients views inside their pages, the AI page's logic. Short pages (Updates,
  Confirmations) end above white space beside a full side list; nothing was invented to fill them.

## D-099 — A backup or a restore opens simple; the wizard is "Advanced settings…" (K26; the owner, 2026-11-20)

The owner found the backup wizard had too many things: a simple mode by default, the present wizard as
"Advanced settings". Choices:

- **One dialog, two views over one state.** `BackupDialog::view` (`Simple` / `Advanced`); the simple view
  (`backup_dialog/simple.rs`) only sets fields the wizard has (the folder, the file name pattern, the format)
  and runs through the same `command`, `output_path`, `start`, `confirm_restore` and progress page. What it
  shows and runs is what the wizard would with the same choices: no second copy of the logic.
- **The defaults are the wizard's, unchanged** (`BackupOptions::new`, `RestoreOptions::new`). PostgreSQL: the
  custom archive (DBeaver's default; compressed; pg_restore restores all of it or only some tables), with the
  plain SQL script as the one alternative; Directory and Tar stay in the wizard. MySQL and MariaDB: a SQL
  script only, so no format choice, with the options already on (single transaction, procedures and functions,
  triggers; binary columns in hexadecimal, extended inserts); events stay off, as they always were. The view
  describes the options from the options themselves, so it never says something is done that is not. A
  restore: PostgreSQL in one transaction, nothing dropped first; MySQL and MariaDB statement by statement.
- **The file**: one path, the folder joined to the pattern (`${database}-${timestamp}`) and the format's
  extension, the same name in both views. "Choose…" is the native save panel: its folder becomes the folder
  and its name the pattern; a name ending in `.sql` or `.dump` chooses that format, the extension left to the
  format (another format later does not write `name.dump.sql`). The folder of the last backup that succeeded
  is kept (`Settings::backup_folder`) and proposed by both views next time, else the home folder as before.
- **What opens where.** A database's Back up… and Restore… (the tree, Tools ▸ on a database or on the
  connection, the main toolbar, the palette, the menu bar) open simple. Whatever carries explicit settings opens
  the wizard: Tools ▸ Back up… on a table or a schema, Create new task…, a saved task's Run and Edit; the
  global backup and Execute script have no simple view. The switch happens inside the dialog's state, one way
  (`only_tables` / `only_schemas` with objects, `set_purpose` other than a run, `apply`), so no caller changed
  and a later caller with settings cannot land in the simple view. The main toolbar's and the menu bar's Tools ▸
  now take the active tab's database without its schema: a tab's search-path schema is not a choice of what to
  back up (before, a tab on `public` backed up `public` alone).
- **The way back from the wizard** ("Simple view", offered for the backup or restore of a run only): at once
  when nothing the simple view hides differs from its defaults; otherwise a dialog names what differs (the
  objects ticked, a format it does not offer, the content, the dump tool's options, compression, encoding or
  jobs, the restore options, the client commands allowed, the extra arguments, another user) and offers "Reset
  and go back" or "Keep the advanced settings". One function lists the differences; the reset puts each back.
  The folder, the file name, an offered format, the file to restore and the client stay.
- **Nothing weaker.** The same `open` refuses a read-only connection and a route the tools cannot take; a
  restore goes through `confirm_restore` (its command shown, twice on production); the simple restore says what
  it does to the database before anything runs; a file's client commands are refused in the simple view, the
  opt-in left to the wizard's Client commands step; the client's warning and "Local client…" are in the simple
  view. Enter (the dialog's Confirm) starts a simple backup that can run, never a restore nor the wizard's
  Start. The page's height follows the window (the window less 230 px, up to 520 px) in both views: at 600 px
  the wizard's footer had been cut.
- A file that exists is replaced after one question, not two: on macOS the save panel asks, so a file chosen
  there is written without Kerno's own "Replace …" (`agreed_replace`; the review's change, the first version
  asked twice). A path typed or kept from the default name still gets Kerno's question, and so does every file
  on Linux, where the panel is the desktop's portal and may not ask.

## D-100 — PostgreSQL extensions: a manager per database, the tree lists installed ones only (K27; 2026-11-20)

The owner did not like the extensions under each database: about fifty "not installed" rows mixed with the
installed ones, nothing on what an extension is for, no choice of schema or version, no update, nothing on what
an extension requires. He approved a manager per database: a page with a search, each extension's description,
filters Installed / Available / To update, and Install, Update, Drop with the SQL shown first. Choices:

- **A tab, not a dialog.** The list is long and the user reads it while working (as the administration tab):
  a tab of the central area, one per connection and database (opening it again shows the one there), not
  restored after a restart (as the Tasks view: it holds nothing of the user's and reads the server when shown).
  It reads on a session of its own in its database, closed once read; Cmd+. and the tab closing stop the read on
  the server (K15); a statement it runs can be cancelled the same way (K2).
- **The tree keeps the installed extensions** (name and version, the count on the folder once it is opened: the
  folder stays lazy, so a database's first level reads nothing more than before). The folder's double click and
  Enter open the manager (its chevron and the arrows still open it in the tree); its menu, an installed
  extension's menu, Tools ▸ Extensions… on a PostgreSQL database (beside Foreign data wrappers…) and the palette's
  "Manage Extensions…" (which replaces "Install Selected Extension…" and keeps its id, `install_extension`, so that a keymap override naming it still binds) lead there.
- **What is shown is read from the catalog, never guessed or fetched.** One query joins
  `pg_available_extensions` with the row of `pg_available_extension_versions` for the installed version, else
  the default one (`superuser`, `trusted`, `relocatable`, `schema`, `requires`), every version the server has,
  and, for installed extensions only, the targets of `pg_extension_update_paths` from the installed version (the
  function reads script files on the server: not for fifty available ones). "Update available" means the
  installed version differs from the default one and the server has a path to it. `trusted` exists from
  PostgreSQL 13; before it the query reads `false` and `superuser` alone says who may install, which was the
  rule then (the details say so). Installed extensions read their relocatability from `pg_extension`. The
  descriptions are the control files' comments.
- **Statements**: `CREATE EXTENSION name [SCHEMA s] [VERSION 'v'] [CASCADE]` (no `IF NOT EXISTS` any more: the
  manager knows it is not installed, and the old clause hid a chosen schema or version behind a notice),
  `ALTER EXTENSION name UPDATE [TO 'v']`, `ALTER EXTENSION name SET SCHEMA s`, `DROP EXTENSION name [CASCADE]`.
  Versions are string literals, names quoted identifiers. `SCHEMA` and `VERSION` are written only when the user
  picked something other than what the server would choose (its current schema, the default version); an
  extension whose control file fixes a schema has no schema picker and no `SCHEMA`. `CASCADE` is added when a
  requirement is not installed, followed through the requirements' own (what the server installs), and the
  confirmation names them before running, in the same schema unless their control file fixes another.
- **The usual confirmation and rules**: `sql_confirm` with the connection's environment (a production
  connection asks twice); a drop lists what it reaches with K14's reader (moved to
  `navigator::view::read_dependents`, shared with the tree); a read-only connection disables every action and
  says why in a line under the toolbar and on the buttons. After a statement the list is read again and the
  tree's "Extensions" folder of that database is refreshed when it is loaded.
- **Refusals in plain words first** (K9's message after): SQLSTATE 42501 on an install is "Only a superuser can
  install …: it is not a trusted extension" when the catalog says so, else "This user may not install …: that
  takes the right to create objects in the database …"; on other statements "only its owner or a superuser
  can"; 58P01 or the server's "no installation script" / "no update path" is "The server has no files for this
  version …: install the extension's package for that version on the server's machine first".
- **Behaviour reference**: DBeaver's `PostgreCreateExtensionDialog` (a schema picker defaulting to the server's
  schema, the list with versions and descriptions) and `PostgreDatabase`'s read of `pg_available_extensions`
  were read for behaviour; no logic was ported (the catalog reads are written from the PostgreSQL docs), so
  `THIRD_PARTY_NOTICES.md` is unchanged.
- **Narrow groups** (K19): the toolbar wraps; under 640 px of the tab's width the details go under the list
  instead of beside it (the width is measured at each frame).
- Not done: extensions installed in the database whose control file has since been removed from the server are
  not listed (the read starts from `pg_available_extensions`, as before); an install's own requirements'
  "who may install" is not checked before running (the server's refusal says it).

## D-101 — Four defects of daily use: window place, dark selection, notifications, New SQL editor (K28; 2026-11-21)

- **The window's place is a setting** (`"window"` in the settings table, a `WindowPlacement`): the frame in
  logical pixels as the platform reports it, the display's UUID (stable across replugs, unlike its id), and
  Windowed / Maximized / Fullscreen. It is written 400 ms after the window stops moving or resizing (GPUI's bounds
  observer, the tabs' delay) off the UI thread, and again in the quit flush. At start it is used only when its
  display is connected and the window's top (40 px across, its title bar) is on it; otherwise the centred default
  as before. macOS reports a zoomed window as Windowed with its frame, so "maximised" comes back as that frame
  there; full screen comes back as full screen. Only the main window: the start-failure window keeps its own.
- **Dark selection as painted.** The kit paints `table.active.background` and `list.active.background` at most
  20 % opaque whatever the file says, so the dark file's opaque `#323B20` and `#2A311E` came out grey (1.02:1).
  They are now Lime written with its opacity, `#C8F03C2A` (16.5 %): painted over a row it is `#323B20`, the colour
  the earlier choice meant (1.53:1 against a row, 1.48 against a stripe), and the same tint for list items and the
  sidebar's tree. On a selected cell of a hovered row (the usual state right after a click) the muted text of NULL
  fell to 4.11:1, so dark `muted.foreground` goes one step lighter, `#9AA4B0` → `#A3ADB9` (4.57:1 there, 7.9:1 on
  the background). The other way, a fainter tint (`0x21`) with the old muted text, gave 1.37:1: too close to the
  complaint. The test measures what is painted, as the light ones already did.
- **A range takes the cell's tint**, not `selection.background`: that token is also the selected text of the
  editor and the inputs, where a brighter tint would lower the comment colour's contrast (already 4.15:1). The
  text selection is unchanged; the light range moves from `#D8F477` at 30 % to the Lime wash, the same look.
  The cell (or row, or column) the table already tints as its own selection is not tinted a second time: a
  doubled tint would bring muted text under 4.5:1 on the range's anchor.
- **Notifications at the bottom right**, as DBeaver's, 68 px above the window's bottom: the status bar (24 px), a
  tab's one-line footer (fetch buttons, the data pager) and a gap. They cover grid rows for their time on screen,
  never a toolbar. A footer wrapped on two lines in a narrow group can still be partly covered.
- **New SQL editor connects first** on whatever path it is called (⌘T, toolbar, menu, palette), through the
  sidebar's existing `pending_editor`: one place, so failures, cancelled prompts and the environment's rules
  behave as the connection's own "New SQL editor" did. A remembered connection that was deleted since gives the
  unbound tab, as nothing selected does.
- Not done: the light theme's `list.active.background` (`#F1F8D6`, opaque) is painted at 20 % too and a selected
  tree or list item hardly shows in light (pinned by the brand test as written); the record view's current column
  and the dark text selection keep `selection.background` at 30 % (faint, unchanged).

## D-102 — SQLite, the third engine family: a connection is a file (K29; 2026-11-21)

ROADMAP's SQLite item, so that someone can open a SQLite file and work in it every day. Choices taken with the
coordinator before the work, then the ones made on the way:

- **No new dependency.** `rusqlite` 0.42 with the bundled SQLite, already kerno-core's for the app's store, in a
  new crate `crates/kerno-sqlite` shaped like `kerno-mysql` (`connect`, `dialect`, `introspect`, `plan`,
  `session`, `types`). One feature is added to it, `column_decltype` (no crate: the declared type of each result
  column). The SQLite source is public domain; the logo is Simple Icons' (CC0), in `THIRD_PARTY_NOTICES.md`.
- **One owner thread per session.** rusqlite is blocking and its connection is not `Sync`: each session owns its
  connection on a thread of its own (`kerno-sqlite`), and every call is a job sent to it, never run on a Tokio
  worker or the UI thread. A statement's sink moves to that thread while it runs and comes back after; rows are
  written there and the thread waits in `ExecSink::end_row` (through the runtime's handle) when the consumer has
  asked for no more, so nothing is read ahead of demand, as the sink's contract asks. A paused result keeps its
  thread, not a pool's. `ExecSink::stop_signal` (new, kerno-core) lets `execute` notice a consumer that left
  while the thread holds the sink: the statement is then interrupted.
- **Cancel is SQLite's interrupt** (`InterruptHandle::interrupt`), from any thread; a counter as MySQL's makes a
  cancel that arrives while a statement waits for the thread apply to it. A consumer that leaves interrupts too.
- **A connection is a file.** `Engine::Sqlite` (`"sqlite"`, label "SQLite", `Engine::is_file`),
  `ConnectionConfig::file` and `ConnectParams::file` (`serde(default, skip_serializing_if = "Option::is_none")`:
  connections saved before load and write exactly as before; an export carries the path). `validate` asks for
  the file and refuses an SSH tunnel, a password command, bootstrap statements and driver options rather than
  ignoring them (an imported file could carry them). Nothing reads or stores a secret for a file: the password
  resolution, the connect, the dialog's secrets, duplicate and the save skip the store. Deleting a connection,
  or importing one over a server connection of the same id, still removes what the store holds under that id
  (a connection switched from a server to a file may have left a password there).
- **Unfilled parameters are refused.** SQLite binds a parameter nobody filled in as NULL without a word; the
  query tab fills `:name` and `?` before sending, so a parameter left in a statement (`$id`, `@id`) is refused
  in words that say how to write it.
- **Row counts are exact and cost a `count(*)`** per table when `main` is opened and per data-tab refresh:
  quick on files of ordinary size, slow on tables of tens of millions of rows (Stop interrupts it). A cap or
  `sqlite_stat1` when it exists is the way up if it shows.
- **The file is opened as it is**: never created (`READ_WRITE` or `READ_ONLY`, without `CREATE` or `URI`), so a
  mistyped path leaves no empty database behind; a first read checks it is a database; a file the user may not
  write is refused for writing (SQLite would open it read-only silently) unless the connection is read-only.
  Missing, not a database, locked, read-only: each has words of its own (`error::explain`'s file branch,
  `FailureKind::{FileMissing, NotADatabase, FileLocked, FileReadOnly}`), SQLite's message below. "New file…"
  makes an empty file (an empty file is an empty database to SQLite) with `create_new`: a path that exists is
  opened as it is and never emptied, whatever the save panel's "Replace?" said. `journal_mode` is never set.
- **Read-only** opens the file read-only at the SQLite level, and Kerno's own refusal of writes stays.
- **No schemas, one database.** `has_schemas` is false and `databases()` lists `main`, as a MySQL database: SQL
  writes `"main"."t"`, which SQLite reads as the file's own tables. Attached databases and `temp` are not shown.
  The tree under the connection: `main`, then Tables, Views and Triggers; no Routines or Events folder
  (`Dialect::has_routines`, false for SQLite; the tree drops those folders). Indexes are under each table, not
  in a folder of their own (SchemaObjects has none). Row counts in the tree are exact (`count(*)`: SQLite keeps
  no estimate unless ANALYZE ran); a view has none.
- **Types per cell.** A result column's kind comes from its declared type by SQLite's affinity rules, with
  BOOLEAN, DATE, DATETIME/TIMESTAMP, TIME and JSON picked out; that rendering is kept only when the first value
  is stored the way it expects (a date as text, a boolean as an integer, JSON as text), else the column is what
  that value holds (a date kept as a Unix time is an integer, and J6's timestamp hint shows). A column without a
  type follows its first value. Each cell is pushed in its storage class's text: integers and text as they are,
  a REAL in Rust's shortest round-trip form with `.0` when whole (SQLite's `%.15g` would not round-trip and an
  edit would miss its row), a blob as bytes in a binary column and as its `X'…'` literal elsewhere. The model
  has one kind per column: a column whose rows mix storage classes shows them by the first row's kind (a
  `BLOB` column holding text shows that text's hex). Recorded as a limit.
- **SQL that differs**: `BEGIN` (`Dialect::begin_sql`, new; `START TRANSACTION` by default, PostgreSQL `BEGIN`)
  wherever Kerno opened a transaction with `START TRANSACTION` (pending edits, CSV import, transfer, the table
  form, manual-commit tabs); `Dialect::transactional_ddl` (new; PostgreSQL and SQLite true) replaces the import's
  and the table form's checks of the engine's name. `DEFAULT` is no value in SQLite's `VALUES` or `SET`: a cell
  set to its default writes the column's declared default (`NULL` without one, which an `INTEGER PRIMARY KEY`
  numbers). Truncate is `DELETE FROM`. `EXPLAIN QUERY PLAN` is read into the plan tree (id, parent, detail; the
  query tab now joins the cells of a plan row with tabs, which one-column plans do not notice); EXPLAIN ANALYZE
  is refused before it runs, with its reason. One isolation level, serializable.
- **Structure**: read from `pragma_table_xinfo`, `pragma_index_list`/`index_info`, `pragma_foreign_key_list`
  (a key without referenced columns refers to the other table's primary key), `pragma_table_list` (WITHOUT
  ROWID) and `sqlite_schema` (DDL, triggers). An `INTEGER PRIMARY KEY` of a rowid table is the row id: never
  NULL, "auto increment". Foreign keys are named `<table>_fk<n>` (SQLite's are often unnamed). Check
  constraints are not listed. The form creates tables (`INTEGER PRIMARY KEY AUTOINCREMENT` for the numbered
  column, which must be the only key column; foreign keys to the same database; `CREATE INDEX main.i ON t`) and
  alters what SQLite's ALTER TABLE does: rename the table, rename, add and drop a column, plus create and drop an
  index. Every other change (a column's type, NULL or default, keys, comments, moving a column) is refused in
  words that say SQLite cannot do it in place and that rebuilding the table is still to come.
- **Trigger bodies stay one statement**: `LexRules::SQLITE`, and a `begin_blocks` rule (MySQL's and SQLite's
  `BEGIN … END` bodies) instead of the splitter reading MySQL from its `#` comments.
- **Locks**: in rollback-journal mode a result still being read holds a shared lock on the file, so a write from
  another tab waits five seconds (the busy timeout) and fails with "database is locked"; the error's hint says
  what holds the file in plain words.
- **Hidden for SQLite, each said where it is asked for**: the server fields of the dialog (host, port, user,
  password and its sources, URL fill, database, TLS and SSH tabs, bootstrap, driver options, local client);
  Tools ▸ (no client tools: backup, restore, scripts, maintenance, sessions, locks, tasks, copy to another
  connection; `ClientFamily::of` is now an `Option`, the backup dialog refuses to open and keeps the family it
  opened with); the administration tab and the toolbar's dashboard ("a database file has no server"); create
  database and create schema in the tree. Copying tables to or from SQLite is refused by the type mapping
  (`transfer::map_type`) until it has one of its own. Query results are not editable (no column origins);
  table data is.
- Not done (ROADMAP's SQLite item, "still to come"): backup and restore (`VACUUM INTO`, `.dump`), the engine's tools
  (ANALYZE, VACUUM, integrity check), copying tables to or from SQLite, `kerno file.db` and `sqlite://` URLs,
  attached databases and `temp`, the 12-step table rebuild, keying a keyless table on `rowid`, editing query
  results, loadable extensions, check constraints in the structure.

## D-103 — Schema compare: the diff in core, the script opened in a query tab, never run (K30; 2026-11-21)

Item 1 of `docs/ROADMAP.md` (NAV-57, CM-21, CM-59, TOOL-22). DBeaver Community's simple compare
(`org.jkiss.dbeaver.tools.compare.simple`) was read for the flow (pick objects, a report of what differs); its
migration script is in the paid editions, not looked for. No logic is ported, so `THIRD_PARTY_NOTICES.md` is
unchanged. Choices:

- **What is compared.** Tables by their `TableStructure`: columns (type, nullable, default, comment,
  auto-increment, `ON UPDATE`, invisible, and the order where the engine can move a column), primary key,
  unique, check and exclusion constraints, indexes, foreign keys, the table comment. Views, materialized views,
  functions, procedures, sequences, types and triggers by the DDL the driver prints, whitespace aside. Rows are
  never read. Left out: partitions, foreign tables, aggregates and window functions, MySQL events, a sequence owned
  by a column (it comes with its column), whatever the driver cannot print.
- **Sides.** A source (the model) and a target (what the script changes), each a saved connection with its
  database and schema; on MySQL and MariaDB the database is the schema. The dialog picks them as the copy dialog
  (J13) does: a click connects, the password is asked as usual.
- **Same family only.** `compare::Family::of` (core, `matches!`, no wildcard arm on `Engine`) puts PostgreSQL
  alone and MySQL with MariaDB; another pairing, or an engine without a family, is refused in plain words in the
  dialog before anything is read.
- **Names** compare as the engine compares them: exactly on PostgreSQL; on MySQL and MariaDB columns, indexes,
  constraints and routines without case, tables, views and triggers as written (a Linux server's
  `lower_case_table_names = 0`; a macOS server stores names in lower case anyway). A renamed object is one only
  in the source and one only in the target: the view says the compare cannot tell a rename.
- **Schema names.** Each side's session enters its schema first (`SET search_path`, on engines with schemas, as
  the query tab does), so the server prints that schema's own names unqualified on both sides; what is still
  qualified (a function's header, an index definition, MySQL's view bodies, a foreign key's table) has the source
  schema's name rewritten to the target's, a textual rewrite (a `ponytail:` comment says a name in a string
  literal is rewritten too).
- **How a table differs.** The source's structure becomes a `TableDraft` tied by name to the target's columns,
  indexes and keys, and `table_draft::changes` gives the changes the structure editor would make; the target's
  dialect writes them (`alter_table_sql`, per change when it refuses the whole). Constraints and indexes a draft
  cannot hold are compared by name and definition: a changed one is dropped and added again
  (`Dialect::add_constraint_sql` / `drop_constraint_sql`, provided with standard SQL, and the index's own
  definition). A move the engine cannot make (PostgreSQL) is no difference: there the column order is not
  compared, and an added column goes last.
- **The script** brings the target to the source in an order that works: types and sequences, constraint
  drops, new tables (their foreign keys left out), table changes, constraint and index adds, foreign keys,
  routines, views, triggers, then drops of what only the target has (triggers, views, routines, keys between
  dropped tables, tables, types). A changed view or routine goes through `replace_source_sql`; a changed trigger
  is dropped and created; a changed type or sequence is a comment with the reason and the source's definition.
  Whatever the dialect refuses is a comment that says why, never left out silently. Each block starts with a
  comment naming its object.
- **Comments cannot be escaped.** Names, connection names and server texts reach the script's comments (a
  block's heading, a reason, a definition written as a comment): one function, `compare::commented`, writes
  every `--` line of the script and the tab's heading, splitting on every line break (`\r\n`, `\n`, a lone `\r`,
  U+0085, U+2028, U+2029), so a name holding a line break never ends its comment and leaves SQL to run (found by
  the security review of the first version, which used `str::lines` and wrote headings by hand).
- **Drops are opt-in.** Tables, other objects and columns only the target has are listed; their drops are
  comments until "Also drop what only the target has" is ticked. Indexes, keys and constraints only the target
  has are dropped in any case: they hold no data, and the table would not match without it.
- **Nothing runs from the compare.** "Open script" opens the script in a new query tab bound to the target
  (its connection, database and schema), headed by which side it brings to which; it is run there, under the
  connection's rules (confirmations, `read_only`, production's second confirmation). A read-only target says so
  in the tab and the script still opens.
- **The view** is a tab (one per pair of sides, not restored after a restart): both sides and Swap, Refresh,
  Open script; filters All differences / Only in source / Only in target / Different / Same with counts, Same
  hidden at first; a search over names; per-object checkboxes with All / None over what is shown; marks
  `+ − ≠ =` with words; the differences in plain words and both definitions side by side in read-only editors
  (a table's as its dialect writes it from its structure, so the same writer is on both sides). No line-level
  diff highlight: the kit has none.
- **Loading** runs both sides at once, each on a session of its own, eight structures or DDLs at a time (the
  diagram's loop, now `erd::buffered`, shared); per-side progress, Cancel (Cmd+. too) stops both on the server;
  the compare itself runs on the blocking pool.
- **Ways in**: Server ▸ Tools ▸ Compare Schemas… and the palette (the active tab's place as the source), the
  Tools ▸ "Compare schemas…" of a schema and a database in the tree, and their "Compare with…" (both make it the
  source).
- **Not in this version** (ROADMAP item 1): another engine pairing, a SQL file against a live schema, whole
  servers, privileges and owners, saving a comparison, data compare (item 2). Also known: a column whose type
  changes under a view that reads it is refused by PostgreSQL (the view must go first, by hand); enum labels
  added to a type are not written as `ALTER TYPE … ADD VALUE`.

## D-104 — A schema as a document: DBML, Markdown, SQL, Mermaid, HTML; the PDF in K32 (K31; 2026-11-21)

The owner's request of 2026-11-21: export the schema of a database, everything or the tables picked, to PDF,
DBML or other formats, the HTML and the PDF in Kerno's style by default with a plain white one. Choices:

- **Formats.** HTML (one page to read or print), Markdown (a data dictionary), DBML (dbdiagram.io, dbdocs), SQL
  (the DDL in an order that runs) and Mermaid (the diagram's existing export). Each has its use said under its
  card.
- **What a document holds.** The server's product and version, the database and schema, the date, the counts;
  the diagram of the tables (HTML; DBML and Mermaid are diagrams themselves); per table its comment, columns
  (name, type, nullable, default or how the value is generated, `PK` / `FK` / `UQ`, comment), primary key,
  unique, check and exclusion constraints, indexes, foreign keys; views with their definition; with "Also
  routines, sequences, types and triggers", the schema's routines, sequences and types and the triggers of the
  tables ticked, with their DDL. Structure only, never a row. Never the host, the port, the user, a database
  file's path, the connection's name or a secret: SQLite's database is `main`. The title is editable and starts
  as `database.schema` (the database alone without schemas).
- **One model, the compare's.** What is read is `compare::Side` (structures of tables, DDL of the rest) through
  the compare's read, split into `listing` and `read` so the export reads only what is ticked; the writers are
  pure functions of `schema_doc` (`render`), unit-tested, no GPUI. The SQL is the compare's script against an
  empty schema: the same order (types, tables without their keys, constraints and indexes, keys, routines,
  views, triggers), the same writer of comment lines, identifiers quoted by the dialect. Every engine goes
  through the driver trait; a kind of object an engine lacks is an absent section.
- **Untrusted text, one writer per format.** HTML: `esc` on every text and attribute (`& < > " '`), nothing
  from the database in a script, style, event attribute or URL (anchors are numbered), the diagram from
  `erd::to_svg`, which escapes the same five. Markdown: names, types and definitions as inline code with a fence
  longer than any backtick run, on one line; in a table cell, text with a pipe or a backslash is escaped text
  instead (a GFM table splits on a pipe even inside code, and `\|` there is undone by a preceding backslash);
  comments as text with every punctuation that can start markup escaped (emphasis, code, links, images, HTML,
  entities, pipes, autolinks through `:`, `@` and `www.`, math), the first character of a line and the `.` or
  `)` of a leading number too; definitions in fenced blocks longer than any backtick run inside. DBML: names bare
  when they are plain words that are not DBML's own, else between double quotes with `\` and `"` escaped and on
  one line; strings between single quotes with `\` and `'` escaped, or between triple quotes when they hold
  line breaks (every `'` escaped, so no `'''` ends them); a default as a number, `true` / `false` / `null`, a
  string, or an expression between backticks, a string when it holds a backtick or a line break. The DBML
  documentation was read once for this (https://dbml.dbdiagram.io/docs). SQL: comment lines through
  `compare::commented` only. Mermaid: the diagram's rules, and a quoted name now stays on one line (a line break
  would start another statement). Each is tested with the same hostile schema and seen failing without its
  escaping.
- **HTML.** One file that asks the network for nothing: inline CSS, the diagram as inline SVG, no script (the
  table of contents is anchors), no web font (Inter and JetBrains Mono when installed, then system fonts). It
  prints: a page per table and per section, header rows repeated, the dark style printed on white with the
  light diagram and lockup.
- **Styles.** Kerno light (default), Kerno dark, Plain, as option cards drawn in each style's colours. The Kerno
  styles read their colours from the app's theme file (the one source; `include_str!` of
  `crates/kerno/assets/themes/kerno.json`: background, foreground, border, muted foreground, list head, link,
  table head), show the horizontal lockup of `assets/brand/` (ink, or reversed on dark, 32 px high, so wider
  than BRAND.md's 96 px minimum) and end with "Generated by Kerno". Plain is black on white with greys, no brand
  colour, no logo, no "generated by" line, no generator meta. A test holds every style to 4.5:1 for text, muted
  text and the accent on the page, the table headers and the diagram's headers. The reversed lockup carries its
  own Night background, a shade darker than the dark page: kept as the brand file is, not redrawn.
- **The dialog**, not a tab: the export is one action. The place is fixed by where it was opened (a schema, a
  database without schemas, a table or view, the diagram's tables, or for the palette the active diagram, else
  the active tab's place, else the browsed connection); to export another schema, open it from that schema. The
  file is chosen up front in the save panel; a name ending in another format's extension picks that format, and a
  format change renames the chosen file. The macOS panel asks before replacing a file; elsewhere the dialog says
  "This file exists: the export replaces it." and asks nothing more. The list and the export each run on a
  session of their own, off the UI thread; Cancel stops the read on the server, and the file is written as
  `<name>.part` and put in place only if the export was not cancelled meanwhile, so a cancelled export leaves no
  file. The options scroll in a short window; the file and the buttons stay in view. Read-only and production
  connections export without a question: nothing is written to the database.
- **The PDF went to K32 (D-105), on another library.** `krilla` 0.8.2 (MIT OR Apache-2.0, `cargo deny` clean on paper)
  depends on `subsetter` with its `variable-fonts` feature, which brings `skrifa` 0.42 and `write-fonts` 0.48 and,
  with them, `read-fonts` 0.39. The tree already has `skrifa` 0.40 and 0.44 and `read-fonts` 0.37 and 0.41, but
  only through `gpui-pre-wgpu` (Linux): a macOS build would compile one copy, a Linux build (a shipped target,
  J16) a third copy of `read-fonts` and `skrifa` (about 130,000 lines) plus `write-fonts` (46,000). It would add
  to `Cargo.lock`: `krilla` 0.8.2, `pdf-writer` 0.15.0, `read-fonts` 0.39.2, `skrifa` 0.42.1, `subsetter` 0.2.6,
  `tiny-skia-path` 0.12.0 (beside 0.11.4), `write-fonts` 0.48.1, `xmp-writer` 0.3.3 (`rustybuzz` 0.20.1 for its
  `simple-text` is already there). The embedded Inter and JetBrains Mono are static fonts (no `fvar` table), so
  the variable-font path buys nothing here. K32 writes it with `pdf-writer` 0.15 and `subsetter` 0.2.6 without
  `variable-fonts` instead (D-105).
- **Not in this version:** the PDF and its page size (K32, D-105); DBML enums and table groups;
  `IF NOT EXISTS` in the SQL (the dialects do not write it); a document of several schemas; saving the export as
  a task.

## D-105 — The schema document as a PDF, with pdf-writer and subsetter (K32; 2026-11-21)

K31 stopped on the PDF (D-104): `krilla` brings a third copy of `read-fonts` and `skrifa` on Linux. The
coordinator decided, under the owner's "fast and light" request (the owner can veto it): `pdf-writer` 0.15 and
`subsetter` 0.2.6 with `default-features = false`, the only two packages `Cargo.lock` gains (14,825 and
4,925 lines of source). `kurbo` 0.13.1 and `flate2` were already in the tree, through usvg and russh, and are
now direct dependencies of `kerno-core` at the version locked. No logic of krilla is ported. Choices:

- **No `ttf-parser`, no `rustybuzz`.** The brief named `ttf-parser` (or `rustybuzz`) for the glyphs and
  advances; RustSec marks both unmaintained (RUSTSEC-2026-0192 for `ttf-parser`, RUSTSEC-2026-0206 of 2026-07-11
  for `rustybuzz`, which points to `harfrust`), and `deny.toml`'s `unmaintained = "workspace"` refuses a new
  direct dependency on either (their copies under GPUI were accepted before). The agent did not add an exception
  to `deny.toml`: `schema_doc/pdf/sfnt.rs` reads the few tables needed (the table directory, `head`, `hhea`,
  `hmtx`, `cmap` formats 12 and 4, `post`, `OS/2`, `name`), about 280 lines with its tests, every read checked,
  a short or missing table an error and never a panic. Its glyphs, advances and metrics for the three fonts were
  checked against `ttf-parser`'s before it was removed. `skrifa` 0.44 (already in the lock, maintained) was the
  other way: it costs a macOS build `read-fonts` and `skrifa` (about 130,000 lines) that today compile on Linux
  only.

- **One function for every format.** `schema_doc::render` returns the file's bytes for each format and takes the
  fonts; the PDF is `schema_doc/pdf.rs`, pure, unit-tested. The fonts are a parameter: the app passes the Inter
  Regular, Inter SemiBold and JetBrains Mono Regular it already embeds (`theme::document_fonts`, one copy in the
  binary), the tests read the files by path.
- **Real text.** Each character drawn gets a CID (1, 2, …); a Type0 font over a CID font (TrueType outlines) of
  the subset `subsetter` makes, a CIDToGIDMap stream from CID to the subset's glyph, a ToUnicode map from CID to
  the character. So the text is selected, copied and searched as written, and a character the fonts lack is
  drawn as their missing glyph yet still maps back to itself. Glyphs and advances come from `sfnt.rs`: no
  kerning, no shaping, no fallback font (a `ponytail:` comment says where to go: shape with a maintained shaper,
  `harfrust`, and add a fallback font).
- **Untrusted text** reaches the file as glyph ids in strings written by `pdf-writer` (`Str`, escaped) and as the
  outline's text strings (`TextStr`, on one line); resource names, font names and colours are Kerno's own.
- **Layout.** A4 (default) or US Letter, margins of 50 pt at the sides and 56 and 62 pt at the top and bottom;
  Inter 9 pt for text, 7.5 pt in tables, JetBrains Mono for names, types, defaults and DDL. The same content and
  order as the HTML. Two passes: the first lays the document out and notes the page of each section, table, view
  and routine; the second writes those page numbers in the contents, on the same lines, so nothing moves. Each
  table starts a page (as the printed HTML), and so do the diagram and each section; a table's rows continue on
  the next page with the header row drawn again, a cell wraps between words and then between characters, a DDL
  block keeps its indentation and continues on the next page. A footer on every page: the title (cut with an
  ellipsis) and "page n of N".
- **The diagram** is drawn from `erd::layout` and `erd::route` (boxes with rounded corners, the header bands,
  rows, `PK`/`FK` marks, types right-aligned, edges with their crow's feet and bars) on a page of its own, scaled
  to fit the page's width and the height left under its heading, never enlarged: a large schema is small on paper
  but drawn as vectors, so a reader zooms into it. It does not continue on other pages.
- **The lockup** is the brand files' shapes (`kerno-horizontal.svg`, and `-reversed.svg` with its Night tile on
  the dark style), read from their path data with `kurbo`, 32 pt high (102 pt wide, above BRAND.md's minimum),
  with one disk height of clear space under it. Plain has no logo, no "Generated by Kerno", no creator.
- **Styles.** The colours are the HTML's (`Colours`), so the same contrast test holds them. Kerno dark keeps
  dark pages in the PDF (the reader chose that style; the HTML's print rules exist because a browser's print
  dialog is not a choice).
- **Outline and links.** Bookmarks: Diagram, Tables and each table, Views and each, routines and the rest and
  each; the contents' lines are links to their page. Document info: the title; the creator "Kerno" for the Kerno
  styles. No producer.
- **Size and time.** Streams compressed with `flate2`. The fixture's `shop` (7 tables, 2 views and the code)
  makes a 12-page PDF of 66 KB in Kerno light, 66 KB dark, 63 KB plain, written in 7.7 ms by a release build.
- **The dialog**: the PDF card second among six; the style cards and "Include the diagram" for HTML and PDF;
  "Paper" A4 / US Letter beside them for the PDF only; written through the `.part` then rename path as bytes.
- **Not in this version:** kerning and complex scripts, a fallback font, a diagram over several pages, a
  landscape page, tagged PDF for screen readers, PDF/A.

## D-107 — A selected row is seen at a glance in both themes (K34; 2026-11-21)

The owner, on the light theme's sidebar: "le selected se voit pas assez". The kit paints `list.active.background`
and `table.active.background` at most 20 % opaque and `selection.background` at most 30 % (`clamp_alpha` in
gpui-component's `theme/schema.rs`); the light file's Lime wash `#F1F8D6`, opaque, came out at 1.01:1 on the
sidebar, 1.02:1 on white. K28 (D-101) fixed dark only. One idea of "selected" now holds everywhere: the brand's
accent written with its opacity, so that the kit's cap paints it as meant, Olive in light and Lime in dark, and
the same tint, opaque, for the places the kit paints with `accent` (menus, the command palette, dropdowns, the
picked or current rows of Kerno's dialogs).

- **Light**, painted on the surface under it (ratio to that surface; text, muted text, link on it):
  `list.active.background` `#F1F8D6` → `#4D6B0033` (Olive at 20 %): `#D4DBC6` on the sidebar 1.33:1 (was 1.01),
  `#DBE1CC` on white 1.34:1; `table.active.background` `#B9DC3233` → `#4D6B0033`: 1.34 on a row (was 1.10), 1.33
  on a stripe, 1.31 on a hovered row; `accent.background` `#F1F8D6` → `#DBE1CC` (the list's tint on a popup):
  1.34:1 on white and popups (was 1.10), 1.25 on the sidebar; `selection.background` (text) `#D8F477` →
  `#4D6B0026` (15 %): 1.24:1 on the editor (was 1.07), the comment colour 3.95:1 on it (was 4.59).
- **The light muted text goes one step darker**, `#5B6470` → `#505965` (7.10:1 on white, was 6.00): on an Olive
  tint the old one fell to 4.21:1 on the sidebar and 3.92:1 on a selected cell of a hovered row. Now 4.99 and
  4.64. Every muted text of the light theme is a little darker; the dark theme's went one step lighter in K28
  for the same reason.
- **Dark**: the tints stay (`#C8F03C2A`, 1.49 to 1.56:1). `accent.background` `#22281D` → `#374126` (the list's
  tint on a popup): 1.55:1 on a popup (was 1.11), 1.67 on the background (was 1.19); `selection.background`
  `#465424` → `#C8F03C24` (14 %): 1.42:1 on the editor (was 1.20), the comment colour 3.52:1 on it (was 4.15).
- **The record view's current field** takes the grid's selection tint (`table_active`) instead of the text
  selection's, as K28 did for a range.
- **The floor**, 1.22:1 between a selection as painted and what it is painted on, is above every step of the
  theme meant to stay quiet (a stripe 1.08, the grid's header 1.14, a hovered row 1.16): a selected row is the
  strongest band of its list. Every old light value and the old dark accent and text selection are under it.
  Hover stays under the selection on the same surface (light list 1.02, grid 1.16; dark 1.15 and 1.11).
- **Text on it**: text and muted text 4.5:1 on every selection. Syntax colours on selected text 3:1 (WCAG's
  floor for large text: a selection is short-lived, and holding 4.5 kept it invisible).
- **Not reached**: the link colour on a light selection over a striped (4.27:1) or hovered (4.02:1) row: the one
  link-coloured mark in a grid is the `=` of a pending SQL expression, Olive on an Olive tint. In the kit's menus
  hover and selected are the same `accent` (one highlight, by the pointer or the keys: a fork to change). The
  sidebar's tree draws its selection the same with or without focus: the kit paints the tint after Kerno's row,
  so an unfocused row cannot be made weaker, and a frame shown only while the tree has focus was not done, the
  kit's tree exposing no focus handle to test. Settings ▸ Environments paints hover and selected with the same
  `accent` (Settings is another item's ground; not edited); the new connection dialog's engines too, the
  selected one keeping its Olive or Lime border.
- **Side effects, owner-disagreeable**: the kit's ghost buttons hover with `accent` (opaque in light, half in
  dark), so every toolbar button's hover is now the Olive tint in light (1.18:1 on the toolbar, was 1.03). A
  cell matching a grid's find (`GRID_MATCH_FILL`, light `#DFE4D1`) is now the colour of a selected cell in light
  (ΔE 1.6, was 10.0; 7.9 in dark): a match is a tint, the selected cell a tint with a frame, and stepping
  through the matches selects the current one, so the frame still tells it. Library and multi-run rows, which
  hovered with `accent`, hover with `list.hover.background`.
- `CLAUDE.md` §3 says selected rows use Lime wash in light: that line is out of date, the owner's word of
  2026-11-21 wins. `assets/brand/BRAND.md` updated (Olive's role, Lime wash retired).

## D-106 — Passwords in Kerno's own encrypted file by default on macOS, the keychain as a choice (K33; 2026-11-21)

The owner, with a screenshot of the macOS keychain dialog: "pourquoi on doit h24 faire le mot de passe ? On n'en
a pas besoin sur dbeaver, je veux pas qu'on demande de mot de passe, c'est chiant pour rien". Asked how to
remove the prompts, he chose **"Fichier Kerno"**: as DBeaver, passwords go into Kerno's own encrypted file,
readable by him alone, with no prompt, for him and every user; the keychain stays available in Settings. He was
told the cost: a malicious program running under his account could read them.

**Cause, established.** Kerno's builds are signed ad hoc: their designated requirement is the code's hash, which
changes with every rebuild and every update, so macOS no longer recognises the program that the keychain items
trust and asks for the login password again, once per item. There is no Developer ID. Not a Kerno bug; `CLAUDE.md` §3
item 9's "read each secret at most once per app run" only cut it to once per item and per build.

**This overrides one point of `CLAUDE.md` §1** ("Passwords … live in the OS secret store …"): the default place
on macOS. That line of `CLAUDE.md` now disagrees with the code and is the owner's to update (an agent may not
edit it). What stays absolute: never in plain text on disk, never in logs, never in crash output.

- **The file.** `secrets.kerno` in the data directory (it follows `KERNO_DATA_DIR`): one JSON map of every
  secret that goes through the store (connection passwords, SSH passwords, key passphrases, AI keys), sealed
  with AES-256-GCM (`aes-gcm` 0.11.1, already in the tree through `ssh-cipher`; no default features), a fresh
  96-bit nonce from `getrandom` (0.3.4, already in the tree) per write, the header as associated data. The key:
  256 random bits in `secrets.key` beside it. Both, and the empty `secrets.lock`, are created mode 0600 (the
  `open` sets it, never a `chmod` after); the directory is left as it is. `Cargo.lock` gains no package, only
  two edges of `kerno-core`; `cargo deny check` is clean.
- **Threat model, exactly.** The file keeps the passwords out of plain sight: a copy or backup of the secrets
  file alone, a synced folder, a file search, a shared screen, another user of the machine (0600) see nothing.
  It does not protect them from a program running as the same user, which can read the key file as well: the
  level of DBeaver's saved passwords, `~/.pgpass` and an SSH key without a passphrase. A backup that copies the
  whole data folder copies the key too. No wording in the app or the docs claims more.
- **Writes.** Each change takes an exclusive `flock` on `secrets.lock` (`std::fs::File::lock`), re-reads and
  decrypts the file, changes its one key, and replaces the file atomically: `secrets.kerno.tmp` created with
  `create_new` and 0600, written, `fsync`ed, renamed over the file, then the directory `fsync`ed (best effort;
  on macOS `sync_all` is `F_FULLFSYNC`). Nothing is cached in memory between calls, so two Kerno processes on
  one data directory (the single-instance socket usually prevents it) never erase each other's secrets: the
  simplest correct thing, a global lock, as the store is written a few times a day. A stale temporary file
  from a crash is removed under the lock.
- **Failures in plain words, never a secret, never a panic.** A secrets file that does not decrypt (changed,
  cut short, not a Kerno file, another key, a key of the wrong size, the key file missing) is never overwritten
  and no key is ever generated over it: it is renamed to `secrets.kerno.damaged` (then `secrets.kerno-2.damaged`,
  …, never over an earlier copy), with its key file, and the call fails once with "Kerno's password file could
  not be read: <why>. It was set aside as … in Kerno's data folder: enter the password again, and Kerno saves it
  in a new file." The next call starts a new file. serde's messages, which could quote decrypted text, are never
  kept. A write that fails says "… could not be written: <why>. The passwords saved before are kept."
  `error::explain` gives these their own words (what happened, what to do), not the keychain's advice. Known
  gap: the new file has never seen any key, so the next reads fall back to the system store once more; a
  password saved in the keychain before K33 comes back from there (macOS may ask again once per item), even if
  it was removed or changed in Kerno since. A fix would mark a fresh file as having nothing to fall back on.
- **Which store.** Settings ▸ Passwords ▸ "Keep saved passwords in": **Kerno's file** or the system store
  (**macOS Keychain**, **Secret Service** on Linux), with one sentence saying what each means. Default: Kerno's
  file on macOS (existing settings, which lack the field, get it); on Linux unchanged: the Secret Service, memory
  only when none runs. The choice applies from the next read without a restart (`AppState::set_secret_store`
  builds the store again; the system store's per-run cache is one process-wide instance, so switching back and
  forth reads no key twice). `KERNO_SECRET_STORE` wins over the setting: `memory` as before, `file` (Kerno's file
  alone, the keychain never touched: for checks on a scratch data folder) and `keychain` (the system store
  alone); each gives one store and moves nothing. Settings says so on its override line.
- **Migration, once per key, both ways (`ChosenStore`).** The file holds, per key, a secret or a mark "removed
  here". With the file in use, a key the file has never seen is read from the system store once and the answer
  written in the file (the secret, or the mark): macOS may ask one last time per item, at a connect, edit or
  duplicate the user asked for, and never again for it. A system store that refuses that read (Deny) counts as
  having nothing: the app asks for the password and keeps the typed one in the file. With the file in use the
  system store is never written or deleted: a delete of an item another build created may itself make the
  keychain ask, and the owner asked for no prompt. So a removed secret is a mark in the file, which keeps the
  keychain's old copy from coming back; that copy stays in the keychain, still guarded by it, until the user
  removes it in Keychain Access (the guide says so). With the system store in use the file drains: a secret
  found there (newer, written while the file was in use) is moved to the system store and leaves the file, a
  mark deletes the system store's copy, and every write removes the key from the file; switching back to the
  file then reads the system store once more and gets its latest value.
- **Nothing else reaches the keychain while the file is in use.** No probe at startup (the system store is not
  opened until a fallback read needs it), no listing, no delete. A connection set to "Ask each time" saves no
  secret (`persist` deletes them) and is no longer read anywhere at connect, edit or duplicate. An import's
  question "does this id have saved secrets?" (`has_secrets`) asks only the store in use (`SecretStore::peek`):
  a connection whose password is still only in the keychain is then shown as having none, and importing over it
  with "Ask when connecting" marks it removed; accepted, as an import over an unmigrated connection is rare.
- **Words.** The dialog's first "Password from" choice is named after the place ("Kerno's file", "macOS
  Keychain"; "Until Kerno quits" in memory), as are "Password from …" after a connect or a test, the AI key
  lines, the import's link choice ("Use the saved secrets") and the memory-only alert (which now names
  `KERNO_SECRET_STORE=memory` and the setting).
- **Not done:** no master password (it would bring the prompt back); no use of the macOS Data Protection
  keychain or of a Developer ID (none exists); the old keychain items are not deleted; non-UTF-8 secrets cannot
  exist (`Secret` wraps a `String`); the plaintext is not zeroised in memory, as no `Secret` is today.

## D-108 — A restore whose backup names a role the server does not have (K35; 2026-11-23)

The first outside user restored a production dump (custom format) into a local PostgreSQL with the simple
restore view: pg_restore stopped at `ALTER SCHEMA archive OWNER TO app_owner` ("role "app_owner" does not
exist"), in one transaction, and Kerno showed the raw output and no way on. The most common restore there is: a
production backup into a local server that has none of production's roles.

- **Read the SQL, not the words.** pg_restore and psql speak the user's language (the report: pg_restore 18 in
  French) and the server its own (`lc_messages`). A fixed message locale cannot cover both: `lc_messages` may
  be set by a superuser only, so `PGOPTIONS=-c lc_messages=C` would make a non-superuser's connect fail, and
  `LC_ALL=C` on the tool would fix only the tool's half while turning the user's log to English. The SQL is
  never translated: pg_restore prints the failed command after a translated label (`Command was:`, `La
  commande était :`), an ACL entry's whole block at once; psql, which Kerno runs without `--echo-errors` (not
  added: the command stays as it was), prints `psql:<stdin>:N:`, and line N of the script is read back.
  `tools::refused_roles` finds a tail after a colon starting `ALTER `, `GRANT `, `REVOKE ` or `SET SESSION
  AUTHORIZATION ` (pg_dump's spelling), with the block's following lines; lexes each with the PostgreSQL rules;
  takes the roles of `ALTER … OWNER TO`, `GRANT … TO`, `REVOKE … FROM`, `GRANTED BY`, `ALTER DEFAULT PRIVILEGES
  FOR ROLE|USER` and `SET SESSION AUTHORIZATION`, unquoted (`PUBLIC`, `CURRENT_USER`, `SESSION_USER`,
  `CURRENT_ROLE` left out). An ACL block names several roles and only one failed: a role counts only when the
  server's message just above (back to the tool's own `pg_restore:` / `psql:` line) names it as a whole name,
  which holds in any language, the name not being translated. The user's block, verbatim, and its English form
  are test inputs, with a French server's message.
- **"This server has no such role" is asked of the server.** The same failure naming a role that exists comes
  from a restore by a user who may not act as it (PostgreSQL 16's "must be able to SET ROLE"), and no
  language-independent text tells the two apart. Once roles are found, `pg_catalog.pg_roles` is asked on the
  connection's metadata session (the names as literals through the dialect, off the UI thread): only the roles
  it lacks are said to be missing. Otherwise (they exist, or the server could not be asked) the text says the
  server would not let the user use them, and the next step is to restore as a user allowed to.
- **What the failure view says**, above the status line, the command and the log, which stay as they were: "The
  backup names the role “app_owner” as the owner of its objects, and this server has no such role. Restore again
  without the backup's owners and privileges: what it restores then belongs to “postgres”, and the backup's
  grants are left out. Or create the role on this server, then restore again." ("in the rights it grants" for
  privileges). An archive gets **Restore without owners and privileges…**, the primary button: it sets `no_owner`
  and `no_privileges` and goes through Start, so the command is shown and asked as for any restore (twice on
  production; a read-only connection has no restore at all), `--single-transaction` kept. Only after a run in one
  transaction (Kerno's default): without it (unticked in the wizard, or jobs) pg_restore goes on past the refused
  statements and restores the rest, so a second run would meet what is there; the text then says so ("pg_restore
  went on past it: the rest is restored, and what the backup gives to it belongs to “postgres”") and offers
  nothing. A plain SQL script gets no button: "A plain SQL script sets its owners and rights in its own text, so
  psql cannot leave them out. Create the role on this server, then restore again; or back up again as a
  compressed archive, which a restore can run without them."
- **The two flags stay set** after the retry, and after its confirmation is cancelled: the wizard's two
  checkboxes and the simple view's checkbox show them, and a saved task keeps them only if saved again (a
  task's run records its status and log, not its settings). The
  button follows what the failed run had, so a cancelled confirmation leaves it offered.
- **The simple restore's checkbox**, for PostgreSQL archives only (DBeaver's two options as one): "Leave out the
  backup's owners and privileges", with one line on when to use it and who then owns what. It sets both flags
  and is checked when both are set. **Off by default**, pg_restore's own default: a restore between servers
  that share their roles (a backup of this server, a staging copy) keeps its owners and grants, and changing
  them silently would be worse than the failure, which now says what to do. Going back from the wizard does not
  count the pair as a hidden change (one of the two alone still does). A SQL file chosen in the simple view
  after it clears the pair: a script runs as it is, and pg_restore's options on it are refused.
- **MySQL and MariaDB**, looked at only (a view and a procedure with `DEFINER` naming a missing user, run by
  `mysql` 26.10): a user with `SET_ANY_DEFINER` or `SUPER` (MySQL 8.4) or `SET USER` (MariaDB 11) restores
  without an error, the objects created with the orphan definer, failing later when used (ERROR 1449). A user
  without it gets ERROR 1227 at that statement, and the restore stops there, what ran before kept. Nothing
  built.

## D-109 — A table's rows keep loading as you scroll; the size of a load is changed in place (K36; 2026-11-23)

A first user outside the owner, replacing DBeaver by Kerno 0.3.0 (the owner's words, translated): "Load more
works when you run a query, but in a table it does not: it is still a plain pagination. And the default is
1,000 per page (set in Settings), but we cannot change it on the fly in our tables as in DBeaver: there you see
the number per page at the bottom and you click it to change it." DBeaver's data viewer reads the next segment
when the scroll reaches the end (`ResultSetViewer.readNextSegment`) and holds the fetch size in a box of its
status bar. Its behaviour was read; no code was ported.

- **A load adds rows; only a read from the first row rebuilds.** A table's or a view's data tab reads one load
  (Settings' size, now "Rows fetched at a time") from the first row, then the next when the scroll comes within
  20 rows of the last one (the kit's threshold, the same as a query result, and the same switch: Settings ▸
  Grid ▸ "Fetch the next rows when scrolling reaches the end of a result"), or on **Fetch more** (the footer,
  Ctrl+Alt+N, the palette). Its rows are pushed into the grid already on screen (`Grid::took_rows`): nothing
  is rebuilt, so the selection, the scroll, the record view, the find, the columns' layout and the pending
  changes (kept by row as read; rows added are drawn after the rows read) stay as they were. Row numbers go
  on. A load is one statement on the tab's session, after what the session is doing (a count, an apply,
  distinct values); "fetching more…" and Cancel show meanwhile. A load that fails or is cancelled keeps every
  row and every pending change, says why in the footer ("The next rows could not be read. …", "Cancelled: no
  more rows were read."), and its button becomes **Try again**; the scroll does not try again by itself (a
  read that fails would loop). A table dropped since also has the tree read its schema again, as a failed read
  does.
- **The order of the loads.** A table with a key was already read in a total order: `ORDER BY` the sorted
  column, then the key (`browse::select_sql`). Its loads are `LIMIT size OFFSET <rows read>`, the offset being
  the rows read, not a page number, so the size can change between two loads. A table without a key and a
  view are read with no `ORDER BY` (an order over every column would have the server sort the whole relation
  before it sends the first row, on every first load of every view). Two reads of them by offset can repeat
  rows and skip others: on PostgreSQL an `UPDATE` moves a row to the end of the heap, a synchronized or
  parallel scan starts elsewhere; on MySQL and MariaDB the plan can change between an index and a table scan.
  So each load of such a relation reads from the first row as many rows as are on screen plus one load, in one
  statement, in place of the rows on screen: every screen is one read, no row twice, none skipped. These rows
  are read-only in Kerno (no key, or a view), so nothing pending can be lost. Costs: the rows sent grow with
  the square of the loads (100 loads of 1,000 rows send 5 million rows); Fetch all on them is one read
  without a limit, its count jumping once at the end. Known limits: if the server's order changes between two
  loads of such a relation, a row on screen can change place (the screen shows what the server says); rows
  inserted or deleted by someone else before the offset between two loads of a keyed table shift it, as any
  `OFFSET` read does (a read by key after the last row would fix it, at the price of comparing (sort, key)
  pairs with their NULLs on each engine: not done).
- **The footer.** "Rows 1–3,000 of 12,345" ("of about" for the planner's estimate, no total before the
  count); **Fetch size: 1,000** with a caret, a list of 100, 500, 1,000, 5,000 and 10,000 (the current one
  ticked) and "Other size…", a small dialog that takes what Settings takes (100 to 10,000, "2,500" with its
  comma); **Fetch 1,000 more**, outlined while the server has more (K21's lesson), "Fetch 345 more" before
  the last load of a counted table, "All rows fetched" greyed after it (a load that ends exactly on the count
  ends the way on too); **Fetch all**: load after load, each as large as what is loaded (from the size up to
  100,000 rows: few statements, `OFFSET` cost bounded), the count growing; Cancel stops it.
- **The size lasts for the tab** (owner-disagreeable). DBeaver's box writes the connection's preference
  (`RESULT_SET_MAX_ROWS`, `ResultSetViewer` status bar). In Kerno a size picked to look through one large
  table would otherwise make every later opening of that table, or of every table of the connection, read
  that much; Settings stays the default of a newly opened table and says so. Remembering it per table would be
  one more field of the J6 `TableLayout`.
- **Refresh, Apply, auto-refresh: read again as far as loaded.** They read from the first row as many rows as
  are loaded (at least one load), then put the selection and the first row in view back by position, and the
  record view (DBeaver's `refreshData` reads up to the segment of the current row; Kerno reads them all, so
  nothing under the selection goes away). After an Apply the rows are the server's: nothing shown contradicts
  it. An auto-refresh tick does the same; with many rows loaded each tick reads them all, and a tick still
  waits while a read runs. A new sort, a new filter and "Reset layout" start again from one load. Refresh, sort
  and filter still wait for pending changes to be applied or discarded. The value panel's preview of a foreign
  key's row reads it again after a refresh even though the selection stayed on the same cell.
- **Rows added.** Drawn after the rows read, as before. The scroll does not load while rows are added: the
  next load would push the row being filled in down out of view. Fetch more does load: the new rows go above
  the added ones, a selection on an added row follows it, and a cell being typed in one is kept first.
- **Keys and words.** Fetch More Rows (Ctrl+Alt+N) and Fetch All Rows (Ctrl++) were keys of the query tab; they
  are global now and go to the active table or query tab (the find bar's "Fetch all" reaches a table too).
  "Page 2 of 13", Previous and Next and their code are gone; "Go to row" past the rows read says "That row is
  not read yet: Fetch more reads the next rows." The export of a table still reads the server (one statement,
  the tab's filter and order), as do "Rows of … referencing this one".
- **Measured** on `analytics.big_table` (1,000,000 rows, read only), fetch size 10,000, release build,
  offscreen (`HeadlessAppContext` with Metal's headless renderer, 1200 × 800; no window may be opened for this
  item), the machine busy (1-minute load 9 to 50): each of the nine loads after the first took 50 to 68 ms
  from the call to the rows on screen (offsets 10,000 to 90,000: the offset's cost does not show at this size);
  the process grew from 76 MiB with 10,000 rows to 106 MiB with 100,000 (+29 MiB, about 15 bytes a cell: the
  columnar store, nothing per cell); 1,332 frames drawn while scrolling the 100,000 rows down and up, 150 rows a
  frame: 7.72 ms on average, median 7.32, 99th percentile 13.75, longest 36.05. These are the time to lay out,
  paint and render one frame offscreen, not the app's interval between frames on a display.

## D-114 — Import from DBeaver (K41; 2026-11-23)

- **Source.** DBeaver Community (Apache-2.0, ported, named in `THIRD_PARTY_NOTICES.md`): the `.dbp` layout of
  `ProjectExportWizard`, the JSON of `DataSourceSerializerModern`/`DataSourceParser`, the cipher of
  `DefaultValueEncryptor` (AES-128-CBC, IV first, `BaseProjectImpl`'s built-in key). A bare `data-sources.json`
  is read too; the pre-6.1.3 XML is refused in plain words. Checked against the source, not a real export.
- **Caps.** Read in memory, never extracted: the file 64 MB, an entry 16 MB (the read itself capped), 64 MB in
  all, 20,000 entries, a script 1 MB (skipped and listed); `..` and absolute paths ignored; only counts logged.
- **Passwords.** The credentials file is decrypted for the user names; passwords are dropped as read unless
  "Also import the saved passwords" is ticked, then read again at Import and written by `save_secrets` only.
- **Dependencies.** `aes` 0.9.4 and `cbc` 0.2.1 direct (already locked, `aes-gcm`'s cipher); `zip` no longer
  optional. No new package in `Cargo.lock`.

## D-111 — A connection's SQL scripts, in its right-click menu (K38; 2026-11-23)

The owner: when he right-clicks a connection, he wants a submenu with every SQL script linked to it, "like a
small folder". DBeaver's connection menu has SQL Editor ▸ Open SQL script / Recent SQL script, which list the
script files whose resource property names that data source (`SQLEditorUtils.findScriptList`, read for the
behaviour; nothing ported). Kerno has no scripts folder: its scripts are the saved queries and the `.sql` files
opened in tabs.

- **What "linked to a connection" is.** Its saved queries (`connection_id` = this one) and the `.sql` files
  opened, saved, or pointed at it in a tab of that connection. The queries saved for any connection are not
  listed: "Saved queries…" at the bottom of the submenu opens the saved queries view on this connection's,
  which lists them too.
- **A new list per connection, not C6's map.** `Settings::script_connections` (C6) keeps one connection per
  file, only for the ten files of the toolbar's list. A new field, `connection_scripts` (connection id → its
  files, the latest first, ten each), is written by `Settings::remember_script`, which every place that links
  a file to a connection already calls (opening, saving, `QueryTab::set_target`): no line of `query/tab.rs`
  changed. A file used with two connections is in both lists. Old settings load as before (`#[serde(default)]`);
  when the new field is empty, it starts from C6's map, each recent file under the connection it last ran on.
  The toolbar's recent list is unchanged.
- **The submenu**, "SQL scripts", beside New SQL editor: the last ten saved queries (by id: the last saved)
  grouped by folder, a folder as a submenu, then the connection's files (ten at most, so they never overflow),
  a file by its name, with its folder after a dash when another file listed has the same name (the kit's menu
  items have no tooltip). Then "All scripts…" when saved queries were left out, "Open SQL file…" (opens it on
  this connection) and "Saved queries…". With nothing, one disabled line, "No SQL scripts yet", then the last
  two.
- **The saved queries are read off the UI thread.** The menu is built while the right-click waits, so it reads
  a copy (`library::SavedQueries`, a global) read at start and after each change: a query saved, deleted in
  the library, or deleted with its connection.
- **A click opens on that connection, connecting first** through New SQL editor's path (K28, now
  `Workspace::when_connected`): its prompts, its rules, its failure in plain words, nothing opened when it
  fails. A file is read first, so a file gone since is said, leaves every list and connects nothing. **The pick
  binds**, even where C6 would only propose the connection (production, a confirm rule, read-only): the user
  chose this connection by name, as New SQL editor binds. Its environment's confirmations still apply at the
  first run.
- **Never twice.** A file already open is shown (the existing check), whatever its tab runs on. A saved query
  is open when a tab without a file on this connection has its name (the tab opened from it, even edited
  since) or its text (the tab it was saved from, which keeps its "Query N" title); a tab renamed the same is
  taken for it.
- **The keyboard.** Two palette commands on the connection selected in the sidebar: "SQL Scripts of Selected
  Connection…" opens a picker (the Open Table dialog's form: type to filter, Enter opens) with every saved
  query and file of the connection, also behind "All scripts…"; "Open SQL File on Selected Connection…".
- **The link follows the connection.** A copy has a new id and starts with no list; a deleted connection's list
  and its C6 entries are dropped with it (its saved queries already were). An export of connections (J15)
  carries configs, folders, environments and tasks, never the settings: no path of a script leaves in it.
- **Not done.** A connection opened from a URL and never saved (J4) keeps its list in the settings under its
  temporary id after it is gone; nothing shows it. The files are not checked for existence when the menu
  opens (that is I/O); a click finds out.

## D-110 — Where a running script stands, in the editor's left margin (K37; 2026-11-23)

A first user outside the owner, moving from DBeaver to Kerno 0.3.0: in DBeaver the bar on the left of the
editor shows where a long script stands, in real time; in Kerno nothing did. DBeaver (read for behaviour, no code
taken): `ScriptPositionColumn` (`ui.editors.base`) is a ruler column that every 200 ms draws an arrow beside each
line of the queries running now (`SQLEditor.getCurrentLines`), and in script mode the editor scrolls to the
running statement (`showStatementInEditor(query, false)`, `revealRange`); a failure becomes a problem marker.

- **What Kerno marks.** Every statement the run reached, in the margin left of the line numbers, the moment it
  starts and ends: running (a play sign in a halo that breathes, 1.2 s a breath), done (a tick), failed (a
  cross), cancelled (a square), refused (a barred circle: a write not sent on a read-only connection, J14).
  Nothing beside a statement not reached yet. A statement of several rows has a thin line down its rows. Each
  mark is a shape, not a colour alone; the colours are the theme's info, success, danger, muted text and
  warning, from 4.62:1 (light, on the active line) to 10.05:1 (dark) on the editor and its active line
  (`the_marks_of_a_run_read_on_the_editor_in_both_themes` holds 3:1, WCAG's floor for what is not text). Where
  two marks share a row (two statements on a line, statements inside one fold) a failure or the running one is
  painted last.
- **Where a run stopped.** The statements after a failure (without "continue on error") or a cancel stay bare:
  the cross or the square says where it stopped, and a 5,000-statement script cut at its third statement does
  not fill the margin with "not run". The task's "skipped" has no mark of its own for that reason; refused
  writes have theirs. A statement the connection cut off under the run (Disconnect, a server gone) is marked
  failed; a run stopped otherwise (a new run, another connection picked) leaves its running statement
  cancelled. The first page of a statement's rows ends its mark (done): from then on the tab is not waiting for
  the server, and later pages come when the user scrolls.
- **Words.** The status bar's "running…" of a query tab says "running statement 3 of 12 · 4.20 s" (one
  statement: "running · 1.20 s"), the time of the statement now running, going up four times a second; a
  ticker draws the tab again every 250 ms only while a statement runs. Once a run of several statements ended,
  a status bar item says the marks in words while they show: "statement 3 of 12 failed" (or cancelled, refused:
  the first of those), else "12 of 12 statements done". The marks have no tooltip: see below.
- **A single statement** run alone is marked too (running, then its end): it costs nothing and reads as the
  same thing; its tick stays until the next run or an edit, like a script's.
- **After the run** the marks stay as its trace until the next run from the editor or the first edit of the
  text, during the run too, which then marks nothing more (the status bar keeps counting). Clearing is the
  simplest correct choice: following the edits (the kit tracks ranges) would keep "done" beside a statement
  whose text was changed after it ran, and leave a mark on a line boundary for a statement deleted outright. The
  edit is seen through `InputEvent::Change`; a `set_value`, which emits none, is caught when it changes the
  text's length (the vim mode's ponytail, J19). A result read again (a filter, auto-refresh, Run again: no place
  in the text) leaves the marks alone.
- **The user's hands.** No cursor move, selection change or scroll (tested before and after a run). DBeaver
  scrolls to the running statement; making the view follow it while the user has not scrolled is a case for the
  owner, not built.
- **What the kit gave, what Kerno draws.** gpui-component 0.9.1 has gutter decorations
  (`create_gutter_decorations_collection`): a strip before the line numbers, flat bars of one colour per range,
  tracked through edits, wrapped rows and folds handled, only visible rows painted. A bar has no shape and no
  hover, so the marks are drawn by Kerno (`query/run_marks.rs`): a canvas placed after the editor in the same
  parent, whose prepaint reads the editor's layout of the same frame (`visible_row_range`, `line_height`, the
  scrolled `text_bounds`, `input_bounds`, `display_map` for the rows of a buffer line across soft wrap and
  folds), as the kit's own gutter computes its bars. The kit's collection is kept, empty, only to reserve the
  strip (12 px, plus the kit's 4 px gap): it is made with the editor, so every query editor's text sits 16 px
  further right, always; made at the first run, it would push the text aside while the user types. No fork, no
  patch of the kit. Not followed: an inline completion's ghost lines (Kerno shows none) and the overscroll
  bounce.
- **Cost.** A mark changes in O(log n) (a binary search in the statements of the run); a frame looks at the
  statements of the rows on screen only (two binary searches, then those rows). A script of 5,000 one-row
  inserts took 1,138 s before and 1,159 s after in the debug test harness (+1.8 %, one run each with other
  builds on the machine: within noise; the insert rate fell from about 12 to about 3 a second as results piled
  up, in both); the margin's layout of a frame with 5,000 marked statements, 5.4 µs. The 250 ms ticker does not
  run under the test clock: its redraws are in neither number.
- **The statement's place.** `Statement.origin` (a byte offset, dropped when parameters were bound) became
  `place`, the range of the editor's text it was read from, kept when parameters are bound: a script with
  parameters is marked too. The server's error position is still marked only when the text sent is the text
  written (`mark_error` compares them), as before.
- **Not done.** No tooltip per mark: the canvas has no hover, and a GPUI tooltip belongs to an element (one over
  the whole strip would keep the words of the mark it first showed); the shapes and the status bar say it. The
  window of a run on several connections does not say "statement k of n" (each target's tab does, when shown).

## D-113 — A table's data tab keeps its columns scrolled, its WHERE completes (K40; 2026-11-23)

Reported on 0.3.1 by the owner and a first outside user.

- **The horizontal scroll is the grid's, carried.** Every read from the first row builds a new grid; the new
  one takes the old one's horizontal offset (sort, filter, refresh, Apply, reset layout), the first row stays
  the one shown. A result read again on the server (its sort, its filter) carries it the same way (no test covers that path).
- **The WHERE field is the kit's code editor on one line.** The kit's single-line `InputState` has no
  completion provider (only `EditorState` has `lsp_mut`), so the field is an `EditorState`: no line numbers,
  no search, `submit_on_enter` (Enter applies with the list closed, takes a proposal with it open), 24 px high.
  Tab takes a proposal (J1's setting), else goes to the next control. Shift+Enter would put a line break in it,
  harmless in a condition.
- **One completion engine.** The editor's `SqlCompletion` completes the text as the end of `SELECT * FROM
  schema.table WHERE`, with a catalog of this one table, so columns come with their type and quoted as the
  engine needs; GROUP BY, ORDER BY, LIMIT and RETURNING are dropped there. `ILIKE` is now proposed after an
  operand of WHERE on PostgreSQL, in the editor too.
- **Layout per table, verified.** Order after a drag, widths, hidden and pinned columns and the sort are kept
  after closing the tab and after a restart (J6; the order now has its test). Written half a second after the
  last change, so a quit within that half second loses it.
- **The close cross.** 16 px square, rounded small, 4 px left and right of it, in the three tab layouts; not
  looked at in a capture (none taken in this batch).
- **Not done.** The "Filter the result on the server" field under a query result keeps its plain input (no
  proposals); Ctrl+Space does not open the WHERE list (the command is scoped to the query editor).

## D-112 — A restore whose archive needs an extension the server does not have (K39; 2026-11-23)

The K35 user, once the owners were left out, met the next failure of the same restore: `CREATE EXTENSION IF NOT
EXISTS pgaudit` ("extension "pgaudit" is not available"). A restore cannot install an extension's files, and
finding each missing piece by running the restore again is what had to end.

- **Asked before the run.** For a PostgreSQL archive, the `pg_restore --list` Kerno already ran for the
  database name is kept; its extensions and owners are read from it (no server) and the connection's metadata
  session is asked `pg_available_extensions` (cluster-wide) and `pg_roles`, off the UI thread. Only what the
  server lacks is said. If the list or the server cannot be read, nothing is shown and the restore runs as before.
- **Ticked by default.** A missing extension is left out unless unticked (its absence makes the restore fail
  anyway); missing owners tick K35's box, said with the reason, and the next file chosen unticks what the
  preflight ticked (D-108: owners never dropped silently). Leaving out goes through pg_restore's own
  `--use-list`: the list with the extension's `EXTENSION` and `COMMENT - EXTENSION` entries commented out,
  written as a new file (`create_new`, never through a link) in the temporary folder and removed after the run.
  The command shown and confirmed ends with "# leaves out of the archive the extension “…” (…)". What uses the
  extension may then fail: that failure is said, not hidden.
- **After a failure** the failed `CREATE EXTENSION` is read from the SQL the tool printed, as K35 (any language),
  and said only when `pg_available_extensions` lacks it (a refusal for rights names it too). One button retries
  without everything the view names (owners, privileges, extensions), through the same confirmations.
- **Owners in the list are printed bare.** An owner is matched against the whole end of the entry line, so a
  server role whose name holds a space is never taken for missing; a missing one with a space is named by its
  last word. The leave-out is not saved in a task: the check runs again at each open.
- Not done: no server here truly lacks an extension; the "not available" answer is the test server's own for
  `pgaudit`, through a stand-in pg_restore. Kerno never installs, updates or drops an extension for a restore.

## D-115 — What a newer version brings, read from its notes (K43; 2026-11-23)

- Address: `raw.githubusercontent.com/EvanPluchart/kerno/v<version>/docs/release-notes/v<version>.md`, the notes
  file of the tag (plain text, fixed once tagged), not the release's API (JSON, rate-limited, more to parse).
- Asked only when a newer version was found and checks are on (never with Settings off or `KERNO_NO_UPDATE_CHECK`),
  once per version per run, through the update's `Runner` (`curl`, HTTPS only, no redirect, 5 s, 64 KiB, the
  check's user agent). The dialog never waits: the block appears when the text arrives; any failure is silent.
- Outside text: control characters dropped, `**`, backticks and links' addresses removed, the summary and each
  lead capped, eight leads at most then "…and more". CLAUDE.md §1's sentence on the update check (one request
  to GitHub's releases page, which sends nothing else) is now short of this second request: the owner's to edit.

## D-116 — Calendar versions from 2026.11.1 (the owner, 2026-11-23)

- The owner's choice, after 0.3.1: versions are `YYYY.M.N`, the year, the month and the number of the release
  in that month, starting at 1 (2026.11.1, 2026.11.2, then 2026.12.1). The month has no leading zero: Cargo's
  versions are three plain numbers (`2027.1.1`, never `2027.01.1`).
- Nothing in the code changed for it: the update check compares three numbers (`update::Version`), so 2026.11.1
  is newer than every 0.x and is offered to them; the artifact names carry the version as written. It is one
  way: a 0.x published after it would never be offered.
- No pre-release suffix: the update check reads three numbers only.

## D-117 — A faster backup: pg_dump on several connections (K48; 2026-11-23)

After the simple restore's "Faster" (commit `80c50c4`), the owner asked for the same on the backup, then left it
to be done in autonomy for the next release.

- **What it is.** pg_dump reads several tables at once only into its directory archive (`--format=directory
  --jobs=N`): the simple backup's "Faster: back up on N connections at once" is exactly that, N the restore's
  (the machine's cores, 8 at most; `faster_jobs`, now shared by both). Off by default; shown for PostgreSQL
  beside the compressed archive only, the script being one file by one connection. Unticking gives the custom
  archive back, which is what was chosen before since the row shows only beside it; choosing the script unticks.
- **One state, no flag.** "Faster" is the format and the jobs field themselves: the directory format with the
  jobs field at N. The wizard therefore shows it in its own Format and Jobs fields and a task keeps it; the way
  back to the simple view does not call it a hidden change, and its reset keeps it. The directory format with
  other jobs, or none, is a hidden change, named and reset to the archive. The jobs field counts as a change only
  with the directory format, the only one pg_dump runs them for. The simple restore's "Faster" was called a
  hidden change by the same way back (its one-transaction default and its jobs), and its reset unticked it:
  fixed the same way (a bug of `80c50c4`, found here).
- **A folder.** The "To" row ends with a slash; the save panel's name is the folder's, as typed (an extension
  does not choose a format while ticked); the end says the folder's size (the sum of its files: pg_dump's
  directory archive is flat); Show in Finder selects it. A folder already there is refused before the run, as
  the wizard did (pg_dump's documentation says the folder must not exist); the run checks again just before
  pg_dump starts, and a folder found then is not the run's: nothing runs, nothing is removed. A failed or
  cancelled run removes only the folder it made. Left: the microseconds between that second check and pg_dump's
  own `mkdir`.
- **No cheaper compression.** zstd made the custom archive 2.5 times faster for the same size with
  pg_dump 17 (BENCH.md), but the client Kerno picks here, libpq 19.0's pg_dump, is built without zstd and lz4,
  and an archive compressed with them is readable only by a pg_restore built with them: a backup the default
  restore client of the same machine could refuse is not sure. The directory archive keeps pg_dump's default
  (gzip), which every pg_restore reads. Not built.
- **Measured.** BENCH.md: on the fixture database, whose one table holds 301 of 317 MB, "Faster" gains almost
  nothing (5.3 to 5.7 s against 5.7 to 5.9 s); on eight tables of similar size, 1.0 to 1.4 s against 3.5 to
  3.6 s. The hint says so in the view: one large table is still read by one connection.
- **MySQL and MariaDB: nothing.** mysqldump 8.4 (and the installed client) has no parallel mode; mysqlpump is
  gone from MySQL 8.4 (only the installed 5.7 client has it); `mariadb-dump --parallel` (11.8, in
  `kerno-mariadb`) works only with `--tab`, which has the server write one file per table on its own disk, not
  a script Kerno restores.
- **Not verified.** A hot standby that pg_dump cannot dump in parallel, a server with too few free connections:
  pg_dump's own message is in the output and the run ends as any failed backup (the failure path is tested
  with a stand-in pg_dump). No pass in the real app; captures were rendered offscreen (not committed).

## D-118 — A foreign key seen in the grid, followed in one gesture (K49; 2026-11-23)

The owner's request: a foreign key's column does not say it leads elsewhere, and the way there was the cell's
menu only. DBeaver's grid (`SpreadsheetPresentation.navigateLink`, `GridCellRenderer`'s link image) was read for
the behaviour; Kerno's code is its own, the jump itself is E3's, unchanged.

- **The mark is the header's.** A column of a foreign key carries the schema tree's foreign-key glyph (Link2) in
  the link colour after its name, its tooltip "→ shop.customers (id)"; a key of several columns marks each and
  names them, "(pa, pb) → s.pairs (a, b)"; a column in two keys has two lines. Worked out once each time the
  grid learns its table (`with_owner`, `set_edit_target`), never per frame. A key referenced by other tables
  gets no mark and no tooltip: the task allowed one, a second sign in every primary key's header was not
  calm, and the cell menu's "Rows of … referencing this one" stays.
- **The arrow is per cell, and only one or two at a time.** On the cell under the pointer and on the selected
  cell (never a selected row or column, which would put one on every row), when the key's values are all
  there. The cell is laid out as before without it; with it, the text is cut with an ellipsis before it and a
  number stands just left of it. Only a foreign key's cells follow the pointer (`on_hover`); the others pay
  nothing. NULL, a binary key (as the menu), a pending default or expression show none.
- **One way there.** The arrow, ⌘-click (Ctrl-click elsewhere) on the value, the key and the record view's
  arrow all run the cell menu's `GridAction::Follow` with the same number and label (`data::tab::follow_at`,
  also the value panel's): the same tab, filter and environment rules. The menu's entry now reads as the
  tooltip, "Open shop.customers where id = 42"; the tooltip adds "· ⌘-click or ⌘↓".
- **A pending value is followed**, as the menu already did: the cell shows it, the jump goes to it. Applying
  first is not asked; the referenced row may not exist yet, which the opened tab shows.
- **The arrow is its own target.** It stops the mouse down, so neither the cell nor the table starts a click:
  no selection, no row escalation, no edit; a double click opens one tab. A double click on the value still
  edits; shift and click still stretches a range; ⌘-click on a cell without a key does what it did.
- **The key: ⌘↓ (Ctrl+Down elsewhere) only.** The task proposed F4 "as DBeaver's"; DBeaver's own key for
  `core.resultset.navigateLink` is Alt+Space (`ui.editors.data` plugin.xml). Not taken: bound in the grid, it
  would also fire while a cell is being edited, where ⌥Space types a character on a Mac (and GNOME keeps
  Alt+Space for its window menu); a user can add it in the keymap file. ⌘↓ opens the selection in the Finder;
  in a cell being edited the input keeps it (a Mac) or the cell editor stops the command (Ctrl+Down on Linux).
  F4 is the editor's "Open Rows of the Table at the Cursor" in a query tab, which the conflict rule counts as
  overlapping a grid.
- **Query results**: marks and arrows where Kerno already knows the rows are one table's (the edit check of
  RES-51: PostgreSQL by the result's table OIDs, MySQL and MariaDB by the names the server sends), and the jump
  opens a table's tab (`QueryTabEvent::OpenData`); the value panel's "Open the row" works there too, which
  D-091's last sentence ruled out. A result that is not one table's (a join, a view, a key
  missing), and every result on a read-only connection (the check stops there), stays as before; SQLite
  results name no table and stay as before too. A SQLite table's data has both.
- WCAG: the link colour reaches 3:1 on every surface the marks are drawn on, the lowest 3.87:1 (light, an
  edited cell of a hovered row); `theme::tests::foreign_key_marks_meet_wcag`.

## D-119 — The AI assistant's context: nullability, keys and indexes (K50; 2026-11-23)

- **What is added**, for the tables a request is described with: each column's type and whether it can be
  NULL, the primary key, the foreign keys (columns, the table and columns they reference) and the indexes
  (their columns, unique or not), from `TableStructure`. The context holds names, types, nullability and
  key membership only; everything else of a structure is dropped by construction (`Detail::of` copies
  nothing else): defaults, comments, checks, generated columns' expressions, partial indexes' predicates,
  index definitions, row counts, each of which can hold a value.
- **An index part is sent only when it is exactly the name of one of the table's columns** (quotes and a
  trailing `ASC`/`DESC` aside). Any other part (`lower(email)`, `(data->>'kind')`, `coalesce(code, 'N/A')`)
  is sent as the fixed word `(expression)`; an index none of whose parts is a column is left out. A foreign
  key is sent only when its columns are the table's; its referenced table and columns are catalog names. A
  column's type is the engine's text for it: a MySQL `enum('a','b')` or `set(…)` carries its labels (a type's
  definition, already sent by J17's context, not a row's value; the owner's to veto).
  An automated review of the first form of this commit found expression parts sent as written; the tests
  with expression and partial indexes holding literals (core, PostgreSQL, MySQL, SQLite) fail without the
  check.
- **Where it comes from.** Types and foreign keys were already in the tab's completion catalog. Nullability,
  keys and indexes are read with `Session::table_structure` when the request opens, on the catalog's session,
  off the UI thread (4 at a time, 15 s each), each table once per panel. The navigator turns structures into
  tree nodes and keeps no `TableStructure`, and the one cache of structures (the data tab's) belongs to that
  tab: neither is reachable from a query tab without reshaping them, so they are not reused.
- **Bounds.** 30 tables read in full (`MAX_DETAILED`), the first of those chosen: the ones the text names, in
  the order it names them (a plan's or an error's statement first), else the tab's schema; per table 100
  columns, 12 foreign keys and 12 indexes, the rest counted ("… 7 more columns"). The 200 / 1,000 caps of
  D-068 stay; tables 31 to 200 keep the one-line form of names and types, which also gets the 100-column cap.
- **Form.** Under `- shop.orders (table)`, one line per column (`customer_id bigint not null`), then
  `primary key (…)`, `foreign key (…) references shop.customers (id)`, `index (…)` or `unique (…)`: a
  `CREATE TABLE`-like sketch a person reads in "What is sent…", not JSON.
- **Size**, the request of a sentence naming no table on the test server's whole `shop` schema (9 tables):
  2,530 bytes before, 3,510 after (`kerno-postgres` `ai_context::the_shop_schema_before_and_after`).
- **Waiting.** While structures are read the preview says "(reading the tables' keys and indexes…)" and Send
  waits, as for sample rows. A table that cannot be read is described by its catalog names and types.
