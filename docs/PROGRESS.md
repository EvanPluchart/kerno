# Progress log

## 2026-11-12 — Milestone 0

Environment found:

- macOS 27.0.1, Apple Silicon (11 cores, 18 GB), Docker 29.8.2, Compose v5.5.0.
- Rust default stable 1.93.1; pinned 1.99.1 for this repository (D-003). Installing it made rustup update
  itself (1.28.2 → 1.29.3), which was not intended; later installs pass `--no-self-update`.
- Metal Toolchain missing → runtime shaders (D-002).
- Other projects' containers are running on this machine (a PostgreSQL on 3603, a MySQL on 33006, and
  others). They are not Kerno's and are never connected to, inspected or stopped.
- `cargo-deny` was not installed; installed with `cargo install`. `mysql`/`mysqldump` missing (D-006).

Done and checked:

- Workspace with the four crates of the spec; `gpui-kit =0.9.1` pinned (D-001).
- Hello window builds (dev profile, 3 min 18 s cold) and renders: `docs/screenshots/m0-hello.png`, captured
  with `screencapture -l` using the window id from `scripts/window-id.swift`. App quit after the capture.
- `scripts/test-env.sh up`: four `kerno-*` containers healthy. Checked by query, not by healthcheck alone:
  - PostgreSQL 17.11: 1,000,000 rows × 20 columns in `analytics.big_table` (301 MB), 500 customers,
    200 products, 5,000 orders, 12,500 order lines, 800 reviews, 3 `all_types` rows (typical, NULLs, edge
    values such as `NaN`, `±Infinity`, `infinity` dates), 2 triggers, 4 extensions, 3 databases.
  - From the host: wrong password rejected; plain connection works; `sslmode=verify-full` works (TLS 1.3);
    `verify-ca` with the wrong CA fails with "certificate verify failed"; client-certificate login as
    `kerno_cert` works.
  - MySQL 8.4.13 and MariaDB 11.8.9: same counts (1,000,000 × 20, 500, 200, 5,000, 12,500, 800, 3),
    3 routines, 2 triggers, 1 event, 2 views, helper tables dropped, 4-byte UTF-8 stored intact (checked
    with `HEX()`), TLS available.
  - SSH bastion: key login works and reaches `postgres:5432`, `mysql:3306`, `mariadb:3306`.
- Reference clones in `~/VM/refs/` (D-005).
- `cargo fmt --check` clean, `cargo clippy --workspace --all-targets -- -D warnings` clean,
  `cargo deny check`: advisories ok, bans ok, licenses ok, sources ok (D-007).
- `.github/workflows/ci.yml` written (lint, Linux test + build with the containers, macOS test + build).
  It has not run: there is no remote, by design.

Not done in this milestone, on purpose: the Linux container build (milestone 10) and `THIRD_PARTY_NOTICES.md`
(created with the first ported logic or bundled asset).

## 2026-11-12 — Milestone 1 (in progress)

- `kerno-core`: driver and session traits, demand-driven streaming execution, compact row arena and result
  store, connection settings with URL parsing and redacted secrets, tolerant SQL lexer, statement splitting
  with `DELIMITER`, statement at cursor, introspection model, SSH tunnel, connector. 40 unit tests.
- SSH tunnel reworked after an automated security review flagged the standing loopback listener (D-008).
  10 integration tests against `kerno-ssh` pass: unknown host key refused then trusted then remembered,
  changed host key always refused, password / key file / encrypted key / agent authentication and their
  failures, stream forward, single-use port.
  - The "contested single-use port" test first passed even with the guard removed (blocking reads starved the
    forwarding task on a current-thread runtime, so it timed out silently). Rewritten with async reads; it now
    fails without the guard ("a contested connection was forwarded") and passes with it.
  - OpenSSH 10 in the bastion image penalises a source address after unauthenticated connections, which the
    tests cause on purpose; `PerSourcePenalties no` is set in the test `sshd_config` only.
- `kerno-postgres`: connection (direct or through the tunnel stream), TLS modes on rustls, streaming execute
  over the simple query protocol with column types from Parse/Describe, cancel, transaction state tracking,
  catalog introspection, DDL reconstruction, admin queries. 18 unit tests. Integration tests are next.
- References: digests of how DBeaver and Beekeeper handle PostgreSQL and MySQL were written from the clones
  (kept outside the repository). Catalog queries were written from the PostgreSQL documentation; the digests
  served as a completeness check (extension-owned objects, `conparentid`, `reltuples = -1`, `contype = 'n'`).
- `kerno-postgres` integration tests against `kerno-postgres` and `kerno-ssh`: 35 tests, all passing
  (11 connection, 15 execution, 9 introspection/DDL/admin), on top of 18 unit tests. Covered: plain, TLS in
  all five modes, wrong CA, client certificate, read-only, tunnel (password and key), TLS host-name check
  through the tunnel (`verify-full` refuses an alias the certificate lacks, `verify-ca` accepts it), cancel of
  `pg_sleep` directly and through the tunnel, abandoning a suspended million-row result, every type of
  `all_types` including NULLs and edge values, notices, error position/detail/hint, transaction state
  checked against the server, session kill, catalog listings, table structure, administrative views.
- Found by the tests and fixed:
  - the DDL round-trip (recreate each fixture table from its generated DDL, compare structures) caught the
    `serial` heuristic (D-011);
  - an "empty statement reports no row count" assumption was wrong (D-009); the unreachable fallback and
    its comment were removed.
- Core additions: query parameters (`:name`, `$n`, `?`), pending-edit SQL with atomic apply (PostgreSQL tests
  write every type of `all_types` back from its text form and check a failed apply changes nothing),
  SQLite app storage, OS secret store. 69 core unit tests.
- A second automated security review (storage commit) asked for wider redaction; done, with tests (D-012).
- `cargo deny check` now fails on two things the driver dependencies brought in: a data licence
  (allowed after reading it) and RUSTSEC-2023-0071 in `rsa`, accepted with a written reason and flagged for the
  owner (D-013).
- The MySQL/MariaDB driver is being written by a background agent confined to `crates/kerno-mysql`, with the
  PostgreSQL driver and its tests as the template. Not reviewed yet; milestone 1 stays open until it is.

## 2026-11-12 — Milestone 2 (in progress)

- App shell in `crates/kerno`: Tokio bridge, global state, settings, theme (tokens, embedded Inter and
  JetBrains Mono), command table with key bindings and keymap override file, workspace with sidebar, query
  tabs holding a SQL editor, command palette, settings dialog, native menu, status bar, rotating log file.
- 20 tests in the app crate, 8 of them through the real views in a headless window (keyboard, clicks, typed
  text): tabs, palette, settings, restore of tabs/text/active tab across a restart, stored appearance.
- What the UI tests and captures found:
  - clicking a background tab's close button also activated that tab (the click bubbled); fixed;
  - my first test helper sent several key presses in one window update, which no keyboard does; three
    apparent failures were that, plus two wrong expectations of mine (tab numbering, Escape in the palette);
  - the first "light" capture of the real app was dark: the system-appearance observer overrode the pinned
    theme; fixed with an explicit override in the app state;
  - in dark mode the editor gutter and active line were white: the theme file had no editor colours; added,
    with a contrast test;
  - the log directory was world-readable; now 0700.
- Captures of the real app: `docs/screenshots/m2-empty-light.png`, `m2-tabs-light.png`, `m2-tabs-dark.png`.
  Noted for the editor milestone: `>=` renders as a ligature, and indent guides are noisy on aligned SQL.

## 2026-11-12 — Milestone 1 closed

- MySQL/MariaDB driver delivered by the delegated agent and reviewed (D-015). Its report was checked, not
  taken on trust: scope confined to its crate, no stub or silenced lint, tests re-run here.
- Changes made during review:
  - string literals no longer depend on the session's backslash mode (hexadecimal form), with a test that
    switches the mode behind the driver's back on both engines;
  - MariaDB string defaults are normalised through the same function as MySQL's;
  - the core splitter bug the agent reported (`END$$`) is fixed, test first (D-016);
  - `default_literal` is a function, so a dialect override delegates instead of copying;
  - the `Session::cancel` documentation no longer promises an error (MySQL's `SLEEP()` returns early).
- Test run with `KERNO_REQUIRE_CONTAINERS=1` (nothing skipped), all passing:
  - `kerno-core`: 72 unit, 10 SSH tunnel;
  - `kerno-postgres`: 18 unit, 38 integration (connect 11, execute 15, introspect 9, edits 3);
  - `kerno-mysql`: 37 unit, 51 integration on both engines (connect 12, execute 25, introspect 10, edits 4).
- Open at the close of the milestone: MySQL/MariaDB `verify-ca` and the untested positive TLS paths
  (planned in milestone 3); the accepted `rsa` advisory (D-013, owner to confirm).

## 2026-11-12 — Milestone 3: connections

- Built: the `Connections` manager entity (D-017), the connection dialog (engine picker, General / SSL-TLS /
  SSH tunnel / Advanced, fill from a URL, test, save, save and connect), the sidebar list (groups, tag
  colour, status, double click to connect or disconnect, context menu: connect, edit, duplicate, delete with
  confirmation), the prompts (password, SSH password, key passphrase, SSH host key), import and export of
  connections, the `New Connection` / `Import` / `Export` commands and menu items.
- Found by the UI tests, fixed:
  - Enter in any field of the connection dialog closed it (the dialog's own confirm action fired after the
    field's); the form now refuses that, and the password prompt uses it to submit;
  - a connection saved with "save password" but with nothing in the secret store (imported, or the entry
    removed) failed with the client library's "password missing" instead of asking. Both drivers now raise
    `Error::PasswordRequired` and the manager prompts;
  - a wrong password typed at a prompt stayed in memory and was replayed on the next attempt; a typed
    secret that fails is now forgotten, and one that works is saved when the connection asks for that;
  - the keymap test overrode a command with `Cmd+N`, now the default of `New Connection`: the conflict
    report was right, the test was changed.
- MySQL / MariaDB TLS (D-018): both test containers now present the certificate signed by the test CA and
  have a certificate-only account; `verify-ca` works (it used to refuse on any name mismatch); positive and
  negative paths of `verify-ca`, `verify-full` and client certificates are tested on both engines.
- Real app (release build, scratch data directory, in-memory secret store), driven with `orca computer` in
  the background: fill from URL, test (version and round trip shown), save and connect, context menu, edit,
  SSH section. Fixed from what the captures showed: the version line was the full build banner; sections
  of different heights moved the buttons; title and engine search were cramped. Captures are taken once the
  engine logos are in (owner request of the same day, CLAUDE.md §1 "Engine logos").
- Test run with `KERNO_REQUIRE_CONTAINERS=1`, nothing skipped, all passing: app 43 (UI 13, manager 10, form
  7, others 13), `kerno-core` 73 + 10 SSH, `kerno-postgres` 18 + 38, `kerno-mysql` 38 + 52. `cargo fmt
  --check` and `cargo clippy --workspace --all-targets -- -D warnings` clean.
- Still open in this milestone: captures in `docs/screenshots/`; the client-side write block of read-only
  connections arrives with the editor (milestone 5).

## 2026-11-12 — Engine logos, milestone 3 closed

- Owner request (CLAUDE.md §1 "Engine logos"): the two-letter monograms are replaced by the engines' logos
  in the engine picker and on the sidebar connections (D-019, sources and trademark notices in
  `THIRD_PARTY_NOTICES.md`). The MySQL logo policy needs the owner's confirmation (D-019, PLAN).
- Sidebar rows: tag or environment colour as a bar on the left, then the engine logo, name and status.
- Real app, release build, driven in the background with `orca computer`, light and dark. All four test
  targets connected from the UI: PostgreSQL directly, PostgreSQL through the SSH bastion (host key
  question, SSH password, database password), MySQL, MariaDB. An unreachable server shows its error on the
  row and in a notification.
- Captures looked at one by one, in `docs/screenshots/`: `m3-dialog-general-{light,dark}`,
  `m3-dialog-{tls,ssh,advanced}-light`, `m3-password-prompt-{light,dark}`, `m3-ssh-trust-light`,
  `m3-connection-failed-light`, `m3-sidebar-connected-{light,dark}`, `m3-sidebar-menu-light`.
- Noted for the polish milestone: a "connection refused" error should say what to check; the test result
  stays on screen after the form changes; the fingerprint in the host key question is not monospaced; the
  sidebar row will be reworked when the schema tree hangs under it (milestone 4).
- App tests: 44 passing with the containers required.

## 2026-11-12 — Milestone 4: schema navigator

- Built: the schema tree under the connections (D-020): lazy loading at three levels, sessions per
  database on PostgreSQL, filter, refresh, hints (row estimate and size, column type and key, counts),
  a glyph per kind of object, the context menu of every kind of node, and `sql_confirm`, the dialog every
  statement that changes something goes through.
- The SQL of the menu (rename, drop, truncate, create, the four templates) was delegated to an agent with a
  written brief and checked afterwards (D-021). Its report also found that a PostgreSQL test of mine
  compared the exact list of databases and could fail while another test's scratch database existed;
  fixed.
- Found by the tests and by looking at the real app, fixed:
  - after a refresh every folder under the node was closed; open folders now stay open;
  - a long type pushed the column's name out of its row (`created_at` showed only `timestamp with time
    zone`); the name now keeps its room and the hint is cut;
  - a long routine name was clipped without an ellipsis;
  - folders without a chevron were 3 px off;
  - renaming to the same name was offered; it now says to type a different name;
  - my test waited for a row to disappear and asserted before the refreshed tree had arrived; it now waits
    for the tree it expects.
- Real app (release build, driven in the background): the tree on PostgreSQL and MariaDB, the filter, the
  menu, a drop confirmed and run on a scratch schema (the tree refreshed, a notification confirmed it), the
  rename dialog with its live statement, the read-only menu, the two-click confirmation on a production
  connection. Captures `docs/screenshots/m4-*.png`, light and two in dark, each looked at.
- Noted for the polish milestone: the rename field should start with its text selected; `Cmd+A` could not
  be sent by the driving tool, so that path is covered by the UI test only.

- Test run with `KERNO_REQUIRE_CONTAINERS=1`, nothing skipped, all passing: app 62, `kerno-core` 89 + 10 SSH,
  `kerno-postgres` 21 + 40, `kerno-mysql` 43 + 54 (319 in all). `cargo fmt --check`, `cargo clippy --workspace
  --all-targets -- -D warnings` and `cargo deny check` clean.

## 2026-11-12 — Milestone 5, first part: running statements, the grid

- Owner's decisions of the day applied: the official MySQL logo in colour (D-019 rewritten, notices
  updated); RUSTSEC-2023-0071 stays ignored for now and must be gone in milestone 10 (PLAN, D-013; GOAL
  criterion 1 changed by the owner).
- Built (D-022, D-023): query tabs bound to a connection/database/schema with their own session; run at
  cursor, selection or all; paging with "Fetch more" / "Fetch all"; cancel; parameters prompt; auto or
  manual commit with Commit/Rollback; messages tab; error underlined in the editor; history recording; the
  per-tab status bar (connection, database, transaction, rows, timing); the results grid with type-aware
  cells, row numbers, client-side sort, column resize/reorder/hide/pin, range selection, copy in six
  formats, and the value viewer.
- Found by the tests, fixed:
  - PostgreSQL ends a cancelled statement with an error, not a "cancelled" status: the tab now remembers
    that it asked;
  - after the parameters prompt was confirmed with Enter, the keyboard was dead until a click: focus now
    returns to the editor;
  - the grid read the table's selection while the table was drawing its cells (a panic in debug): the
    selection is mirrored in the delegate instead;
  - two of my test statements were wrong for MySQL (`empty` is reserved there).
- Real app: a statement on the test PostgreSQL, 500 rows in 7 ms, grid checked by eye (NULL, booleans,
  JSON, arrays, row numbers, type names in the headers); a syntax error underlined; the restored tab from
  the previous run came back with its text and its connection.
- Tests, `KERNO_REQUIRE_CONTAINERS=1`, nothing skipped: 426 passing (app 71, core 160 + 10 SSH + 1 doc,
  PostgreSQL driver 71, MySQL/MariaDB driver 113). fmt, clippy clean.
- Next in this milestone: read-only and destructive-statement guards (classification), EXPLAIN tree,
  history and saved-queries panels, open/save `.sql`, completion, format, find/replace.

## 2026-11-12 — Milestone 5, second part: guards, plans, library, files

- Guards before a run (statement classification from the core): a read-only connection runs nothing that
  writes, EXPLAIN ANALYZE of a write included, and says how to allow it; a statement that destroys (DROP,
  TRUNCATE, ALTER … DROP, DELETE or UPDATE without WHERE) is shown and confirmed; on a production connection
  every write is, with two clicks. The "confirmations" setting turns the first of these off, never the
  production one.
- EXPLAIN (Cmd+E) and EXPLAIN ANALYZE (Cmd+Shift+E): a tree with estimated cost and rows, real rows, own and
  total time per step, a bar for each step's share, the slowest step highlighted, planning and execution
  time, and the raw output a click away. PostgreSQL, MySQL and MariaDB.
- Query library (Cmd+Y, toolbar buttons): history per connection, searchable, with status, duration, rows
  and error; saved queries with a name and a folder; a click opens either in a tab on its connection.
- `.sql` files: open (Cmd+O), save (Cmd+S), save as; the tab is named after its file and marked when its
  text differs; a file already open is shown, not opened twice; file tabs come back after a restart.
- Found by the tests, fixed: a tab was marked unsaved as soon as the cursor moved (the editor notifies on
  every change of state; only text changes count now); a row count was shown for SET and DO ("0 rows
  affected"); the history recorded an EXPLAIN as the statement itself, with "0 rows".
- Real app: the plan of a three-table join (captured, `m5-explain-analyze-light.png`), the history dialog
  (`m5-history-light.png`). Fixed from the capture: plan details listed measurement bookkeeping before
  the conditions; they are left to the raw output now.
- App tests: 80 passing with the containers required.

