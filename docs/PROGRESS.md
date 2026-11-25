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

## 2026-11-12 — Milestone 5, third part; milestone 6; plans for what is left

- Editor: comment toggle (Cmd+/), find and replace commands, Edit menu. PostgreSQL: a paged result
  abandoned inside an open transaction is left to finish (a cancel aborted the transaction).
- Core, delegated and checked (tests re-run, lint): `sql::complete` (schema-aware completion from a
  catalog) and `sql::format_sql` (token-preserving formatter). Not wired into the editor yet.
- Table data view (milestone 6): `kerno_core::browse` and `pending`, `DataTab`, the grid's edit layer.
  Pages, server-side sort and filter, inline edits, add / duplicate / delete, Set NULL / default, review
  and atomic apply, read-only with the reason, foreign keys both ways, restore after a restart.
- Found in the real app and fixed: the kit's table never opens a row menu in cell-selection mode, so the
  copy menu of results did not exist outside tests; each cell now opens its row's menu (test with a real
  right click). New and deleted rows are tinted as whole rows; the editor selects the cell's text.
- Checked in the release app against `kerno-postgres` on a scratch schema (dropped afterwards): edit,
  delete, review, apply (verified on the server), filter, restore of a data tab, dark theme. Captures
  `m6-*.png`.
- Tests: app 91, core 212, drivers green with the containers required; clippy and fmt clean.
- Owner request: Fable budget nearly spent. Plans written in `docs/plans/` (README, M5–M10); from here the
  main session plans, reviews and commits, and subagents implement.

## 2026-11-13 — Milestones 5 to 8 largely built; context menus and DBeaver parity begun

Committed since `356bf42` (HEAD `712b7cf`, 28 commits; the plans are in `docs/plans/`):

- Milestone 5: completion catalog (one driver call per session) and schema-aware completion and Format SQL in
  the editor; `split_statements` made linear (20,000 statements: 16 s, now 22 ms); lexer fixes.
- Milestone 6: the table data view (done earlier) plus the decisions written down (D-026).
- Milestone 7: table drafts and DDL per engine, the structure tab, the table form with a live DDL preview,
  view / function / procedure source editing, PostgreSQL extensions and enums.
- Milestone 8: streamed export and CSV import (core and dialogs, three engines); backup and restore through
  the installed client tools (core only) with a safety layer for restored SQL files.
- Owner feedback of 2026-11-13: a double click or Enter opens a table's data; connection folders, drag and
  drop and the sidebar menus; DBeaver's keys on the tree and the connection list; the DBeaver parity
  inventory (418 rows) with its gaps as plan parts.
- Packaging: macOS `.app` and `.dmg`, Linux tarball, the workspace built in an Ubuntu 24.04 container
  (D-031). `cargo deny check` clean with no ignored advisory (russh without its `rsa` feature).
- Tests: 280 `kerno-core` tests pass in the Linux container (plans/M10.md). The last full run on the Mac with
  the servers, 2026-11-12, was app 91, core 212, drivers green; no later total was recorded, so none is
  claimed here. Docs: FEATURES sections A to H are filled in from the committed code and test names; parity
  summary recounted: 169 done, 9 done untested, 45 planned, 99 gap, 96 roadmap.

Found and fixed on the way:

- The kit's table opens no row menu from a cell in cell-selection mode: the menu is attached per cell
  (D-026), which also repaired the copy menu of results.
- Security review of the restore commit: `psql --file` and `mysql < file` run the client commands a file
  holds (shell, files, reconnecting elsewhere with the password). Restores now use the client's sandbox
  where its version has one and a scan of the file elsewhere; hostile files are tested not to run; a psql
  before 8.4 reads glued forms such as `\!cmd`, now refused too (D-029).
- Flaky tests made deterministic: they no longer depend on how many progress reports arrive nor on scratch
  databases created by tests running beside them. Settings changes are saved in the order they were made.
- A MySQL column edit lost `ON UPDATE CURRENT_TIMESTAMP`, `INVISIBLE`, and a PostgreSQL identity turned off
  and on became `BY DEFAULT`; a foreign key on its own table did not follow a column rename. All carried
  or fixed, with tests on the servers (D-027).
- Reliability of the suite (D-039): the PostgreSQL cancel waits for the server to act (the cancel of a
  dropped result stopped the next statement), a cancel covers a statement not yet sent on both engines, the
  sizes list leaves out a table being dropped, tests assert on their own objects only and wait for the
  statement to be on the server before cancelling. `docs/plans/README.md` has the rules. Measured: the app
  suite 20 of 20 before review, then 12 of 12, and the workspace 8 of 9, after it.
- Third visual list of `plans/M10.md` Part 4 done (D-040): the session question without a primary button,
  grid headers that keep the name whole and first widths from the content, Stop only while something runs,
  counts grouped, Properties, Open Table matches, object-filter wording, Truncate set apart, "Sessions (2
  of 6)", record-view buttons, Cmd+, with the focus nowhere, the dark danger button and selected row, the
  engine logo in the status bar. A real right-click test per surface and tests for the v1 items that had
  none (`ui_tests/menus.rs`, `ui_tests/coverage.rs`); two bugs found by them and fixed (a click on a row
  number selects the row; a cancelled import stops instead of replaying its batch). The matching bracket
  is marked (D-041) and the last two status-bar items have a menu. Workspace: 953 tests, twice green.
- Owner's test of 2026-11-13 (CLAUDE.md §3, ten items; `plans/M10.md` Parts 9 to 11). Wave 1 done
  (D-042, D-043): the sidebar is one tree (folders and connections as roots, a browse state per
  connection, states as rows under the connection), colour tags are dots, the default database comes
  first; connection failures are told in plain words with the raw message below; the connection dialog
  lists the server's databases; each secret is read from the OS store once per run and the macOS bundle
  can be signed with a stable identity. Workspace: 976 tests green.
- Owner's test, wave 2 and part of wave 3 (D-044, D-046, D-047): environments are the user's (model in
  `kerno-core/src/environment.rs`, Settings → Environments, rules enforced, the chrome takes the
  environment's colour); backup, global backup, restore and execute script are step wizards with the tools'
  real options, and local clients are detected, listed and chosen (Settings → Local clients); folders have a
  colour and an icon; the bare binary shows the app icon. Workspace: 1,030 tests green, `cargo deny` clean.
- The owner added items 11 to 13 (main toolbar, update check, no GitHub Actions): `plans/M10.md` Parts 12
  to 14.
- Owner's test, items 10 (second half), 12 and 13 (D-045, D-048, D-049): the Tools submenu per engine,
  foreign data wrappers, saved tasks and the Tasks view; the update check through the system's `curl` with
  a verified install by swap on macOS and Linux; no GitHub Actions: `scripts/check.sh` and
  `scripts/release.sh`.
- Owner's test, item 11 (D-050): the main toolbar under the title bar (DBeaver's items and order, greyed
  with the menus' reasons, shortcuts in tooltips, items hidden by right-click, overflow menu) and isolation
  levels per session on the three engines. All thirteen items of the owner's test are built. Workspace:
  1,087 tests green.
- First look at the new screens in the real app (`docs/screenshots/m10-p9-*`): eleven defects listed in
  `plans/M10.md` Part 15.

- Fourth visual list done (D-051); the toolbar's pickers follow the active tab whatever its kind
  (`1c019a0`); idle memory brought back from 170 MB to 150.5 MB bare / 148.4 MB bundled (D-053, `0bf5515`).
- `docs/BENCH.md` written from the bench on the final code (`5df5a01`). `docs/screenshots/final/` holds 98
  captures (49 screens, light and dark), each looked at (`1a53cba`, `86592f3`). `scripts/check.sh` and
  `scripts/release.sh` ran in full; the table form, the import confirmation and the view template gained the
  tests that left three rows of `FEATURES.md` partial (`4718b0f`).

## Final verification of `docs/GOAL.md` (2026-11-13, final code)

1. `cargo build --release` succeeds. `scripts/check.sh`: `cargo fmt --all --check`, `cargo clippy
   --workspace --all-targets -- -D warnings`, `cargo deny check` (no ignored advisory: `deny.toml` has no
   `ignore`), `KERNO_REQUIRE_CONTAINERS=1 cargo test --workspace -- --test-threads=3`: all passed, 1,099
   tests, 0 failed (1 ignored: a sort timing measurement).
2. `docs/FEATURES.md`: every clause of CLAUDE.md §4 has a row with its status and tests; every cited test
   exists; no row is partial; each §4 item has at least one current capture (rows about behaviour with no
   screen of its own say `no screenshot`). `docs/DBEAVER-PARITY.md`: 330 rows done, 93 on the roadmap with a
   reason, none open. A real right-click test per surface (the context-menu table of `FEATURES.md`).
3. `no_secret_is_written_to_the_data_directory_or_the_log`; drop, truncate and apply edits show their SQL
   and ask (`dropping_renaming_and_truncating_show_the_sql_and_ask`,
   `cells_are_edited_reviewed_and_applied_in_one_transaction`).
4. `a_running_statement_can_be_cancelled` on PostgreSQL, MySQL and MariaDB;
   `a_running_statement_is_cancelled_and_the_window_stays_usable`.
5. `docs/BENCH.md`: start-up 279 ms, first rows 2 to 7 ms, scroll 8.35 ms per frame, 1M × 20 in 340 MB.
   Idle RSS 148.4 MB in `Kerno.app`; 150.5 MB for the bare binary, 0.5 MB over the target, because it sets
   its Dock icon itself: both are in the document, with the reason.
6. `through_the_ssh_tunnel`, `tls_modes_without_verification`, `tls_verification`,
   `tls_through_the_tunnel_checks_the_database_host_name` (they ran, containers required).
7. `docs/screenshots/final/`: every main screen in light and dark, each looked at; what looked wrong was
   fixed and retaken (Part 15, the toolbar's pickers, neutral paths).
8. `scripts/release.sh` built `Kerno.app`, the `.dmg` and, in the Linux container, the tarball, with
   `SHA256SUMS` (verified with `shasum -c`), and printed the publish command without running it;
   `dist/Kerno.app` starts through LaunchServices as `dev.kerno.Kerno` with its icon. No `.github/`.
9. README, `CONTRIBUTING.md`, `docs/ROADMAP.md`, `docs/ARCHITECTURE.md` follow the final code.
10. `docs/PLAN.md`, this file and `docs/DECISIONS.md` (D-001 to D-053) are current; `git remote -v` is
    empty; nothing pushed; containers stopped and no Kerno left running at the end of the session.

Still open (none blocks a criterion):

- One test failed about once in forty full runs earlier in the day:
  `every_owner_of_a_session_closes_it_when_it_goes_away` saw two MySQL sessions of its own user right after
  connecting. It has not failed since the cancel fix (`a4bcda5`), in more than thirty full runs; the test
  prints those sessions and their last statements if it ever does.
- The bare binary's idle RSS is 0.5 MB over the 150 MB target (see 5 above and D-053).
- The update check has never made its real request and the install has never run for real: the repository
  does not exist and nothing is published (the owner's to do).
- A few final captures still show a sidebar folder with a test name (`kerno_ui_demo_folder`).

## 2026-11-14 — Milestone 11: v1.1, what competitors' users ask for (CLAUDE.md §4 J)

Plan: `docs/plans/M11.md` (fifteen implementation parts in three waves, each with a "Done:" note, then the
final part). Up to four agents in one working tree (D-052, D-055); the main session reviewed each part,
committed checkpoints, and ran the final checks. Decisions D-054 to D-073.

- **J1–J9** (`1f5161b`, `c52e5df`, `7a1cf9f`): Tab accepts a completion; SSH through the system OpenSSH
  (config aliases, jump hosts, agents, RSA and encrypted key files through an askpass that only answers
  OpenSSH's own question, D-056); passwords from a command, AWS RDS IAM, 1Password, `~/.pgpass` and
  `~/.my.cnf` (D-060); `kerno <url>` and one running instance (D-059); edits on rows keyed by uuid, citext,
  enum, composite and binary keys, JSON round-trips, SQL expressions as pending values (D-054); find and
  "matching rows only" over the fetched rows, the layout remembered per table, the Unix time hint (D-058);
  occurrences, folding verified, zoom, formatter options and format on save (D-057); column order on MySQL
  and MariaDB; the four benches and a reconnect that really reconnects (D-062).
- **J10–J16** (`c52e5df`, `7a1cf9f`, `cbca394`, `71d3146`): the tab list and three tab layouts, stacked
  results (D-061); XLSX export and the export/import of connections, folders, environments and tasks with
  no secret (D-065); copying tables with their data to another connection (D-063); one script on several
  connections, each with its own rules (D-064); Linux packages for x86_64 and arm64, glibc 2.31 (D-066).
- **J17–J19** (`8cde8f0`, `a207c11`): the opt-in AI assistant with one gated sender and the exact payload
  shown (D-068); the ER diagram with SVG, PNG and Mermaid export (D-069); vim mode (D-070).
- **Security reviews during the milestone**, each fixed before the next commit: the askpass could be asked
  a look-alike question by a jump host (D-055); an imported connections file could attach a stored
  password, an SSH password or a key passphrase to another server (D-065); an imported password command
  does not run until confirmed (D-060).
- **Review follow-ups and visual passes** (`d115fd7`, `b34bd15`): seven follow-ups (D-071), ten visual
  defects found on the first captures (D-072), three on the second (D-073).

## Final verification of `docs/GOAL-2.md` (2026-11-14, final code `b34bd15`)

1. **J1 to J19 work end to end** on PostgreSQL 17, MySQL 8.4 and MariaDB 11 where they apply. Each item
   has unit tests, integration tests against the `kerno-*` containers and UI tests for its surfaces
   (`docs/FEATURES.md`, section J, names every one; all cited names exist). `scripts/check.sh` sets
   `KERNO_REQUIRE_CONTAINERS=1`: no test is skipped. Nothing is behind a flag except the AI assistant and
   vim mode, both off until the user turns them on. One row of section J is `partial`: code folding works
   from the gutter and "Unfold all", and the kit offers no call to fold from the keyboard (D-057).
2. **The named tests exist and ran.** J2 through a real jump host (`kerno-ssh-jump`) with the Jump host
   field and with `ProxyJump` under an alias, and with an RSA key file (`crates/kerno-core/tests/openssh.rs`,
   15 tests). J3 with a fake `aws` on a temporary PATH and a `.pgpass` and a `.my.cnf` in a temporary HOME
   (`tests/password_sources.rs` of both drivers, `crates/kerno-core/tests/password.rs`). J17 against a fake
   OpenAI-compatible server on 127.0.0.1 (`crates/kerno-core/tests/ai.rs`, `ui_tests/ai.rs`): no connection
   while the assistant is off, the body equal to the preview byte for byte, no cell value without the
   "include sample rows" tick.
3. **`docs/BENCH.md`** has the four J9 measurements with their method, and the earlier targets
   re-measured on the final code: cold start 264 ms (target 400), first rows 3 to 6 ms (150), scroll
   8.4 ms a frame (16), 1M × 20 in 340 MB. **Idle memory is on the line**: 149.9 MB at the median in
   `Kerno.app` (149.5 to 150.4 over nine runs, target 150), 152.1 MB for the bare binary. See "Still
   open" below.
4. **`scripts/release.sh`** builds the macOS bundle and, for x86_64 and arm64, a tarball, a `.deb` and an
   AppImage, with `SHA256SUMS`; `scripts/check-linux-packages.sh` starts each Linux package in Ubuntu 20.04
   (the floor, glibc 2.31) and Debian 13: run in full on the final code, seven artifacts (the `.dmg` and six
   Linux packages), twelve starts of twelve print `kerno 0.1.0`, `shasum -c SHA256SUMS` OK for the seven.
   The floor is in the README.
5. **`scripts/check.sh`** on the final code: `cargo fmt --all --check`, `cargo clippy --workspace
   --all-targets -- -D warnings`, `cargo deny check` (no ignored advisory) and the whole suite with the
   containers: all passed, 1,402 tests, 0 failed (2 ignored: two timing measurements, slow in a debug build).
6. **Docs**: `docs/FEATURES.md` lists J1 to J19 with status, tests and captures; the `v11-*` captures of
   `docs/screenshots/final/` exist in light and dark and each was looked at (172 files in all; D-073 says which
   states have none and why); `docs/ROADMAP.md` and `docs/DECISIONS.md` no longer contradict §4 J (ERD, AI, jump
   hosts, stacked results, column order, RSA key files, AppImage and amd64 corrected); the six "Not now"
   items are in the roadmap with their reasons.
7. **README** covers the new features, SSH through OpenSSH, the password sources, the AI assistant and
   what it sends, and the Linux packages; `docs/PLAN.md`, this file and `docs/DECISIONS.md` (to D-073) are
   current; every J group has its commits (list above).
8. **Local only**: `git remote -v` is empty, no tag, nothing pushed or published. At the end no Kerno
   instance, container, watcher or browser of this session runs: `pgrep -x kerno` is empty, the `kerno-*`
   containers are stopped, the Linux build image and its cache volume are removed, the scratch objects of
   the captures and of the bench are dropped.

Still open (for the owner):

- **Idle memory has no margin.** v1.1 added about 1.6 MB (a larger binary). The app meets 150 MB at the
  median only; the bare binary is 2.1 MB over. The one large thing left is a 16.2 MB block the system
  allocator keeps after the SQL highlighter compiled its query; returning it takes one
  `malloc_zone_pressure_relief` call, a second `unsafe` in a workspace that allows one (D-042): three lines
  if the owner agrees, and idle memory would be near 134 MB.
- **Never run for real**: the update check and install (no repository), AWS RDS IAM and 1Password (a fake
  `aws` in the tests), a remote AI endpoint (a fake on 127.0.0.1), an AppImage through FUSE and a window
  opening from the Linux packages (containers have no display: the check is `kerno --version`).
- **Small gaps noted in the parts**: Fold / Fold all from the keyboard; `ciw` plus typed text undoes in two
  steps in vim mode (the kit's undo grouping is private); AI "Stop" drops the answer but the request ends
  on its own (no cancel in the HTTP client); the copy wizard has no "Save as task"; no drag to reorder
  tabs or columns (buttons and menus do it).
- The version is still 0.1.0 (D-073): nothing was published yet.

## 2026-11-16 — Repository on GitHub, README in four languages, website, release 0.1.0 (owner's request)

- New README (English) with a picture per section, an install section, the measured numbers and the privacy
  summary; the reference text moved to `docs/GUIDE.md`. `README.fr.md`, `README.es.md`, `README.ko.md`.
- `docs/release-notes/v0.1.0.md`; `scripts/release.sh` run on the release commit.
- `gh repo create EvanPluchart/kerno --private`, `main` pushed; made public the same day at the owner's
  request, GitHub Pages on `gh-pages` (<https://evanpluchart.github.io/kerno/>), release `v0.1.0` on
  `7055e8b` with the seven files and `SHA256SUMS`, downloaded again and verified (D-074).


## 2026-11-17 — The general critique of DBeaver, verified; milestone 12 proposed

- Triage of C1-C22 in `docs/COMPETITOR-ISSUES.md` (section of 2026-11-17): 3 answered, 9 gaps, 3 gaps
  (untested), 3 do not apply, 2 not now. Three audits by subagents (completion, splitting and formatter;
  safety rails, tab binding and transactions; long operations and error surfaces), every cited line opened
  again by the main session before it entered the table.
- `crates/kerno-core/tests/sql_hard_inputs.rs`: 36 tests pinning the hard inputs Kerno gets right (dollar
  quoting, `;` in every comment and string form, `DELIMITER`, statement at cursor, formatter comments,
  dollar bodies, idempotence, completion of aliased `UPDATE … FROM`, backticks, quoted mixed case).
- `docs/plans/M12.md` (proposed, awaiting the owner): K1-K10 above the cut, K11-K16 below; the proposed
  `CLAUDE.md` §4 K and `docs/GOAL-3.md` in its appendix. `docs/ROADMAP.md`: the critique's "not now" lines.
  `docs/GUIDE.md`: what the update check does behind a proxy. D-076.
- Most important gaps found: Cmd+Q drops an open transaction without a question; data, structure and
  navigator writes cannot be cancelled and wait forever behind a lock; after `USE` or `SET search_path` a tab
  runs elsewhere than its picker shows, and a reconnect silently goes back; a `.sql` file opened beside a
  production connection is bound to it; a start failure (downgrade, one bad saved row) quits without a
  window or a log.

## 2026-11-18 — K3: a query tab follows its session's database and schema (C6)

- `Session::schema()`; the PostgreSQL driver reads `current_schema()` at connect and after the statements
  that may change it or end a transaction (`touches_session_state`), never after every statement.
- A query tab moves its target (picker, status bar, stored tab) when its session moved, says so in the
  messages, and a rebuilt session opens there. Three UI tests kill the tab's backend and ask the server where
  the next session is; with the tab's follow disabled all three fail (first on the message; with only the
  reconnect check kept, the server answers `kerno` / `public`), with the driver's read disabled the
  PostgreSQL UI test and the driver test fail.
- Limits and plan errors in the "Done:" line of K3 (`docs/plans/M12.md`).


## 2026-11-18 — K4: an open transaction is visible on every tab, with its age (C15, C21)

- A query tab holding a transaction shows a dot after its title (warning, danger once failed) in the strip,
  the rows, the side list and the list of every tab; its tooltip gives the age and the keymap's shortcuts of
  Commit and Roll back; the status bar says "transaction open · 4 min". The dot follows the session's own
  state; the age (`QueryTab::tx_started`, executor clock) starts at the `BEGIN` and a failed transaction keeps
  it. A 15 s redraw timer lives only while some tab holds a transaction.
- Nine UI tests (six on PostgreSQL and MySQL) and two unit tests; the contrast test covers the dot's colours
  on the seven tab surfaces. Each UI test but two was seen failing with a part of the change disabled (the
  dot, the timer, the list's dot, the dating after `BEGIN` or after a statement, the clear when a session
  closes); the restored-tabs test is a negative check, the background-tab test a pin. Six status bar checks
  of older tests now match the start of the item ("transaction open · 12 s").
- Limits and plan errors in the "Done:" line of K4 (`docs/plans/M12.md`).

## 2026-11-18 — K5: switching to auto-commit or "Set as default" ends the transaction with a question (C15)

- Every switch of a query tab to auto-commit goes through `QueryTab::request_manual_commit`: with a
  transaction open, the window asks `end_then`'s question ("Switch “Query 1” to auto-commit?") and switches
  once it is ended; Cancel keeps the mode and the transaction. Switching to manual commit asks nothing.
- The idle rule watches any open transaction that wrote, in either mode: the transaction's log is now kept in
  auto-commit too (a typed `BEGIN`), and a new transaction starts it empty. "Set as default" calls
  `request_target`, the toolbar picker's path, and asks the same question.
- Review fix: the switch ends the transaction only (`Asking::Transactions`); the edits waiting in a result are
  neither asked about nor discarded.
- Seven UI tests (five on PostgreSQL and MySQL); each was seen failing with a part of the change disabled.
- Limits and plan errors in the "Done:" line of K5 (`docs/plans/M12.md`).

## 2026-11-18 — K6: a script file never binds to a non-local connection silently; a deleted connection's tabs (C6)

- A `.sql` file opens on the connection it last ran on, else the one browsed, only where that binds silently
  (`ConnectionConfig::binds_scripts_silently`: no `confirm_execute`, `confirm_writes_twice` or `read_only` rule,
  not read-only); otherwise the tab opens unbound, with a "Pick a connection to run this script." bar and its
  picker, the proposed connection first. Runs of an unbound tab run nothing and say so. A pick is remembered for
  the file (`settings.script_connections`). The pickers accept a closed connection, connected at the first run
  (D-078).
- Deleting a connection asks once (a table form's changes included, `Asking::Deleting`); its query tabs are
  unbound with their text before it closes (a running statement is stopped, not reported lost); once it has
  left the list its other tabs close, the closed tabs and the stored tabs follow; nothing is rebound.
- Three UI tests (one on PostgreSQL and MySQL), two unit tests; each UI test was seen failing with a part of the
  change disabled (twelve mutations). One older test now expects the message of an unbound run.
- Limits and plan errors in the "Done:" line of K6 (`docs/plans/M12.md`).


## 2026-11-18 — K7: three classifier holes (C21)

- `classify` reads a statement with a versioned comment (`/*!NNNNN … */`, `/*M! … */`) both ways, with and
  without such comments, and asks as soon as one reading destroys; a `WHERE` (and the other words that make a
  statement safer) counts only beside the keyword it protects, in the same comment or both outside one. A bare
  `/*! … */` runs everywhere and keeps its `WHERE` (D-079).
- PostgreSQL `PREPARE name [(types)] AS <statement>` is classified as its statement; `EXECUTE` and MySQL
  `PREPARE … FROM '…'` stay writes. `MERGE … THEN DELETE` is destructive. `WHERE true`, `REPLACE INTO` and
  `INSERT … ON CONFLICT` unchanged.
- 43 new classifier rows (24 failed before the fix) and one UI test on MySQL 8.4, seen failing with the fix
  disabled. The confirmation's explanation names a `MERGE` that deletes.
- Found by the agent, fixed at review: the editor dropped the comments that open or end a statement, so a
  trailing `/*! WHERE … */` was never sent (once confirmed, `DELETE FROM t /*! WHERE id = 1 */` deleted every
  row) and a statement that is all comment (`/*!40101 SET NAMES utf8 */;`) never ran. On MySQL rules such a
  comment is now statement text (`split.rs::runs_as_sql`). Limits and plan errors in the "Done:" line of K7 (`docs/plans/M12.md`).

## 2026-11-18 — K8: Kerno never vanishes at start; the store is copied before a migration (C16, C14)

- A failure between the data folder and the main window opens a small window (`start_failure.rs`): what
  happened in plain words, what to do, the raw error, and Open data folder / Start fresh (move data aside) /
  Quit as they apply; it is logged, the logger now starting first from the paths alone. The text follows a
  typed error: `storage::OpenError` (newer schema, damaged, in use, not writable, copy failed, migration
  failed), the app's `NoDataDir`, `kerno_core::Error` (D-080).
- "Start fresh" renames `kerno.db` with its `-wal` and `-shm` to `kerno-moved-<time>-schema<N>.sqlite3`, all or
  none, deletes nothing, and starts Kerno in the same run; offered when the data is the problem, never while
  another Kerno answers on that data folder's socket (checked at open and at the click).
- Before a migration an existing store is copied with `VACUUM INTO` to `backups/kerno-<time>-schema<N>.sqlite3`,
  checked, the three newest kept; a failed copy stops the migration. The pending migrations run in one
  transaction (they ran one each). One unreadable saved connection is hidden, kept and named once after start.
  A failed settings save shows one notification until a save works.
- 11 core tests, 2 unit tests, 3 UI tests; each part seen failing with it disabled (13 mutations). The guide
  says where Kerno keeps its data, the backups and how to put one back (all three files).
- Limits and plan errors in the "Done:" line of K8 (`docs/plans/M12.md`).

## 2026-11-19 — K9: the server's detail and hint on every error surface (C14)

- `kerno_core::describe` gives an error as Kerno shows it: message and code, then "Detail: …" and "Hint: …" on
  lines of their own, nothing empty or repeated; any error not from a server keeps its words (D-081). The
  transfer and the CSV import stop dropping the code, the detail and the hint.
- Every surface a server error reaches uses it: data tab and result Apply ("Nothing was changed: statement 1
  failed. The pending changes are kept." then the server's words), table form, structure tab, navigator writes
  ("The table “t” was not dropped." then the hint "Use DROP ... CASCADE …") and node loads (tooltip), COMMIT and
  ROLLBACK (the outcome first, from the session's state after the failure), transfer, import, maintenance, FDW,
  administration, ER diagram, export, quick open. An error notification with more than one line stays until
  closed. The inventory is in the "Done:" line of K9 (`docs/plans/M12.md`).
- The secret store refusing the saved password at connect is told in plain words with the next step (Allow,
  unlock, or "Ask each time"); a task whose connection did not open says it as the window does.
- Two core tests, four UI tests and one unit test; each UI test seen failing with its part disabled (five
  mutations). Limits and plan errors in the "Done:" line of K9.

## 2026-11-19 — K10: pins for what worked but nothing tested (C1, C2, C3, C6, C21)

- `scripts/bench.sh` exits 1 when a median misses its target, after the whole table, and `--report FILE`
  prints the table of a raw file kept from a run (`target/bench-raw.tsv`) without measuring. Tested on
  hand-written raw files only (the bench was not run): all met → 0, one missed → 1. On the last recorded
  numbers a full run now exits 1, for the bare binary's idle memory (152.1 MB for 150): the target is left as
  it is, the owner's decision (D-082).
- Completion on 2,000 tables and a 2 kB statement asserts under 20 ms a call; measured 0.3 to 1.9 ms in a
  debug build at a load of 5 to 11.
- Eight UI tests: "Fetch all" and `EXPLAIN ANALYZE` cancelled on the server; renaming a connection renames
  its tabs' picker; `EXPLAIN ANALYZE` of a `DELETE` without `WHERE` asks; each local target of a run on several
  connections asks; a data Apply on production asks twice; and two taken from what K3 and K9 left untested (a
  moved tab through the read-only toggle, the table form's hint). Each seen failing with a one-line mutation;
  no product code changed.
- `docs/COMPETITOR-ISSUES.md`: C1, C2, C6 and C21 now answered (9 answered, 5 gaps, 1 untested). Limits and
  plan errors in the "Done:" line of K10 (`docs/plans/M12.md`).

## 2026-11-19 — K11: completion knows the columns of CTEs, subqueries and `LATERAL` (C8)

- After `x.`, a CTE (column list, built on another CTE, `RECURSIVE`), a subquery alias, a `LATERAL` subquery,
  `VALUES`/a function with a column list, a data-modifying CTE (`RETURNING`) and a relation of `DELETE … USING`
  offer their output columns; unqualified too, ranked and qualified as a table's. Computed in `complete.rs` on
  the same tolerant tokens, once per CTE, eight queries deep at most; names and scope rules in D-083.
- Engine-made names of unaliased expressions are not offered on purpose. Not covered: a column alias list on a
  real table, a function or `VALUES` in FROM without a column list (the "Done:" line of K11).
- Six core tests and six `sql_hard_inputs` probes, all seen failing before; one UI test types a CTE on
  PostgreSQL and MySQL. Completion timing unchanged (0.3 to 1 ms a call in a debug build, ten chained CTEs
  included). `docs/COMPETITOR-ISSUES.md`: C8 answered (10 answered, 4 gaps, 1 untested).

## 2026-11-19 — K12: `BEGIN ATOMIC` and MySQL `BEGIN … END` bodies stay one statement (C9)

- A routine typed without `DELIMITER` is one statement to the split: PostgreSQL `CREATE FUNCTION | PROCEDURE …
  BEGIN ATOMIC … END`, MySQL and MariaDB `CREATE PROCEDURE | FUNCTION | TRIGGER | EVENT … BEGIN … END` (nested
  blocks, labels, handlers, `END IF` and the other `END <word>` forms, `CASE … END` expressions), and MariaDB's
  `BEGIN NOT ATOMIC … END`. Run script, run selection and run at cursor send it whole; a bare `BEGIN` still
  splits; under `DELIMITER` nothing changed (dumps included). Tracked in the split's one pass (`Blocks`), rules in
  D-084: an `END` closes only after a `;`, so `CASE` needs no tracking and `end` as a MySQL name does not close.
- A body not yet closed holds the rest of the text (nothing after it runs on its own; the server's syntax error
  shows). `classify` no longer takes `BEGIN NOT ATOMIC` for a transaction. Split timing unchanged (about 15 ms for
  20,000 statements in a debug build; a 20,000-line body never closed, about 45 ms).
- Five split tests, a completion pin (a body completes statement by statement), three classify rows, five
  `sql_hard_inputs` probes, and one UI test per engine typing a routine and checking its body on the server; the
  ones holding a body seen failing before. `docs/COMPETITOR-ISSUES.md`: C9 answered (11 answered, 3 gaps, 1
  untested). Limits in the "Done:" line of K12 (`docs/plans/M12.md`).

## 2026-11-19 — K13: search in Settings, what wins, every menu entry in the palette (C4, C5, C20)

- Settings opens with the keyboard in a search field: each word typed must start a word of a row's section,
  label, description or keywords; matches from every section show under their section's name and work there;
  Environments, Local clients and AI are found by their words and open; no match says so with Clear search;
  Escape empties the search, then closes Settings; ⌘F ("Find a Setting…", also in the palette) gives it the
  keyboard. One declaration per row (`settings_view::ROWS`) draws the row and is searched (D-085).
- A muted "… wins: …" line under each setting something overrides, worked out from the code: the destructive
  confirmation names the environments whose rules ask anyway and those where nothing asks with it off, and says
  it covers query-tab runs; closing a tab, stacked results, update checks, Local clients, and the environments'
  commit mode, read-only preset and confirmations say theirs. No behaviour changed.
- The connection dialog says what the chosen environment does beside its chips (`environment::describe`), with
  Edit environments…; Colour moved beside Folder so the General tab does not grow.
- Every entry of the data-built context menus and every toolbar button reaches a palette command, or is an
  exception with its reason (`menu_palette.rs`, exhaustive `match`es); 51 commands added (tree create, generate,
  DDL, column and enum edits; result-tab closes; generated row SQL; folder entries; Apply source; Show
  messages), each checked against what its menu greys. "Run again" and the grid's filter by this value wait for
  that check; closure-built menus are named, not walked.
- Tests: one `kerno-core`, five settings, three menu/palette, four UI (two on PostgreSQL pinning that production
  still asks twice with the setting off and local does not), each seen failing under a one-line mutation.
  `docs/COMPETITOR-ISSUES.md`: C4 and C5 answered, C20 still does not apply (13 answered, 2 gaps).

## 2026-11-19 — K14: what a drop, a truncate or a rename reaches, before the user confirms (C11)

- One driver method, `Session::dependents(&Change)`, default "not read" (Unsupported) so nothing is ever shown as
  "nothing depends on it" without a read (D-086).
- PostgreSQL: "Drop with cascade…" lists what the cascade drops with the object, by the server's own rule over
  `pg_depend` (recursive, up from a part to its owner, normal dependencies only): views over views, another
  table's foreign key as the constraint, functions on its row type, columns of a dropped type, defaults of a
  sequence, triggers of a function; not a table's own indexes, constraints, triggers, owned sequences or
  partitions. A plain Drop shows the same list as what the server refuses it for; the Truncate tool with Cascade
  lists the tables it empties too. Compared with the server's own "drop cascades to" notice on 13 shapes, in a
  transaction rolled back.
- MySQL, MariaDB: a table's Drop lists the foreign keys of other tables that refuse it and the views over it that
  break (MariaDB by a search of the definitions, said so). A rename lists what names the object in its source as a
  text search: PL/pgSQL and quoted SQL bodies on PostgreSQL (its views follow), views, routines, triggers and events
  on MySQL and MariaDB (nothing follows there, checked on both servers).
- The confirmation draws the list under its explanation (count per kind, 8 lines then "and N more", Copy the
  list); the button waits for it only where it is what the statement destroys (cascade, truncate cascade), at most
  10 s; a failed read says so with the server's words and leaves the choice to the user.
- Tests: four PostgreSQL and one MySQL/MariaDB driver test against what the servers do, four unit tests, eight UI
  tests, each seen failing under a one-line mutation; the existing cascade UI test now waits for the list before
  it clicks. `docs/COMPETITOR-ISSUES.md`: C11 answered (14 answered, 2 gaps, 3 do not apply, 1 not now). Limits
  in the "Done:" line of K14 (`docs/plans/M12.md`).

## 2026-11-19 — K15: the other long operations (C3)

- One way to stop a read on a server session (`bridge::Stop`, D-087): the read is aborted where it waits, then its
  session is closed, which stops the statement on the server; MySQL's `close` kills a statement whose caller
  dropped it midway.
- The schema tree reads on sessions of its own: on the connection's meta session a read waiting for a lock held
  the keep-alive ping, and the connection was taken for lost after 10-15 s (read, not run). Every read of the tree
  has a 30 s clock and a Cancel ("Loading…" line, row menu, Cmd+.); the row says why in plain words and the tree
  reads on at once. One more server connection per connected connection; idle memory not measured again.
- The structure tab's, the table form's and the ER diagram's reads: Cancel, Cmd+. and closing the tab stop them on
  the server. "Set comment" can be cancelled; a data apply's cancel between two statements rolls back. K14's list
  is read on a session of its own and stopped when its dialog goes.
- AI Stop closes the connection (`ureq`'s `Agent::shutdown`); a new CSV preview stops the scan before it; the ER
  layout runs on the blocking pool; a `.sql` file over 20 MB asks before the editor reads it, or runs it with the
  Execute script wizard.
- Tests: three driver or core tests, eleven UI tests (the AI test now checks that the endpoint saw the connection
  close), each seen failing under a one-line mutation; two tests that count the app's sessions now count two
  per connected connection, not one.
  `docs/COMPETITOR-ISSUES.md`: C3 answered (15 answered, 1 gap). Limits in the "Done:" line of K15
  (`docs/plans/M12.md`): the FDW dialog, "List models", "Revert file", MySQL lock tests (its catalog reads never
  waited).

## 2026-11-20 — K16: a tab whose own server session died says so before Commit (C15)

- The plan's "ping the tab's session every 30 s" was not done: any command restarts the server's idle timers
  (PostgreSQL `idle_in_transaction_session_timeout`, MySQL `wait_timeout`, a `COM_PING` too), so it would have
  kept a forgotten transaction and its locks forever. Proved on PostgreSQL, MySQL and MariaDB with a 1-2 s
  timeout set on the session (D-088).
- `Session::gone()`: PostgreSQL keeps the server's FATAL as its connection task reads it (free, with its
  SQLSTATE); MySQL/MariaDB ask from a short connection of their own whether the server still lists the session.
  A tab holding a transaction asks every 15 s while idle; on loss it says once that the server rolled the
  transaction back and why, drops the session (badge, "transaction open", Commit and Roll back go), keeps the
  transaction log and the result's waiting edits, and its next statement opens a new session.
- Found on the way: a statement that met a closed session took the whole connection down (every tab's session
  closed, their transactions rolled back) and never said its own transaction was lost; a dead session met by
  the manual-commit `BEGIN` or by a result's Apply was kept and failed every run after it. Now only the tab's
  session is dropped and the connection is checked (`Connections::verify`). Commit on a closed session is not
  sent and says so; a Roll back on it succeeds.
- Tests: five driver tests (PostgreSQL, MySQL, MariaDB) and eight UI tests, each seen failing under a one-line
  mutation. `docs/COMPETITOR-ISSUES.md`: C15 answered (16 answered, no gap). Limits in the "Done:" line of K16
  (`docs/plans/M12.md`).

## 2026-11-20 — K20: a theme switch, Settings ▸ Shortcuts, more default shortcuts (the owner's request)

- The three theme modes were there (Follow system by default, followed while Kerno runs); now pinned by a test
  of the observer's body (Follow system only). New command "Theme: Switch Light and Dark" (⌘⇧D, View menu,
  palette, also inside Settings): the opposite of the theme on screen, kept as Light or Dark.
- Settings ▸ Shortcuts (last section; "Keyboard Shortcuts…" in the Kerno menu and the palette): every command by
  group with its shortcuts, "Changed" on the user's, a filter by name, group or shortcut (⌘K, `cmd k`); change by
  pressing the new keystroke (caught before any binding by `App::intercept_keystrokes`; a key that would type
  is refused with the reason, and so is a text field's own key such as ⌘C for a command that works everywhere,
  which would win over it in every field; Escape cancels), Add, ×, Reset to default, Reset all… (asked); a key
  another command answers asks Replace or Cancel. Changes apply at once without a restart: Kerno's bindings carry a
  meta tag, so they are replaced and the kit's stay; the menu bar is set again for its shortcut column. They go
  to `keymap.json`, edited in place by one background writer (the rest of the file kept); a file that cannot be
  used is said on the page and never written. Open Keymap File stays. D-089.
- The check of the keymap file at start uses the page's conflict rule (two commands whose contexts can both
  apply), not only one context.
- Fifteen new default keys on thirteen commands, from DBeaver, DataGrip and macOS or browser conventions
  (letters, arrows, Page Up/Down with Cmd or Ctrl: nothing behind Shift or Option on AZERTY), checked against the
  kit's input keys, macOS and Linux desktop keys and vim mode, and seen winning in the editor and in a text field.
- Tests: seven unit tests in `actions.rs`, fourteen UI tests in `ui_tests/shortcuts.rs`, each seen failing under a
  mutation (`docs/plans/M12.md`, K20); `a_bad_keymap_file_is_reported_and_the_defaults_stay` now pins that a
  second binding replaces the first (it relied on keys stacking). Capture of the page to take.

## 2026-11-20 — K17: a value panel docked beside the grid (the owner's request of 2026-11-18)

- `query/value_panel.rs`: the selected cell whole at the right of every query tab's results and every data
  tab's rows (F7, "Toggle Value Panel", the footers' "Value" button); shown or not and its width kept with the
  settings (D-090). It follows the selection from the grid's store: JSON indented and highlighted in the kit's
  code editor, long text wrapped, binary as hex, NULL apart from the empty string, the Unix-time line, real
  empty states; large values laid out off the UI thread.
- Edits through the dialog's path: Set or Cmd+Enter (never "Run statement"), Set NULL, Format / Compact JSON;
  broken JSON refused with its line and column; the tab's reason when the rows take no value. Typed text holds
  the panel on its cell while the selection moves, and goes to the clipboard with a notification if its rows
  go. A double click or Shift+Enter focuses the panel when shown.
- `viewer.rs`'s renderers and save rule became functions the dialog and the panel share.
- Tests: three unit tests (one extended), three UI tests on PostgreSQL and MySQL, each seen failing under a
  one-line mutation. Captures `m12-value-panel-json-{light,dark}` to take. Limits in the "Done:" line of K17
  (`docs/plans/M12.md`).

## 2026-11-20 — K18: the row a foreign key points to, previewed in the value panel (the owner's request)

- `query/value_panel/fk_preview.rs`: under the value of a foreign-key cell, the referenced row as a short
  record ("customers, by customer_id"): the columns a person reads first (`default_columns`, a pure function),
  "Show all N columns", "Choose columns…" (kept per connection and table beside J6's layouts, under a key of
  their own: no migration), "Open the row" (the cell menu's jump). Composite keys, a pending key (its pending
  value's row, said), NULL (nothing), a dangling key ("No row with this key"), a failure (the server's
  message), a stop ("Not read: cancelled." / "… took longer than 10 s.", Try again). Editable one-table query
  results too. D-091.
- The read is the E3 jump's statement (`data::tab::reference`, extracted from `related`, and `page_sql` with a
  limit of one), on a session of the panel's own, started once the selection rests 250 ms, stopped
  (`bridge::Stop`) when it moves to another key and after 10 s, cached per grid.
- `Grid::text_of` became `Grid::table_text` (the result's column that shows a table column, through an
  editable result's labels); `data::tab::table_key` is J6's layout key, shared.
- Tests: two unit tests, one storage test, seven UI tests in `ui_tests/fk_preview.rs` on the three engines,
  each seen failing under a one-line mutation (`docs/plans/M12.md`, K18). Captures `m12-fk-preview-{light,dark}`
  to take.

## 2026-11-20 — K19: the tab area split in groups (the owner's request of 2026-11-18)

- `workspace/panes.rs`: the groups of tabs as a tree (D-092), with split, move, close, resize, store and restore
  (an old or broken layout mended into something usable) unit-tested. The workspace keeps its flat list of
  tabs in the layout's order; every former use of the one active tab goes through the focused group.
- Split Right (⌘⌥⇧L), Split Down (⌘⌥⇧I), Move Tab to the Next / Previous Group (⌘⌥⇧M), Focus Next / Previous
  Group (⌘⌥⇧N / ⌘⌥⇧P) in the palette, the Window menu and each tab's menu; a tab dragged to an edge of a
  group's content splits there, with an indicator of the half it will take; onto a strip it moves there (and
  reorders within its own). A divider between groups; a group left empty closes. The layout and the sizes come
  back after a restart.
- The focused group (last clicked, or where the keyboard is) drives ⌘Enter, Commit, ⌘., ⌘W, the toolbar, the
  status bar and the chrome's colour; quitting and every check over the tabs still cover every group.
- Tests: eleven unit tests of the tree, twelve UI tests in `ui_tests/split.rs` (three on PostgreSQL), each seen
  failing under a mutation. Captures `m12-split-views-{light,dark}` to take. Limits in the "Done:" line of K19
  (`docs/plans/M12.md`).

## 2026-11-20 — K21: the first outside user's feedback

- A colleague of the owner, first time, beside DBeaver: Fill was not understood, the URL example stayed
  `postgres://` on MySQL, "no load more", the light grid "all white" (and "ultra fast": nothing made slower).
- Connection dialog: the URL example follows the engine (at open and on each change); a hint under the field; a
  pasted URL fills the form at once, a typed one waits for Fill or Enter; a line says when the form was filled.
- Results: the fetch at the end of a scroll already existed (M5); the footer now says "1,000 rows fetched, more on
  the server" and "fetching more…", a setting turns the automatic fetch off (DBeaver has one), and it waits while
  rows are sorted in Kerno on an incomplete result. A table's pager says "Page 2 of 13" and "Next 1,000 rows",
  outlined while there is one, and wraps in a narrow group.
- Light theme: stripe, grid lines, header and hover darker (ratios in D-093), dark unchanged; every grid striped,
  a table's data included.
- Tests: three unit tests (one extended), seven UI tests in `ui_tests/feedback.rs` (fetch on PostgreSQL and MySQL),
  each seen failing under a mutation. Captures `m12-light-grid-{light,dark}`, `m12-connection-url-{light,dark}`
  to take.

## 2026-11-20 — K22: legacy MySQL servers (the owner's request of 2026-11-20)

- Four real old servers in a `legacy` compose profile: MySQL 5.7.44, 5.6.51 and 5.5.62 (official images, amd64
  under emulation) and 5.1.73 (built from Oracle's archived binaries, no image exists), seeded with a fixture
  every release accepts; `scripts/test-env.sh up --legacy`, started by `scripts/check.sh`.
- Run against them, the 8.4 driver failed on 5.1 at the first navigator listing (`PARAMETERS`), and on every
  5.x in the isolation level, what a drop breaks, the accounts, settings and lock lists, read-only
  connections and EXPLAIN before 5.6.5, literals with a backslash before 5.5.3, and TLS (TLS 1.0 on 5.1/5.5,
  DHE-only on 5.7.44), even in `prefer`.
- One engine that adapts (D-094): `Capabilities` in `kerno-mysql`, read from the server's version at connect,
  consulted by the dialect, the introspection and the administration queries. `prefer` goes on without TLS on
  an old TLS; the strict modes, pre-4.1 password hashes, plans and renames an old server lacks are said in
  words; a read-only connection to a server without read-only sessions warns; a failed apply on MyISAM says
  what stayed; a value a non-strict server would store changed stops the apply. "Connected to MySQL 5.1.73".
- Tests: thirteen integration tests on the four servers, unit tests of every boundary, one UI test; each seen
  failing under a mutation. MySQL 8.4 and MariaDB 11 unchanged. Limits in the "Done:" line of K22
  (`docs/plans/M12.md`) and in `docs/ROADMAP.md`.

## 2026-11-20 — K23: the AI assistant's endpoints

- The owner: "tell me what it is for", then make reaching local and work models simple (models on two DGX
  Spark machines behind an OpenAI-compatible LiteLLM proxy on the company LAN, declared for omp by a company kit).
- Settings → AI now opens on what the assistant does, where it is found and what it sends, then the switch;
  several saved endpoints (one active, Use, or the panel's "endpoint · model" picker), added three ways: Detect
  on this computer (six local ports, on click), Import from omp, pi or the environment (keys to the secret store,
  a command key never run), or by hand (with or without `/v1`); Test says the latency and the models, or the
  failure in plain words; the model is picked from the endpoint's list, filtered as you type, kept per endpoint.
- The client trusts the system's certificates (an internal authority the computer trusts works), names the
  issuer of one it refuses, waits 5 minutes for an answer (Stop still at once) and leaves a model's reasoning
  out of what is shown and inserted. The settings of before become one saved endpoint, with their key; an older
  Kerno keeps working with the active one (D-095).
- Tests: ten unit tests in kerno-core (a local TLS server with a test authority for the certificate), eight UI
  tests in `ui_tests/ai_endpoints.rs`, one rewritten in `ui_tests/ai.rs`, each seen failing under a mutation.
  Nothing was run against a real LAN endpoint. Captures `m12-ai-endpoints-{light,dark}` to take.

## 2026-11-20 — The owner's test of the build and a first real-app pass

- The owner's test: "Fill" on an empty URL field takes the clipboard's URL (`278e0a3`); a table's toolbar drew
  over the group beside it in a narrow split: the toolbars of the table, structure, administration and diagram
  tabs wrap, and a group cuts what is drawn wider than itself (`66974a6`).
- A real-app pass through the release checklist (an agent driving its own instance on a scratch data directory,
  in the background): 26 of 86 points passed before macOS's Stage Manager and then the screen lock stopped it;
  the rest is still to run. What it found, fixed here: the value panel kept its remembered width in a narrow
  group and was drawn past the group's edge (`ValuePanel::fit`: it takes what the grid leaves and its own width
  again when there is room, without forgetting the user's width;
  `ui_tests::split::the_value_panel_stays_inside_a_narrow_group`); a list an old server does not have (the locks
  on MySQL 5.1) was shown as a failure that blamed the account's privileges, two sentences run together: it is
  said as missing, with the server's name (`PaneState::Missing`;
  `ui_tests::legacy::a_list_an_old_server_does_not_have_is_said_as_missing`); four sentences carried a run of
  spaces from a broken line continuation since M6 (the result's Apply dialog: "or none            does.", the
  read-only and no-secret-store notices); Run and its menu stay together when a query tab's toolbar wraps.
- Found and left: a notification covers the tab's toolbar while it shows; Cmd+T on a saved connection that is
  not connected opens a tab without a connection; the window's size is not restored after a restart; with a
  very long data directory the instance socket's path is too long and `kerno <url>` does not reach the running
  instance (a warning in the log).

## 2026-11-20 — The second real-app pass, in the foreground, and what it found

- The pass (an agent driving its own instance, window in front, on a scratch data directory): the 55 points the
  first pass left, plus the four fixes of `1b8a375`. 50 passed, 3 partly, 2 failed; no crash, no password in a
  log, nothing left running. Not run, the owner's: the system appearance (`k20-system`), the shortcuts on a
  physical AZERTY keyboard, the import from omp and the test of the work proxy, and the points of after the
  release. The AI points ran against a fake endpoint on loopback.
- Behaviour, fixed: "Insert" put the assistant's SQL at the caret with nothing around it
  (`…restart_check_oneSELECT * FROM…`): a selection is replaced, at a caret the SQL gets its own line
  (`ai::placed`), and the panel shows and copies the answer without its code fences (`ai::unfenced`). The value
  panel took a width `fit` had squeezed it to for the user's (450 kept as 308) and did not grow back: an event
  that answers a fit is never saved, and one fit runs at a time. A panel with no room is said on its button
  (warning outline, "No room for the value panel in this group: widen it"). The status bar showed the time since
  the run after a later page (34 s for a 28 ms statement): a statement's time stops at its first page, in the
  status bar and the history; its row count takes the thousands separator. Settings ▸ Shortcuts: "1 changed
  command loses its changes"; a conflict question goes when its row changes another way. Settings ▸ AI: adding
  an address already saved names the endpoint that has it and asks, instead of renaming it silently; the model
  field saves on Enter, on leaving the field or on a pick, not at each key. Sidebar: a filter that matches
  nothing loaded says so and names Quick Open; a load the user cancelled reads "Cancelled. Expand again to
  load.", muted, not as an error.
- Layout in a half-width group, fixed: the result's filter bar, edit bar and footer wrap (Apply was cut to "A",
  Delete and Set NULL dropped out, the WHERE field shrank to "Filter tl": it keeps 180 px); the same for the
  table's raw filter and the selection's figures; the sub-tabs of the structure and administration tabs get the
  kit's menu that lists them all; an empty administration list wraps. The commit-mode menu was not clipped: the
  kit caps a menu at 31.25 rem and one entry was longer; it reads "Manual commit: changes wait for Commit or
  Rollback".
- Light grid: the kit paints a table's selection at 20 % opacity at most, so the Lime wash came out grey on a
  stripe, and the header was 1.05:1 against a stripe. The selection is written so that it paints the Lime wash
  (`#B9DC3233`), the header is `#E4E8EC`, the row line `#D3D8DE`; the theme tests read the selection as painted.
- Left to the owner: in the dark theme the same cap makes a selected row's tint about 1.02:1 against its row
  (the cell's outline is what shows), and `a_selected_row_stands_out_in_the_dark_theme` reads the colour as
  written, not as painted; list and sidebar selections are capped the same way; muted text (NULL) under a
  selected cell stays under AA (about 3.6–4.0:1); Import and Detect still merge into an endpoint with the same
  address without asking; a paged result's time no longer counts a later "fetch all".
- Tests: 2 unit tests in kerno-core, 13 UI and theme tests added and 3 rewritten, each seen failing under a
  mutation.

## 2026-11-20 — Kerno 0.2.0 published (the owner's ask)

- The owner tested the build of `027e158` and asked for the release. `52efdc8`: workspace version 0.2.0, the
  four READMEs' status line, `docs/release-notes/v0.2.0.md`. `scripts/release.sh 0.2.0` built the macOS disk
  image and the six Linux packages (arm64 in 355 s, x86_64 cross-compiled), started every Linux package in
  clean Ubuntu 20.04 and Debian 13 containers, and wrote `SHA256SUMS`; 40 minutes in all.
- Published at the owner's word: `main` pushed, tag `v0.2.0`, `gh release create` with the eight files and the
  notes. Checked after: not a draft, `releases/latest` redirects to `v0.2.0`, the published `SHA256SUMS` equals
  the local one. Not checked from here: the update as a 0.1.0 install sees and installs it (the owner's
  installed app is his to run).
- After it, at the owner's ask: a release builds on every core (D-096).
- Still to do: the captures of K1 to K23 for `docs/screenshots/final/`, the website, the bench of memory at
  rest (152.1 MB measured before K15 and K18 added sessions).

## 2026-11-20 — K24: a new version opens a dialog at launch (the owner's ask, after 0.2.0)

- The owner saw 0.2.0 offered only as a small text at the end of the status bar and asked for a window at every
  launch, as DBeaver does, until the version is skipped. D-097: a timed check that finds a newer version opens
  the kit's dialog once per run ("Kerno X is available", the version in use, what Update does on this copy,
  What's new, Update / Later / Skip this version); Enter and Space act on nothing, Tab reaches Later first and
  Update last (the review's change: every answer from the keyboard), Escape is Later; it waits while another
  dialog is open; Update is the status bar item's path; a skipped version keeps its item (tooltip) and Settings
  ▸ Updates shows it with Ask again. No new request.
- Tests: 6 UI tests added and 1 rewritten in `ui_tests/updates.rs`, each seen failing under a mutation.
- Still to do: the captures `m12-update-dialog-{light,dark}`.

## 2026-11-20 — K25: the look of Settings (the owner's request)

- The owner sent a capture of Settings ▸ Appearance ("a bullet list is not great, we can have small card
  buttons with animations"). K25 in a worktree beside K24: a side list of the ten sections with glyphs, each
  page a head and panels of rows (name left, switch or control right, override lines marked), option cards with
  previews drawn in the theme's colours for the theme, the double click and the tabs' layout, 120 ms colour and
  fade motion, one Tab stop per group of cards with the arrows moving the choice, the search's results grouped
  with a link to their page, a height that follows the window (the footer was cut at 600 px). D-098.
- Seen while working: every page in both themes at 1200 × 800 and 900 × 600, rendered offscreen by a temporary
  test (GPUI's headless context, Metal's headless renderer, the real text system; not committed). What the
  captures showed and was fixed: a click on a section drew a focus ring over its wash (a click no longer takes
  the keyboard from the search), the side list kept a section marked during a search, the AI purpose in body
  size above everything.
- Tests: `ui_tests/settings_look.rs` (five), the theme's previews and new contrast pairs; the kit's switch track
  was 1.35:1 on white, now a token (`switch.background`). Whole `cargo test -p kerno`: all pass but the two SSH
  tests whose generated keys a worktree lacks.
- Still to do: the final captures (`m12-settings-*`), the owner's look at the cards' size and at the short
  pages (Updates, Confirmations) under their panels.

## 2026-11-20 — K26: a simple backup and restore by default (the owner's request)

- The owner found the backup wizard had too many things ("a simplified mode by default, and an 'advanced
  settings' mode for the version we have now"). A database's Back up… and Restore… now open one screen over the
  wizard's own state (`backup_dialog/simple.rs`): what is backed up in one sentence with its schemas, tables and
  views counted, the file (the last backup's folder, else home; the wizard's name pattern; the save panel),
  PostgreSQL's two formats as option cards (compressed archive by default, plain SQL script), MySQL's defaults
  said in one line, the client small with "Local client…", "Show command"; a restore's file, its database and
  what a restore does to it. "Advanced settings…" is the wizard on the same file and format; "Simple view" goes
  back, asking first when a hidden option would be reset. Tools ▸ on a schema or a table, tasks, the global backup
  and Execute script stay in the wizard; the toolbar's Tools ▸ backs up the tab's database, not its schema. Enter
  backs up, never restores. The page's height follows the window in both views (the wizard's footer was cut at
  600 px). D-099.
- Seen while working: both views and the way back's question in both themes at 1200 × 800 and 900 × 600,
  rendered offscreen by a temporary headless test (not committed) on the test server with a stand-in client.
  Fixed from the captures: the client's long folder on the same line as its name (now a small line under it), a
  bare "Show command" that did not read as a disclosure (a chevron).
- Tests: `ui_tests/backup_simple.rs` (six, each seen failing under its mutation); the wizard's own tests open it
  through `show_advanced`. The tool modules, Settings, the toolbar, the tasks and the whole `cargo test -p kerno`
  pass; clippy and fmt clean.
- The review's change: a file chosen in the macOS save panel, which already asks before replacing it, is written
  without Kerno's second "Replace …" question; Linux keeps Kerno's question.
- Still to do: the final captures (`m12-backup-simple-*`, `m12-restore-simple-*`), the owner's look at the format
  cards' size and the wording of the MySQL line.

## 2026-11-20 — K27: an extensions manager per PostgreSQL database (the owner's request)

- The owner did not like the extensions in the tree (fifty "not installed" rows, no description, no schema or
  version, no update, nothing on requirements) and approved a manager per database. K27 in a worktree beside
  K26: the tree lists installed extensions with their count; a tab per database lists every extension the
  server has, with its description, versions, state, a search, filters with counts, the details read from the
  catalog (schema, relocatable, versions, requirements, who may install it), and Install… (schema, version,
  requirements with `CASCADE` said first), Update, Update to…, Move to schema…, Drop and Drop with cascade (K14's
  list), each through the usual confirmation and the connection's rules, refusals in plain words first. Ways in:
  the folder (double click, Enter, menu), an installed extension's menu, Tools ▸ Extensions…, the palette's
  "Manage Extensions…" (replacing "Install Selected Extension…"). D-100.
- Seen while working: the manager rendered offscreen in both themes (list, details, confirmation, empty search,
  narrow group; temporary test, not committed). Fixed from the captures: the selected row out of sight, a version
  marked "(default) (installed)", two sentences.
- Tests: core and driver statements and reads (a scratch database each, the fixture's extensions untouched),
  three unit tests of the tab, seven UI tests (six new in `ui_tests/extensions.rs`, one rewritten), each seen
  failing under a mutation; `tools_menu.rs`'s database Tools ▸ line updated (K26's file, one assertion, agreed).
  Whole `cargo test -p kerno`: 774 pass; `kerno-core` and `kerno-postgres` pass (both with the git-ignored
  `tests/fixtures/generated` linked from the main checkout for the run: the worktree has no SSH keys or TLS
  certificates of its own); fmt and clippy clean.
- Still to do: the final captures (`m12-extensions-*`), the owner's look at the manager.

## 2026-11-21 — K28: four defects of daily use

- Found in real use, each cause read in the code before the fix (D-101): the window always opened centred; the
  dark theme's selection was written opaque and painted by the kit at 20 % (1.02:1); notifications sat at the top
  right over the toolbars; ⌘T on a selected connection that was not open gave a tab bound to nothing.
- Now: the window's frame, display and state are stored with the settings (after it stops moving, and at quit)
  and restored on that display, centred as before when the display or the window's top is gone; the dark
  selection of rows, cells, ranges, lists and the tree is Lime at 16.5 % (1.53:1 painted), muted text one step
  lighter for NULL on a selected cell of a hovered row; notifications at the bottom right above the status bar and
  a tab's footer; "New SQL editor" from every way in connects first and opens the tab on the connection, no tab
  when it fails or is cancelled.
- Seen while working: a selected row, cell and range and an error notification over a query tab, rendered
  offscreen in both themes (temporary test, not committed).
- Tests: two core, one theme test rewritten to measure what is painted, four UI tests in
  `ui_tests/daily_use.rs`, each seen failing under a mutation. `cargo test -p kerno`: 784 pass; `kerno-core`
  passes (with `tests/fixtures/generated` linked from the main checkout); fmt, clippy and deny clean.
- Not proved in a real window: the restore of a real window's place, maximised or full screen (test context
  only). Still to do: the final captures (`m12-grid-selection-*`, `m12-notification-*`).

## 2026-11-21 — K29: SQLite, the third engine family (ROADMAP's SQLite item)

- A new crate `kerno-sqlite` on the bundled `rusqlite` already in the workspace (no new dependency; the
  `column_decltype` feature): a connection is a database file, chosen or made empty with New file…, never
  created by a mistyped path; one thread per session owns its connection; rows stream to the sink without
  reading ahead; cancel is SQLite's interrupt; types per cell by storage class, the declared type's rendering
  kept only when the value fits it. `Engine::Sqlite` and every match on the engine decided; three
  capabilities added to the dialect with defaults (`begin_sql`, `transactional_ddl`, `has_routines`), client
  tools now optional per engine, so the backup dialog, Tools ▸, the dashboard and the administration tab refuse
  or hide themselves for a file with their reason. D-102.
- In the app: the dialog shows the file and the choices every connection has, General and Advanced tabs only;
  the tree shows `main`, tables, views, triggers, columns and indexes; queries, plans, data editing, the
  structure tabs, the table form (SQLite's four alters and indexes, the rest refused in words), exports, CSV
  import and the ER diagram go through the generic paths, each tested on a file.
- Seen while working: the dialog, the tree and a data tab, a query with every storage class, a plan, the
  structure tab and the table form's refusal, offscreen in both themes (temporary test, not committed). Fixed
  from them: the empty Routines and Events folders under `main`; the four engine cards now fit one row.
- Tests: 15 integration and 12 unit tests in `kerno-sqlite` (files made in temporary directories, never a file
  of the user's), core tests of the config, the URL and export, the failures' words and the trigger split, one
  form test and three UI tests; mutations named in M12's K29.
- Review: the table form no longer shows its comment fields on an engine without comments
  (`Engine::has_comments`; SQLite refused them only once typed); the UI test of the refused alter checks it and
  was seen failing with the capability switched.
- Still to do: the final captures (`m12-sqlite-*`); what ROADMAP's SQLite item lists as still to come.

## 2026-11-21 — K30: schema compare (ROADMAP item 1)

- Compare two schemas (two databases on MySQL and MariaDB) of the same engine family: a dialog picks the source
  (the model) and the target, a tab lists tables, views, routines, sequences, types and triggers only in the
  source, only in the target, different or the same, says what differs in plain words with both definitions
  side by side, and "Open script" opens the script that brings the target to the source in a query tab on the
  target, where nothing runs by itself. Drops of what only the target has are comments until asked for. Ways in:
  Server ▸ Tools, the palette, a schema's or database's Tools ▸ and "Compare with…". D-103.
- The diff is core code (`kerno-core/src/compare.rs`) over `TableStructure` and `table_draft::changes`; the
  diagram's read loop is shared (`erd::buffered`); two provided dialect methods write constraints.
- Seen while working: the dialog, the tab with a table selected, the script and the identical state rendered
  offscreen in both themes (temporary test, not committed).
- A name with a line break could end a comment of the script and leave SQL to run (the security review):
  one function now writes every comment line, split on every line break.
- Tests: twelve core unit tests, a round trip per engine (compare, run the script, compare again: identical) on
  PostgreSQL, MySQL and MariaDB in scratch schemas and databases, six UI tests in `ui_tests/compare.rs`; each
  seen failing under a mutation. Two Tools ▸ tests updated for the new entry.

## 2026-11-21 — K31: a schema as a document

- "Export schema…" writes the structure of a schema, everything or the tables and views ticked, as HTML,
  Markdown, DBML, SQL or Mermaid; the HTML in Kerno's light or dark style or plain black on white, with or
  without the diagram, one file that asks the network for nothing and prints a page per table. Ways in: the
  tree (a schema, a database without schemas, a table or a view ticked alone), the ER diagram's "Export
  document…", Server ▸ Tools and the palette. Structure only, never a row, never the host, user or file path.
  D-104.
- The writers are pure core code (`kerno-core/src/schema_doc.rs`, `schema_doc/html.rs`) over the compare's
  model, read through the compare's loop narrowed to what is ticked; the SQL is the compare's script against an
  empty schema. One escaping writer per format, each tested with the same hostile schema.
- The PDF was not done here: `krilla` brings a third copy of `read-fonts` and `skrifa` on Linux (D-104 lists the
  crates); it came in K32 with `pdf-writer` and `subsetter` (D-105).
- Seen while working: the dialog on HTML, on Markdown, done and empty, offscreen in both themes at 1200 × 800
  and 900 × 600 (temporary test, not committed); the fixture's HTML in the three styles on screen and printed
  through headless Chromium, and a 60-column table, the empty document and the hostile one printed. Fixed from
  them: at 900 × 600 the Export button fell off the dialog (the options now scroll, the file and the buttons
  stay); the SQL card's preview ran out of its frame; the "Tables" heading stood alone at the foot of a page (a
  section now starts a page with its first table); a cell's code printed larger than the rest; in a table of long
  types the short columns were squeezed to a letter wide (header cells no longer wrap, names and comments keep a
  minimum width).
- A cancel that lands while the file is written leaves no file: it is written as `<name>.part` and put in place
  only if the export was not cancelled.
- Tests: eighteen core unit tests (golden text per format, the hostile schema, the styles' contrast), a read-only
  driver test per engine (PostgreSQL, MySQL, MariaDB on the fixtures, SQLite on a file it makes), six UI tests in
  `ui_tests/schema_export.rs` and a unit test of the file's write; each but one seen failing under a mutation
  (the read-only and production test guards against a question that does not exist).

## 2026-11-21 — K32: the schema document as a PDF

- The schema document is now written as a PDF too: the HTML's content on A4 or US Letter pages, in Kerno light,
  Kerno dark or Plain, with the diagram and the lockup as vectors, a contents page whose entries are links with
  their page numbers, a bookmark per table, view and routine, header rows repeated on every page a table
  continues on, and "page n of N" at the foot of each page. Its text can be searched and copied: the fonts are
  embedded as subsets with a map back to each character. D-105.
- Two packages added, `pdf-writer` and `subsetter` (without `variable-fonts`); `kurbo` and `flate2`, already in
  the tree, are now direct dependencies of `kerno-core`. `ttf-parser` and `rustybuzz`, named for the fonts, are
  marked unmaintained by RustSec and refused by `cargo deny` as direct dependencies: a small reader of the font
  tables needed (`sfnt.rs`) takes their place, checked against `ttf-parser`'s values first. The fonts are the
  ones the app already embeds, not a second copy.
- Seen while working: every page of the fixture's PDF in the three styles, the hostile schema, a 60-column table
  of 300-character types and an empty pick, rasterised with `pdftoppm` (nothing on its stderr) and read back with
  `pdftotext`; the dialog with the PDF card and its finished state, light and dark, 1200 × 800 and 900 × 600.
  Fixed from them: a DDL block lost its indentation.
- Tests: ten core unit tests that read the PDF back without a reader (its objects, ToUnicode maps and shown
  strings) or check the font reader against `ttf-parser`'s values, two of the font reader's character maps, the
  driver tests of K31 extended to the PDF on PostgreSQL, MySQL, MariaDB and SQLite, one UI test; each seen failing
  under a mutation.

## 2026-11-21 — K34: a selected row is seen at a glance in both themes

- The light theme's selection was the Lime wash, painted by the kit at 20 % and so at 1.01:1 on the sidebar: the
  owner could not see which schema was selected. Every selection is now the brand's accent at the kit's cap,
  Olive in light (1.31 to 1.34:1 on what it is painted on) and Lime in dark (1.49 to 1.56, as K28 left it), and
  the menus, the command palette, the dropdowns and the dialogs' picked rows use the same tint, opaque (1.25 to
  1.34 in light, 1.55 to 1.73 in dark; they were 1.02 to 1.19). The text selection of the editor and the inputs
  goes from 1.07 to 1.24 (light) and 1.20 to 1.42 (dark). The record view's current field takes the grid's tint.
  D-107.
- The light theme's muted text is one step darker (`#505965`, 7.1:1 on white) so that it keeps 4.5:1 on a
  selection, as the dark one went one step lighter in K28.
- Not reached: the `=` mark of a pending SQL expression on a light selection over a striped or hovered row (4.3 and
  4.0:1); hover and selected are one look in the kit's menus and in Settings ▸ Environments; the tree's selection
  is the same with or without focus. Side effects: ghost buttons hover with the Olive tint in light, and a find
  match is the colour of a selected cell in light (the selected one keeps its frame).
- Seen while working: the tree with `shop` selected, focused and not, the grid's row, cell and range, the record
  view, a dropdown, the command palette, the schema export and compare dialogs, light and dark, before and after.
- Tests: the dark-only selection test of K28 becomes one for both themes, plus one for selected text; each seen
  failing with each old value put back.

## 2026-11-21 — K33: passwords in Kerno's own encrypted file, so macOS stops asking

- The owner was asked for his login password by the macOS keychain after every update ("je veux pas qu'on
  demande de mot de passe"). The cause is the ad-hoc signature, which changes at each build: macOS asks again
  once per keychain item. He chose DBeaver's way: on macOS, saved passwords now go by default into Kerno's own
  encrypted file in the data folder (`secrets.kerno`, AES-256-GCM, its key in `secrets.key`, both readable by
  his account only); no prompt. Settings ▸ Passwords keeps them in the macOS Keychain instead, and says what
  each choice protects: the file keeps them out of plain sight, not from a program running under his account.
  Linux keeps the Secret Service by default. D-106, which notes that `CLAUDE.md` §1's line on the OS secret
  store is now the owner's to update.
- Nothing to retype: a password saved in the keychain before is read there once, at the first connect, edit or
  duplicate that needs it (macOS may ask one last time), then kept in the file; the keychain is never written or
  emptied while the file is in use. Back on the keychain, the file's passwords move there. The setting applies
  without a restart. "Ask each time" no longer reads any store.
- A file that cannot be decrypted is set aside as `secrets.kerno.damaged`, said once in plain words, never
  overwritten; writes go through a lock, a temporary file and a rename, so two Kerno processes or a crash lose
  nothing. `KERNO_SECRET_STORE` takes `file` and `keychain` besides `memory`.
- `aes-gcm` and `getrandom`, already in the tree, are direct dependencies of `kerno-core`; no package added.
- Wording that named the keychain for any saved password now names the place in use ("Password from Kerno's
  file", the AI key lines, "Use the saved secrets", the memory-only alert).
- Tests: thirteen core unit tests of the file and of the move between stores (a fake keychain), one of the
  error's words, one manager test, one UI test; each but two plain round trips seen failing under a mutation.
  Captures of the Passwords page, light and dark, at two sizes.

## 2026-11-21 — Release 0.3.0: a pass in the real app, then the version

- The owner's word: « release 0.3.0 ». Before it, a pass in the real application (a copy of the release
  bundle of `cd9a401`, a scratch data folder, driven through its window): the start from a URL, the selection
  in both themes, the schema export (PDF 10 pages with its text read back by `pdftotext`, HTML without a
  script, DBML of one table; no host, port or password in any file), the schema compare (22 differences,
  "Open script" ran nothing: the target was read again and is unchanged), the extensions manager (read),
  SQLite (a file chosen in the panel, browsed, queried, one cell edited and applied), Kerno's password file
  (`secrets.kerno` and `secrets.key` in mode 0600, the password in neither, a reconnect after a restart without
  any question), the window back at its place and size, and the simple backup view.
- Fixed from it: "Compare with…" on a SQLite database did nothing (the compare has no SQLite side yet): the
  entry is now disabled with its reason. `the_fixture_file_is_browsed_queried_and_edited` holds it.
- Seen and left for after the release: in a window 700 px high, the result of "Test connection" pushes the
  connection dialog's buttons below the window; the toolbar's "New SQL editor" follows the connection the
  tree is browsing (the active tab's), not a row of another connection just selected; a test with a typed
  password says "Password from Kerno's file" before it is saved; the compare dialog's schema fields are free
  text; save panels open in the home folder each time; a data folder with a very long path leaves the
  single-instance socket unopened (a warning in the log).
- Version 0.3.0 in `Cargo.toml` and the four READMEs (SQLite named, schema compare off the roadmap line),
  notes in `docs/release-notes/v0.3.0.md` with what the keychain asks once after the update.

## 2026-11-23 — K35: a restore whose backup names a role the server does not have

- The first outside user restored a production dump into a local PostgreSQL: pg_restore stopped at `ALTER SCHEMA
  archive OWNER TO app_owner` (the role exists only in production), and Kerno left the raw output. The failure
  view now says first, in plain words, which role the backup names and that the server has none (the server asked
  through `pg_roles`), then what to do; the output stays below. A PostgreSQL archive gets "Restore without owners
  and privileges…", the same restore with `--no-owner --no-privileges`, shown and asked as any restore (after a
  run in one transaction, the default; one that went on past it says so instead). A plain SQL script gets no
  button, only what can be done: create the role, or back up again as an archive. D-108.
- The detection reads the SQL the tool printed (pg_restore's failed command, psql's script line), never the
  words around it: pg_restore and the server each speak their own language. The user's French output and its
  English form are test inputs.
- The simple restore has a checkbox for PostgreSQL archives, "Leave out the backup's owners and privileges",
  off by default (pg_restore's own default); the wizard's two checkboxes stay and follow it.
- MySQL and MariaDB looked at only: a `DEFINER` naming a missing user passes for a user with `SET_ANY_DEFINER`
  (`SET USER` on MariaDB), the object failing later when used, and stops the restore for a user without it.
- Tests: five core unit tests of the detection, one of the words per case, three UI tests (a stand-in pg_restore
  and psql, and the real tools on `kerno-postgres` with a scratch role whose name needs quotes); each seen
  failing under a mutation. Captures of the failure view and the simple restore, light and dark.

## 2026-11-23 — K36: a table's rows keep loading as you scroll, the fetch size changed in place

- A first user outside the owner, replacing DBeaver: a table's data turned pages while a query's result
  fetched more at the end, and its size could only be changed in Settings. Now a table's or a view's next rows
  are read when the scroll reaches the last ones, or on "Fetch more" (Ctrl+Alt+N), and added under those
  there: the selection, the scroll, the record view and the changes waiting stay. "Fetch all" (Ctrl++) reads
  load after load, the count growing, until Cancel.
- The footer: "Rows 1–3,000 of 12,345", "Fetch size: 1,000" (a list, or a size typed, for this tab; Settings
  keeps the default), "Fetch 1,000 more" outlined while there are more, "All rows fetched" at the end. A load
  that fails keeps everything, says why and offers "Try again".
- A table with a key is read by offset in its key's order. A table without a key, or a view, is read again
  from its first row at each load, in one statement, so no row shows twice and none is skipped (D-109).
  Refresh and Apply read back as many rows as were loaded and keep the selection; a new order or filter starts
  from one load. The pages, Previous and Next are gone.
- Tests: six UI tests (PostgreSQL, and MySQL for the first), each seen failing under a mutation; five tests
  updated from pages to loads. Offscreen, with 100,000
  rows of `analytics.big_table` loaded: 7.72 ms a frame scrolling, +29 MiB for 90,000 rows, a load of 10,000
  rows in 50 to 68 ms. Captures of the footer, light and dark.

## 2026-11-23 — K38: a connection's SQL scripts, in its right-click menu

- The owner wants, on a right-click on a connection, every SQL script linked to it "like a small folder". A
  connection's menu now has SQL scripts ▸ beside New SQL editor: its saved queries (the last ten saved, by
  folder) then the `.sql` files last used in a tab of it; a click opens one on that connection, connecting it
  first when it is closed, and shows the tab that already holds it instead of opening it twice. D-111.
- Each connection keeps its own last ten files in the settings, written where Kerno already linked a file to a
  connection (opening, saving, pointing its tab at one): a file used with two connections is in both lists;
  older settings start them from each recent file's last connection; the toolbar's recent list is unchanged.
- The saved queries are kept in memory, read off the UI thread at start and after each change, so the menu
  never waits on the store. A file gone since is said, leaves every list and connects nothing.
- From the keyboard: "SQL Scripts of Selected Connection…" (a picker to type a name) and "Open SQL File on
  Selected Connection…". A copy starts with no scripts, a deleted connection takes its list with it, and an
  export of connections carries no path.

## 2026-11-23 — K37: where a running script stands, in the editor's margin

- A first user outside the owner, replacing DBeaver: DBeaver's ruler shows where a long script stands, Kerno's
  editor showed nothing. Now the margin left of the line numbers marks each statement the run reached the moment
  it starts and ends: running (a play sign in a halo that breathes), done (a tick), failed (a cross), cancelled (a
  square), refused on a read-only connection (a barred circle); nothing beside the statements not reached. The
  status bar's "running…" says "running statement 3 of 12 · 4.20 s", the time going up while it runs; after
  the run it says the marks in words ("statement 3 of 12 failed", "12 of 12 statements done"). D-110.
- After the run the marks stay as its trace until the next run from the editor or the first edit; an edit during
  the run clears them too. A single statement is marked as well. Nothing moves the cursor, the selection or the
  view.
- The kit's gutter marks are flat bars of one colour, so the marks are drawn by Kerno over the editor from the
  editor's own layout of the frame (soft wrap, folds, zoom, line numbers off); the kit's strip is reserved once,
  so every query editor's text sits 16 px further right.
- Measured: a script of 5,000 one-row inserts took 1,138 s before and 1,159 s after (+1.8 %, debug test harness,
  one run each: within noise; in both the rate fell from about 12 to about 3 inserts a second as results piled
  up); the margin's layout of a frame with 5,000 marked statements takes 5.4 µs.
- Tests: five unit tests, one theme test, nine UI tests (PostgreSQL and MySQL: in the middle of a run, after it,
  a failure with and without "continue on error", a cancel, a disconnect, a refused write, an edit, the next
  run, a result read again; and the drawing beside wrapped, folded, zoomed and far-down lines without a
  server), each seen failing under a mutation of its part.

## 2026-11-23 — Release 0.3.1: K35 to K38, a pass in the real app, then the version

- Asked by the owner ("release 0.3.1") once K35 to K38 were in: the four first reports of a user replacing
  DBeaver by Kerno. With them, `url` 2.5.9 for RUSTSEC-2026-0592, published after 0.3.0.
- A pass in the real app on the bundle of `016c783` (a scratch data directory, the in-memory secret store, the
  test servers): the marks of a running script move in real time and the status bar's time goes up; a table
  loads at the end of its rows by the size chosen in the footer, on `analytics.big_table` too; a connection's
  "SQL scripts" lists a saved query and a file, connects first and never opens twice; a restore of a backup
  owned by a role the server lacks fails with the plain words, and "Restore without owners and privileges…"
  restores it. Nothing blocking.
- Found by the pass and left for later: a path typed in the restore's File field is taken only at Return or
  Tab, with "Restore…" disabled until then and nothing saying so; a table tab whose connection was closed still
  shows its fetch and edit buttons; the query bar's icons move when Cancel appears; the running mark's halo is
  too faint to be seen breathing; the fetch size of a tab is not kept across a restart (as D-109 decided). One
  row of the fixture had been changed by hand before the pass (`shop.customers` 1's e-mail): no test reads it.
- Also seen in K37's timing and not looked into: a script of 5,000 statements gets slower as its result tabs
  pile up.
- The full check on the release commit `38a2740`: 1,908 tests, 0 failed. `scripts/release.sh 0.3.1`: the
  macOS disk image and six Linux packages, each started in ubuntu:20.04 and debian:13. Pushed, tagged
  `v0.3.1` and published at the owner's word.

## 2026-11-23 — K40: a table's columns stay scrolled, its WHERE completes, the close cross

- Reported on 0.3.1 by the owner and a first outside user (D-113). A sort, a filter, a refresh or an Apply on a
  table keeps the columns scrolled where they were; a result read again on the server too (no test for that one).
- The WHERE field of a table proposes its columns with their type, then AND, OR, IN, IS NULL, LIKE, ILIKE on
  PostgreSQL, BETWEEN; Tab and Enter take one, quoted when needed; Enter with the list closed applies.
- Verified: a column's order after a drag, widths, hidden, pinned and sort come back after closing the tab and
  after a restart. The close cross of a tab has a small centred hover.
- Tests: three UI tests (PostgreSQL and MySQL) and one core test, each seen failing under a mutation.

## 2026-11-23 — K41: import from DBeaver

- "Import from DBeaver…" (File ▸, sidebar, palette) reads a `.dbp` or a `data-sources.json`, previews folders and
  connections (ticked, or greyed with why), then imports PostgreSQL, MySQL, MariaDB and SQLite connections with
  their folder, environment, colour, SSH and SSL; scripts become saved queries; passwords only when ticked;
  nothing twice. D-114. Written from DBeaver's source; no real export was available to check it.

## 2026-11-23 — K44: ⌘← collapses the schema tree from the tree

- "Collapse Schema Tree" is a key of the tree (context `Navigator`, group Navigator in Settings ▸ Shortcuts) with
  ⌘← (Ctrl+Left on Linux) by default; in the tree's filter and the editor ⌘← still goes to the line's start.
- Tests: `ui_tests/k44.rs`, `cmd_left_collapses_the_tree_only_from_the_tree`, `a_key_of_the_text_fields_is_taken_for_a_command_of_the_tree`.

## 2026-11-23 — K39: a restore whose archive needs an extension the server does not have

- The K35 user's second failure of the same restore: `CREATE EXTENSION pgaudit`, not available on the laptop's
  server. Before Start, Kerno now reads the archive's list and asks the server which extensions and owning roles
  it lacks, says so above the settings, leaves the extensions out through `--use-list` (ticked by default) and
  ticks the owners' box (D-112).
- After a failure, a failed `CREATE EXTENSION` is said in plain words, and one retry leaves out the extension
  and the owners at once.
- Tests: three core tests on the real list of pg_restore 18.7 and the user's block, one unit test, three UI
  tests (two with a stand-in pg_restore and the real server's answers, one with the real tools on scratch
  databases); K35's real-tools test unticks the box the preflight now ticks. Each seen failing under a mutation.

## 2026-11-23 — K42: "Check for updates" in the About window

- The About window has "Check for updates" beside the version: the same check as Help ▸ Check for Updates…,
  its answer said there (the latest version, a newer one with Update, offline, or checks off in Settings or by
  `KERNO_NO_UPDATE_CHECK`, which send nothing); the Kerno menu has "Check for Updates…" too.
- Why 0.3.1 was not offered to a Kerno left open: the 6-hour timer counts the Mac's awake time only (GPUI's
  `dispatch_after` on `DispatchTime::NOW`), and the dialog shows once per run. The interval is unchanged.

## 2026-11-23 — K45: the palette's "Recently used"

- ⌘K with nothing typed lists first the last five commands run from the palette (`Settings::recent_commands`, ids
  only, older settings load empty), the latest first and highlighted; typing ranks as before, with no boost.
- Tests: `settings::tests::recent_commands_keep_the_last_five_latest_first_once_each`, `ui_tests/k45.rs`.

## 2026-11-23 — K43: what a newer version brings, in the update dialog

- When a newer version is found and checks are on, Kerno reads that release's notes file from its tag on
  GitHub (once per version, 5 s at most) and the update dialog, and the About window's answer, show its summary
  and the leads of "New" and "Better" as plain text; a failure leaves the dialog as before (D-115).

## 2026-11-23 — K47: a connection's environment in the sidebar

- A non-local connection's sidebar row shows its environment's name in a tag of the status bar's colours, connected
  or not, in place of the engine's name; local rows are unchanged. Test:
  `ui_tests::environments::the_sidebar_shows_an_environment_before_connecting`.

## 2026-11-23 — K46: environments in the DBeaver import

- Each connection of the preview shows its environment as a tag (dot and name); a click chooses another one.
- DBeaver's own connection types become Kerno environments (colours, auto-commit, confirm execution and data
  edits, close idle transactions) under "Create these environments", ticked; made once, never twice.
- Tests: `dbeaver::tests::dbeavers_own_types_become_environments_to_create`,
  `ui_tests::dbeaver_import::environments_are_shown_chosen_and_created_once`.


## 2026-11-23 — Release 2026.11.1: the day's reports on 0.3.1, and calendar versions

- Asked by the owner ("release 2026.11.1"). K39 to K47 in a day, from a first outside user and the owner
  trying each item in a test build: the owner checked every one himself in the app; no pass by an agent.
- After the agents' commits, done in the main session at the owner's word: ⌘← closes connections and folders
  too; the restore's list file moved from the shared temporary directory to Kerno's data directory, mode 0600
  (an automated review found it); "Faster: restore on N connections at once" in the simple restore view (450 s
  to 219 s on the owner's production backup); a restore that went to the end past ignored errors is no longer
  called a failure.
- Versions are calendar ones from this release (D-116).
- One test is timing-dependent and failed once under load, then passed three times alone:
  `ui_tests::grid_v11::a_million_fetched_rows_are_searched_off_the_ui_thread` (it asserts a background scan is
  still running right after a click). To make robust.
- Asked and left for the next release: a faster backup (pg_dump on several connections writes a directory, not
  one file).

## 2026-11-23 — K48: a faster backup, pg_dump on several connections

- The simple PostgreSQL backup has "Faster: back up on N connections at once" (off by default): pg_dump's
  directory archive with `--jobs=N`, a folder the simple restore reads back, on several connections too. The
  simple restore's "Faster" is no longer called a hidden change on the way back from the wizard.
- Measured (BENCH.md): almost no gain on the fixture database (one table holds nearly all of it), about 3.3
  times faster on eight tables of similar size. No zstd: the default client here is built without it (D-117).
- Tests: `tools::tests::parallel_jobs_go_with_the_directory_format_only`; `ui_tests::backup_simple::`
  `faster_backs_up_into_a_folder_and_the_wizard_shows_it`, `the_simple_restores_faster_is_not_a_hidden_change`,
  `a_faster_backup_removes_only_the_folder_it_made`, `a_faster_backup_is_a_folder_the_simple_restore_reads_back`.

## 2026-11-23 — K49: a foreign key seen in the grid, followed in one gesture

- A foreign key's column has a link mark in its header, its tooltip where it leads; the hovered or selected cell
  of a key that holds a value shows an arrow; the arrow, ⌘-click on the value and ⌘↓ open the
  referenced table filtered on the value, as the cell menu's entry, now worded "Open shop.customers where id =
  42". Table data on the four engines, the record view, and one-table query results (PostgreSQL, MySQL,
  MariaDB) (D-118).
- Tests: `ui_tests::k49` (three, PostgreSQL, MySQL, MariaDB, SQLite), `theme::tests::foreign_key_marks_meet_wcag`;
  each seen failing under a mutation (`docs/plans/M12.md`, K49). Frame times with and without keys alike
  (`docs/BENCH.md`, offscreen).

## 2026-11-23 — K50: the AI assistant's richer context

- The tables a request is about are described with each column's nullability, the primary key, the foreign
  keys and the indexes, read when the request opens (30 tables at most, the ones a statement names first); no
  default, comment or value. On `shop`: 2,530 bytes before, 3,510 after (D-119).
- Tests: `ai::tests::{a_table_read_in_full_says_nulls_keys_and_indexes_and_no_value,
  a_table_read_in_full_stays_within_its_caps}`, the drivers' `ai_context` tests (PostgreSQL, MySQL, MariaDB)
  and `the_ai_context_says_nulls_keys_and_indexes` (SQLite), the UI tests of `ui_tests/ai.rs`.
- After an automated review: an index part that is not exactly a column's name is sent as `(expression)`, an
  index of expressions only is left out (tests with expression and partial indexes holding literals).

## 2026-11-23 — K51: the AI assistant as a conversation in a side panel

- A panel at the right of the window holds the active query tab's conversation (one per tab, for the run):
  follow-ups go with the earlier turns within a budget (20 turns, 40,000 characters) that says when the oldest
  are left out; the editor's query as a removable chip; Insert, Replace and Copy under each SQL block; answers
  streamed when the server streams; the four "AI: …" actions open it with their message ready (D-120).
- Core API for K52 and K53: `ai::build_chat`, `Action::message`, `ai::with_query`, `ai::Turn`,
  `ai::left_out`, `ai::chat_stream`, `ai::segments`; app: `ai_view::open(tab, Draft)`,
  `QueryTab::{ai_facts, ai_statement, ai_chat}`, `AiPanel::{insert_sql, replace_sql}`.
- Tests: `ai::chat::tests` (7), `tests/ai.rs` (2 new), `ui_tests/ai_chat.rs` (9), the J17 UI tests adapted.

## 2026-11-23 — K53: the AI assistant asked from a right-click

- AI ▸ on a table, a view, a materialized view and a column of the tree, an AI button on a data tab's footer
  and a structure tab's bar, AI ▸ in a result grid's menus: ask about the table, explain it, write a query
  on it, generate test data, explain columns, a result or a selection. Each opens the panel with its subject
  as a chip; nothing is sent before Enter; a result's values only by their own tick (D-122).
- Core `ai::ask` (new): the first messages, the subject put first in the context (also before the tab's
  catalog is read), the values the tick sends. App `ai_view/ask.rs` (new) and hooks in the panel, the tree,
  the grid's menus, the data and structure tabs, the window and the palette.
- Tests: `ai::ask::tests` (8), `ai_view::ask::tests` (2), `ui_tests/ai_ask.rs` (7: PostgreSQL, MySQL, MariaDB,
  SQLite), each seen failing with a one-line mutation; captures `m12-ai-tree-menu`, `m12-ai-explain-table`, `m12-ai-result-menu`,
  `m12-ai-explain-result` (light and dark).

## 2026-11-23 — K52: ask the assistant in the editor, fix an error in one click

- "AI: Ask Here…" (⌘⇧H) opens a field under the statement at the cursor (or the selection, or an empty line);
  "Fix with AI" under a failed statement's error and beside its mark sends the statement and the error at once.
  The answer is a diff: the lines it removes tinted in the text, a card of `−` / `+` lines; Accept (⌘↩) is one
  undo step and runs nothing, Reject (Esc) changes nothing; a further sentence refines it; "Open in the panel"
  moves the exchange to the tab's conversation (D-121).
- Tests: `ai::inline::tests` (9), `query::ai_inline::tests` (3), `theme::tests::a_proposals_lines_read_on_their_tints`,
  `ui_tests::ai_inline` (10, the stand-in endpoint, the PostgreSQL fixture read only).

## 2026-11-23 — The AI panel after its first pass in the real app

- The keyboard comes back to the editor (or the workspace) whatever takes the panel away; Insert ends a
  statement left without `;` before its SQL, one undo step; Enter in "Rows to generate" sends; the panel is
  never wider than the window leaves (D-120).
- Tests: `ai::tests::inserted_sql_runs_as_statements_of_its_own`, `ui_tests::ai_chat::`
  `{the_keyboard_comes_back_whatever_takes_the_panel_away, enter_in_the_rows_field_sends_and_escape_leaves_it,
  the_panel_always_fits_in_the_window}`, the Insert tests.

## 2026-11-23, night — 2026.11.2 prepared, not published: K48 to K53, a pass in the real app

- The owner left for the night with full autonomy and one rule: prepare the release, publish nothing until
  his word. K48 (a faster backup), K49 (foreign keys seen and followed), K50 to K53 (the AI assistant: a
  richer context, a conversation in a side panel, asking in the editor with a diff and a one-click fix, the
  right-click entries), each by an agent in its own worktree, merged one by one with the full check green
  each time.
- Before anything: the fixture database of `kerno-postgres` was recreated from its seed scripts (its `public`
  schema had been dropped during the day, which had made six UI tests fail at the release of 2026.11.1); the
  code published as 2026.11.1 then passed the full check. Two tests that depended on timing were made not to.
- A user's report: the DBeaver import read an SSH tunnel's port 2222 as 22 (DBeaver writes it `2222.0`). Fixed.
- Two findings of the automated security review of the commits, both fixed before any merge or with a test:
  an index on an expression could carry a value to the model (now `(expression)`); a table tab's statement
  went with its filter's literals (now `?`).
- A pass in the real app by an agent, on a copy of the bundle, with a stand-in endpoint on this computer in
  place of a model (canned answers, every request kept for reading): everything worked, nothing crashed. It
  found, and these are fixed: the keyboard lost after the panel closed while the assistant was off; Insert
  glued to a statement with no `;`; Enter in "Rows to generate" doing nothing; the panel wider than the
  window; the Ask Here card's hint overflowing; a tab per question, all named alike. Found and left, older
  than tonight: the palette's search wants the words in order; the tab strip clips its leftmost tab when
  there are many; a long data directory leaves the single-instance socket unopened.
- The full check on the tree prepared: see the release commit's message for the count.

## 2026-11-23 — K54: a foreign key's arrow leaves with the pointer

- The arrow of a hovered foreign-key cell (K49) stayed on its row after the pointer left: once its tab was
  shown again (after following the arrow), after a double click and Escape, under rows scrolled or read again.
  The hovered cell now checks for the pointer each frame it is drawn and hears every move until the next one;
  it is drawn only while the pointer is over that very cell (`docs/plans/M12.md`, K49, corrected).
- Tests: `ui_tests::k54` (3, PostgreSQL and MySQL, fixtures read only), each seen failing under a mutation.

## 2026-11-23 — K54: the Edit menu's standard entries

- The Edit menu starts with Undo, Redo, Cut, Copy, Paste and Select All, with their keys, as system actions:
  they act on the focused SQL editor or text field, and on a table's or a result's grid for Copy, Paste and Select
  All (the same code as the grid's keys); greyed where nothing focused takes one. The keys do what they did.
- Tests: `ui_tests::k54::the_edit_menu_has_the_systems_editing_entries` (PostgreSQL fixture read only, the pending
  changes left unapplied), each part seen failing under a mutation.

## 2026-11-23 — K57: a Kerno left open learns of a new version

- Two users had to restart Kerno to be offered an update: the 6-hour timer of the checks stops while the
  computer sleeps, and the dialog showed once per run. The checks now follow the wall clock: looked at every
  minute and when the window is activated again, one is sent when six hours have passed since the last, within
  two minutes of a wake, never inside the interval (D-123). A newer version found later in the same run opens
  the dialog too; a version downloaded but not installed gives way to a newer one, its download removed.
- Tests: `update::tests::a_timed_check_is_due_after_six_hours_of_wall_time_and_not_right_at_a_wake` and four UI
  tests in `ui_tests/updates.rs` (one renamed, one moved onto the wall clock), each seen failing under a
  mutation (`docs/plans/M12.md`, K57). No request leaves the machine in them (the fake runner).

## 2026-11-24 — K56: seven small defects of dialogs and tabs

- In a window less than about 750 px high, a failed "Test connection" pushed the connection dialog's buttons
  out of the window. The fields now scroll above the result and the buttons, which stay in view down to 560 px;
  the other tall dialogs were checked at that height and pass.
- A path typed in the restore's File field was taken only on Enter or when the field lost the focus. It is
  now taken once the typing pauses, and a path that names nothing says "No such file".

## 2026-11-23 — K55: four small defects of the window's chrome

- The command palette finds the words typed in any order, in a command's name, its section or its keywords,
  accents aside ("theme light", "sql new"); Escape closes it at once, with text typed or not (it cleared the
  text first). Tests: `ui_tests::k55` (3), each seen failing under a mutation.
- With many tabs open, the strip no longer starts inside a tab (a bare "×", "uery 2"): once scrolled to a tab
  (a reveal, a click) and still, it starts on a tab's start with the active tab whole, in the three layouts.
  Tests: `ui_tests::k55` (one more), `tabs::tests`, each seen failing under a mutation.
- A table tab whose connection was closed kept offering Fetch more, Fetch all, Duplicate, Delete, Set NULL and
  Set default. Its rows now stay readable under a bar with Connect, and everything that needs the server is
  greyed with the reason (refused from the keyboard too); Connect reads the rows again where they were.

## 2026-11-24 — K59, part 1: the result's filter proposes its columns

- The field "Filter the result on the server" is now the table's WHERE field (K40): it proposes the result's
  columns with their types, then the words of a condition, with the same keys; Ctrl+Space (⌘I) opens the list
  in both fields. Found on the way, in both fields: Enter that took a proposal also applied the condition as it
  was before (MySQL showed it); Enter now only takes the proposal (`docs/plans/M12.md`, K59; D-125).
- Tests: `ui_tests::k59` (2, PostgreSQL and MySQL, fixtures read only), each seen failing under a mutation.

## 2026-11-24 — K58: the DBeaver import takes jump servers, and says what it leaves out

- A DBeaver tunnel's jump servers (`jumpServer<i>.` properties, read as DBeaver's `SSHUtils` reads them) now
  come as the connection's jump hosts through the system's OpenSSH, a chain in order, as Kerno's dialog saves
  them; what a hop logs in with that Kerno cannot give it (its own key file, agent socket, password) is said
  with what to do; behind a tunnel that logs in with a password they are left out, said (D-124).
- Every note of the import says what was left out, then what to check, once per connection: under each
  connection of the preview ("To check: …") and connection by connection in the report.
- Tests: core `dbeaver::tests::a_tunnels_jump_servers_become_openssh_jump_hosts`, the double-port test
  extended to the hops, UI `the_preview_and_the_report_say_what_to_check_for_each_connection`, each seen
  failing under a mutation (`docs/plans/M12.md`, K58). Fixtures written from DBeaver's source.
- Nothing in a query tab's bar moves when a run starts or ends: Cancel (or Stop) sits left of the history and
  saved-queries buttons, as wide whatever its word, and its room is kept while nothing runs. Test:
  `ui_tests::k55` (one more, PostgreSQL), seen failing under a mutation.
- The toolbar's "New SQL editor" opened on the active tab's connection even with another selected in the tree.
  It now opens on the tree's selected connection when the keyboard was last there (DBeaver's way), on the active
  tab's otherwise, and its tooltip names it.
- The running mark (K37) is seen to breathe: 1.5 s a breath, six of the tab's redraws, from no halo to 50 % of the
  info colour and wider, held still when the system asks for less motion (D-127). Test: `theme::tests`, seen
  failing with K37's breath. Captures light and dark of the strip, the bar and two frames of the mark, offscreen.
- "Test connection" said a password just typed in the dialog came from Kerno's file (or the keychain). It now
  says "Password typed here", and still says where a saved one, untouched, really came from.
- Each save panel (an export, a backup, a schema document, a diagram, the connections' export, a saved script, a
  new database file) opened in the home folder. Each kind now opens in the folder it last saved in, the home
  folder when that folder is gone.
- With a long data directory, the single-instance socket could not be opened ("path must be shorter than
  SUN_LEN" at each start) and `kerno <url>` no longer reached the running Kerno. Such a directory now gets a
  short socket in a private folder of the user's runtime or temporary directory, found the same way by both.
- Tests: `ui_tests::k56` (8; PostgreSQL where a server is needed, fixtures read only) and the core test
  `instance::tests::a_long_data_directory_gets_a_short_socket_of_its_own`, each seen failing under a mutation
  (`docs/plans/M12.md`, K56).

## 2026-11-24, night — K60: data compare

- The rows of two tables of the same engine family compared by a key, item 1 of the roadmap (TOOL-22), the
  owner's request of 2026-11-23: "Compare data with…" on a table and in a data tab's menu, Server ▸ Tools ▸
  Compare Data… and the palette; a dialog as the schema compare's with the table, the key and the columns; a tab
  with the counts and the rows that differ, one per key, a changed cell as `source → target`, the filters, a
  row's values and its copies; "Open script" opens the statements that make the target like the source in a
  query tab on the target, nothing run (D-126).
- Both tables are read at once, each by one `SELECT` in the order of its key, and merged as they arrive: a batch
  per side and the differences kept (10,000 at most) are all that is held. The order is one no collation
  changes (numbers by value, the rest by the bytes of its text) and is checked as the rows come, so a server
  that orders otherwise stops the compare instead of giving wrong counts; proved with keys that sort one way in
  en_US or a `_ci` collation and another in bytes, on PostgreSQL and on MySQL against MariaDB.
- Measured: 1,000,000 rows of `analytics.big_table` against a scratch copy with 4,000 planted differences in
  1.7 to 2.9 s, the whole test process at 19 MiB of peak memory at most (`docs/BENCH.md`).
- Tests: `data_compare::tests` (16), the driver round trips (PostgreSQL; MySQL against MariaDB: compare, run
  the script on the target, compare again, nothing left), `ui_tests::data_compare` (7), each seen failing under
  a mutation (`docs/plans/M12.md`, K60). Scratch schemas and databases `kerno_test_dc_*` only, dropped also on a
  failure; the fixtures are only read.

## 2026-11-24 — K59, part 2: foreign keys marked in more results

- K49's marks and arrows now come on every result whose columns the engine says come from a table: a join, a
  few columns, an alias, a result that cannot be edited, a read-only connection, SQLite (its column metadata,
  a feature of the crate already there). A key with a column missing says which; a column in two keys offers both
  from its arrow; a binary key is followed with the engine's binary literal. PostgreSQL names a result's tables
  in one query; the keys come from the tab's completion catalog (`docs/plans/M12.md`, K59; D-125).
- Measured: 300 columns over 20 tables named and matched in 1.7 ms (debug build).
- Tests: `ui_tests::k59` (7 in all), `links::tests` (4), three driver tests; each new one seen failing under a
  mutation.

## 2026-11-24, night — K61: two defects the pass in the real app found

- After a click on the first tab in view the strip still started with a bare "×", the end of the tab before
  (K55): the kit's reveal of a click does not move on its first frame, which counted as "stopped". That frame no
  longer counts; the K55 test now moves the test clock between its frames, which it did not (the reveal never
  moved there), and failed without the fix. Seen in the release build before the fix and gone after it.
- Edit ▸ Copy copied nothing where ⌘C copies on its own (K54): in the schema tree, a structure tab's and an
  administration tab's lists the menu's Copy reached only the window's copy of selected label text. Each now
  routes it to what its ⌘C does (the selected object's name, the selected cell). The other surfaces have no ⌘C,
  Paste or Select All of their own. Test: `ui_tests::k54::edit_copy_does_what_the_key_does_on_every_surface`,
  each of the three seen failing without its handler.

## 2026-11-24, night — 2026.11.2 prepared a second time, not published

The owner's word of the evening of 2026-11-23, from his phone: all five things proposed for the night go
into the morning's release, which stays unpublished until his go.

- In, each merged with the whole check green on `main` in a build directory of its own (2,026 tests in the
  evening, 2,100 after the last merge of the night): K54 (a foreign key's arrow that left with the pointer,
  the Edit menu's standard entries), K55 (the palette's words in any order, the tab strip, the query bar,
  the running mark; D-127), K56 (seven defects of dialogs and tabs; D-128), K57 (an update found without
  restarting; D-123), K58 (the DBeaver import's jump servers; D-124), K59 (the result's filter and foreign
  keys in every result; D-125), K60 (data compare; D-126), K61 (two defects of the night's pass).
- Reviews. The automatic review of the K58 commit found two defects, fixed the same hour: a jump host after
  a comma or after `user@` could start with `-` (each jump host is now checked by itself), and the note about
  a hop's key wrote the imported file's path inside a command to paste. An independent review of K60, asked
  before it could ship, found that the script's UPDATE and DELETE could match several rows of the target (a
  key unique on the source only; keys the server holds equal and whose bytes differ), that a MySQL FLOAT was
  read with six digits, and six smaller things: all fixed by K60's author in three commits, the first proved
  by running the script on scratch targets under `_ai_ci`, PAD SPACE and citext keys and comparing again.
- A pass in the real app on the build of 02:04 (parts A to K of the night's brief): the keys the Edit menu
  now declares act once everywhere; nothing blocking; the two defects it found are K61. Left as they are,
  cosmetic: the theme chosen while the DBeaver import dialog is open is taken only once it is closed; "Open
  script" twice gives two tabs with one title; the running mark's halo looked faint in two dark captures
  taken 0.7 s apart (the phase of the captures is not known).
- Two tests that failed once each under the night's load and pass alone: the folding test clicked before
  the parse had ended (it now tries until its mark is there); `transfer::copy_tables_opens_from_tools_and_
  from_the_palette` timed out once and was not seen again.
- Not verified in the real app: K61's Edit ▸ Copy on the tree (its tests only), a real sleep for K57, and
  the last form of data compare after its review (the pass ran on the build before the fixes).

## 2026-11-24 — K62: which databases a PostgreSQL connection shows

- From a user's report: an account allowed into one database of twenty saw them all, and learnt which ones it
  could not enter only by clicking each (a red line). A database the account may not open (the CONNECT
  privilege, or `datallowconn`) is now dimmed and opens on a muted line that says so, with no connection tried;
  a session refused anyway says "You may not connect to database “x”." first.
- A PostgreSQL connection chooses what its tree and its database pickers show: All (the default), Those this
  account can open, Only the default database; on the Database field's line of the dialog, applied on save
  without a reconnect, with "N databases hidden" after the databases (a click opens the dialog). The DBeaver
  import follows DBeaver's "Show all databases"; the connections file carries the choice
  (`docs/plans/M12.md`, K62; D-129).
- Tests: a driver test as a role of its own, core and app unit tests, `ui_tests::k62` (2), each seen failing
  under a mutation; captures `final/m12-databases-shown-*`.

## 2026-11-24 — K62, after its review: a global backup says what it left out

- The review of K62's first commit: the global backup left the databases this account may not open out of
  pg_dumpall's run without a word, and froze them into a saved task's list. It now says so under its list of
  databases ("Left out, this account may not open it: billing"; the first three and "and N more", all in the
  tooltip) and as the first line of the run's log, which a task's run keeps; they are read from the server
  at each opening of the wizard and added to that run only, a task keeping only what the user unticked.
- The "Databases shown" button with its longest label and a 22-character database name, captured light and
  dark at the default size: the name reads in full; the wording stays.
- Tests: `ui_tests::k62` (the backup part: the line, the task's list, the log of a real run of one small
  database), `backup_dialog::tests::the_databases_left_out_are_named`, each seen failing under a mutation.

## 2026-11-24 — GitHub Actions: the checks and the release builds

- The owner lifted his "no GitHub Actions" rule (free on a public repository) and asked for the builds and
  the releases on GitHub; the sentence of `CLAUDE.md` §5 is replaced at his word (D-130).
- `.github/workflows/ci.yml` (fmt, clippy, deny; tests on macOS without the containers, on Linux with them)
  and `.github/workflows/release.yml` (a tag builds the `.dmg` and the six Linux packages with the local
  scripts, checks them, writes `SHA256SUMS`, opens a draft release). Actions pinned to commits.
- Proved on GitHub from a branch: lint and the macOS tests green; the release dry run green on its second
  try, its eight files downloaded, the checksums verified, the `.dmg`'s signature and version checked, the
  dry-run draft deleted. The first try failed on one check only: the x86_64 AppImage under the runner's
  emulation; x86_64 packages are now started on an x86_64 runner (`scripts/check-linux-packages.sh [arch]`).
- Found: 13 tests of the app fail on Linux, where the tests had never run (K65). A probe on a Windows
  runner: every dependency compiles, 27 errors in `kerno-core` (K64).

## 2026-11-24 — K64: Windows, step 1 (the workspace builds and `kerno-core`'s tests pass there)

- The owner's word: releases for macOS, Linux and Windows from CI; step 1 first. This Mac cannot compile for
  Windows (ring and SQLite need a Windows C toolchain); a GitHub runner did, through the probe workflow
  `.github/workflows/windows.yml` on the `windows` branch (not for `main`; two jobs on `windows-2025`).
  Every crate and target compiles for `x86_64-pc-windows-msvc`, clippy is clean there, `kerno-core`'s tests
  pass there, and a release `kerno.exe` prints `kerno 2026.11.2`. Nobody has seen Kerno on a Windows screen
  (D-131).
- One home per Unix concern, with a Windows side: private files and folders (`kerno_core::private`), the
  running instance's channel (a named pipe, both ends proving they can read a token of the data directory,
  the URL sent without its password), the SSH agent (a named pipe of this machine only), the system OpenSSH
  tunnel (Unix only: Windows says it is not available yet). The Unix sides are today's code, moved.
- Reviews during the work, all folded in before the report: the URL's password never crosses the pipe; a pipe
  that cannot prove it is this user's Kerno gets nothing and the second Kerno opens the URL itself; an agent's
  pipe from an imported connection or `SSH_AUTH_SOCK` must be a pipe of this machine (a host's pipe would send
  the user's credentials to it over SMB). A first run that looked green was not: `continue-on-error` reported
  every step a success while clippy had failed (two `mut` only Unix uses) and one test had (a `/bin/sh`
  script); none is left in the workflow.
- macOS: fmt, clippy (`-D warnings`) and `cargo deny` clean; `kerno-core` 701 unit tests and its integration
  tests, `kerno` 945, `kerno-postgres` 148, `kerno-mysql` 198, `kerno-sqlite` 30, all passing. Not done on
  Windows yet, in order: a CI job and a zip, a pass in the real app, the home folder and the update check, the
  local clients and the password presets, an installer and the update's install, the system OpenSSH tunnel.

## 2026-11-24 — K65: the tests pass on Linux

The workspace's tests on GitHub's `ubuntu-24.04` runner with the `kerno-*` containers (legacy MySQL too), from a
probe workflow on the branch `linux-tests` (not merged). On 4aeff7e the app crate ended 932 passed, 13 failed;
with `--no-fail-fast` four more failed in `kerno-core`, `kerno-mysql` and `kerno-postgres`. Now the whole
workspace is green there: 2,101 passed, 3 ignored (the three measurements ignored on every system): `kerno`
945, `kerno-core` 780, `kerno-mysql` 198, `kerno-postgres` 148, `kerno-sqlite` 30.

- Defects of the Linux app, fixed: Query History took Ctrl+Y, the text fields' only Redo on Linux (now
  Ctrl+Alt+E there, ⌘Y on macOS); a cancelled backup into a new file and a folder picture written just after a
  prune depended on Linux's coarse file clock (both now independent of it; the code runs on macOS too, with
  nothing changed for its user).
- Tests that assumed macOS now say what is true on each system (keys of the kit's text fields and editor,
  hashed known_hosts, Debian's `pg_wrapper`, the command box's padding, a click on a field cut at the page's
  edge); none weakened, none skipped on Linux.
- What the runner lacked: PostgreSQL 17's client from apt.postgresql.org (key checked by fingerprint) in
  `scripts/install-linux-deps.sh`; an X.509 v3 client certificate from `scripts/test-env.sh`.
- macOS: fmt, clippy and `cargo deny` clean; `kerno` 945 passed, `kerno-core` and `kerno-postgres` green.
  Nobody opened the Linux app; D-132 has the details.
- One run of four on the same code showed four timing races of the slower runner: two fixed (a single table's
  backup showed `--schema` while the tables were still listed; a test clock advanced too early), two seen once
  and left (D-132).

## 2026-11-24 — K66: the Windows zip in the release

- The owner: the Windows package belongs in the release, as he had asked. `scripts/bundle-windows.sh` builds
  `kerno-<version>-windows-x86_64.zip` (`kerno.exe` with its icon and the C runtime inside), launches the app
  once and keeps a picture of the screen; `release.yml` has a `windows` job and lists the zip in `SHA256SUMS`;
  `ci.yml` builds it on every push (D-133).
- Proved on GitHub: the CI green with the job; the launch check's picture looked at (the window is drawn);
  a release dry run's nine files downloaded, their checksums verified, the zip and the executable inspected.
- Not proved: the dry run's draft from a branch (HTTP 403, D-133); the app in a person's hands on Windows.

## 2026-11-24 — 2026.11.3 prepared: the first release built by GitHub Actions

- At the owner's word ("go pour la release"): version 2026.11.3 and its notes
  (`docs/release-notes/v2026.11.3.md`): K62 (the databases a PostgreSQL connection shows, a closed database
  said before any click), K65 (the Linux fixes, Query History on Ctrl+Alt+E there), K64 and K66 (Windows, a
  preview: one zip). The READMEs say what the Windows preview is and is not.
- The tag `v2026.11.3` makes `release.yml` build the nine files and open a draft: the first real use of the
  tag's path (the dry runs took the other one). Publishing waits for the owner, who tries the draft first.

## 2026-11-24 — A release in about 15 minutes (D-134)

- 2026.11.3 was published (the first release built by GitHub Actions: a tag, a draft with nine files, the
  owner's "je valide"). He asked why it took 25 minutes and to make it faster.
- The Linux architectures are built side by side (`scripts/bundle-linux.sh [arch]`, a matrix); macOS and
  Windows start from the dependencies compiled on `main` by two package jobs of `ci.yml` (`macos-package`
  is new). Measured on a dry run from a branch: Windows 7.5 min, Linux 11 and 10 min, macOS's build 4.5 min;
  that run's macOS job then failed in `hdiutil create`, which is now retried.

## 2026-11-24 — K67: Windows finds the home folder, its updates and the backup tools

- The owner's pick for the next release ("A1 A2"). One owner of the home folder (`kerno_core::home`:
  `%USERPROFILE%` on Windows, `HOME` on Unix); libpq's `%APPDATA%\postgresql\pgpass.conf` read there, no MySQL
  option file; the update check through `System32\curl.exe`; `pg_dump.exe`, `mysqldump.exe` and the others
  found in the installers' folders of Program Files and on `PATH`, started without a console window (D-136).
- Proved on GitHub's `windows-2025` runner by a probe workflow on the branch `windows` (not merged), against a
  PostgreSQL 17 and a MySQL 8.4 it starts itself: the clients found with their versions, a backup and a restore
  through Kerno's own code for both engines, a cancelled backup that leaves no tool in `tasklist`, a version
  found through the system's curl, the home and libpq's file read; check, clippy and `kerno-core`'s tests green
  there. A deliberate red run showed each of those tests failing under its mutation.
- macOS: fmt, clippy and `cargo deny` clean; `kerno-core` and `kerno` green. Not seen: the Windows screens (the
  dialog's hint, Settings ▸ Local clients, no console window), MariaDB on Windows, the password command's presets.

## 2026-11-24 — K73: the macOS disk image opens on a designed window

- The owner's request of 2026-11-16, picked for the next release: the `.dmg` opens on a window of its own,
  a background from the brand with an arrow from Kerno to Applications, no bars, 128-point icons in place,
  Kerno's icon on the volume (D-142).
- The layout is a `.DS_Store` Finder wrote once on GitHub's runner (`packaging/macos/make-dmg-layout.sh`),
  committed and copied into every image by `scripts/bundle-macos.sh`: no Finder at build time. The script
  then checks the compressed image as it ships and fails, never falls back to a plain image.
- Proved on the `macos-15` runner (a probe workflow on the `macos-dmg` branch): the script's image opened on
  the designed window in light and dark. Locally: the full script passes, each of its checks seen failing.
  Not proved locally: the picture itself, hidden by a mounted 0.1.0 image of the same volume name.

## 2026-11-24 — K72: test data for a table

- The owner's pick for the next release (ROADMAP TOOL-23): "Fill with test data…" on a table, from the tree, a
  data tab's menu and the palette (Generate Test Data…), on PostgreSQL, MySQL, MariaDB and SQLite. The rows are made here from a
  seed, exact about the table's types, keys and foreign keys; a preview follows the settings; "Open script"
  opens the `INSERT` statements in a query tab and runs nothing, "Insert…" inserts them in one transaction
  under the environment's rules (D-141).
- Proved by the core's unit tests (every generator, bounds, uniqueness, the NULL share, the script), a driver
  test per engine (1,000 rows of every common type inserted and their rules kept, the script of 1,000 more run
  as written, an empty parent refused, a refused row keeping nothing) and six UI tests, each seen failing
  under a mutation; captures light and dark.
- Not done: values read from a file or another table, CHECK rules kept, settings saved as a task (D-141).

## 2026-11-24 — K71: the tests that failed now and then

- The owner's pick C1: the CI must not go red by chance. Seven tests were looped on this Mac (one copy at a
  time, beside the other agents' work) and on GitHub through the probe workflow on `linux-tests` (a CPU hog
  per core; Linux with the containers, and `macos-15` for the DBeaver import) until each failed, then fixed
  where the wrong thing happens (D-140).
- Four defects of the app: a PostgreSQL close let a statement whose describe was cancelled be sent and wait
  for a lock unseen (the FK preview's stopped read); a table locked elsewhere held every PostgreSQL schema
  listing (Copy tables, the tree, the backup dialog), and an object dropped while it was listed failed it; a
  second DBeaver import at once could copy a connection; a run on several connections said "Starting…" while
  it asked. Three tests read a state before
  it was there (the update relaunch, the fold marks, MySQL's own transaction after a sizes read).
- Proved by the loops (numbers in M12, K71), four new tests seen failing without their fix, and the probe's
  last run on the final code (45468100079): every loop 0 failures, the whole workspace on the Linux runner
  2,120 passed, none failed. Left: J15's import plans from the list in memory too (not reproduced).

## 2026-11-24 — K63: a faster backup and restore for MySQL and MariaDB, an old server's client said

- A user's request relayed by the owner ("B1 B2"): MySQL and MariaDB get PostgreSQL's "Faster". Kerno runs N
  `mysqldump` at once into a folder, one SQL file per table, a large table with a single-column integer key cut
  into key ranges, the routines, triggers, events and views after, `kerno-backup.json` last; the restore loads
  the folder on N `mysql` clients. "Block writes during the backup" holds Kerno's own `FLUSH TABLES WITH READ
  LOCK` (RELOAD; asked first); off, the wizard says in plain words how each table is read. From MySQL 5.1 on
  (D-135).
- An old server needs an old client (the owner's "B2"): mysqldump 8.4 and later (and MariaDB's 11.8) read no
  column of a MySQL 5.1 table, reproduced on the legacy servers; the backup says so before it runs, refuses to
  start, and names a MySQL 5.7 client found. "prefer" goes without TLS for the tools on 5.1 and 5.5.
- Proved against the servers: a scratch database backed up and restored identical (rows, checksums, view,
  routines, trigger, event) on 8.4, MariaDB 11 and 5.1 to 5.7; a cancel leaves no mysqldump, no folder, no lock
  (8.4 and 5.1); a broken file stops the restore and says what is in; a refused client command loads nothing.
  Every new test seen failing under a mutation (docs/plans/M12.md).
- Measured (BENCH.md, MySQL 8.4, 8 processes): the fixture's analytics database (one 238 MB table) backed up
  in 0.8 s against 1.8 s, restored in 6.5 to 8.4 s against 14.3 to 17.1 s; eight tables of 30 MB about as much;
  on 5.1 about 1.7 and 2 times faster (one run).
- Not verified: a GTID server, MySQL 8.0's clients on 5.1, an SSH tunnel, a pass in the real app.

## 2026-11-24 — K63: the review's nine findings fixed

- An independent review of ccc0b47 found nine faults, three confirmed by a run. Fixed on top, each with a test
  seen failing before its fix (docs/plans/M12.md, the K63 table): a named table that is gone now stops the
  run (F1); the read lock is watched, and a lost one fails the backup (F2); the tools talk `utf8mb4`, so a 5.7
  client no longer writes a 4-byte character as `?`, in the plain backup too (F3); inside one table every range
  reads one moment, under Kerno's short lock of that table, and only InnoDB tables are cut (F4); a panicked
  task fails its file (F5); the plain backup fails on "it has no fields" (F6); the extra arguments may not set
  what the folder sets, in any spelling (F7); the lock is taken before the tables are listed (F8); a lock that
  does not come within 10 s stops the start, and the confirmation names the long queries (F9).
- Added: a test that the lock is held during the run, a restore into a database that is not empty (said
  before the run), and the size threshold's real path. D-135 amended (F4's rule above all).

## 2026-11-24 — K63: the second review's ten findings fixed

- A second review of the fixes found ten more, two confirmed by a run (a row doubled on MySQL 5.1; the
  replication options stalling each range 30 s). Fixed on top, each test seen failing on the code before it
  (docs/plans/M12.md, the second K63 table): a table is cut only where its ranges read one moment, by server
  version, engine and consistency (D-135's table: before 5.5.3 only under "Block writes", where any engine is
  cut; never under "No locks") (G1, G7); the folder refuses the replication position and `--flush-logs`, both
  backups `--log-error` and `--verbose` turned off (G2); the restore's words follow the backup's content, in the
  wizard and the confirmation (G3); the lock's watcher starts after the catalog and takes no answer for no
  evidence (G4); single files fill the processes a cut table waits for (G5); "it has no fields" fails only a
  MySQL backup, on mysqldump's own line (G6); long `Execute` statements are named, an account without PROCESS
  is told what Kerno cannot see, an error is said (G8); harmless arguments are taken (G9); a utf8mb4 backup
  into a server before 5.5.3 is said, `--ignore-table` refused, the lock test checks what the server saw (G10).
- BENCH: one cut table among eight, and its key skewed, measured again.
- After the re-run: a lock check that never answers is cancelled and the lock released within bounds, so the
  run always ends; the refusal of a replication position says what is true with and without "Block writes";
  tests added for `Execute` in the long statements, the Consistency sentences and a folder in utf8mb4.
- A test run with that fix taken out held the server's read lock on the 8.4 test server for 1 h 41 min (its own
  cleanup waited behind it, and every other MySQL test with it): the folder backup's driver tests now run under
  a guard that kills the test's connections when its body ends or after 120 s, from a thread of its own; the
  dialog's close of the session is bounded too.
- The CI of `main` after K63: on Windows clippy named two test constants used only by Unix-gated tests (now
  gated the same way); on Linux the MySQL 5.1 server blocked the runner ("Host … is blocked because of many
  connection errors") once the folder backup's tests had added to the connections that fail on purpose: a 5.x
  server counts them only for a host whose name it can resolve, which a runner's is and this Mac's is not. The
  four legacy test servers start with `--max_connect_errors=1000000`.

## 2026-11-24 — K70: a Windows installer, and the update installed from the app

- The owner's pick "A4": `Kerno-<version>-windows-x86_64-setup.exe` (Inno Setup, as Recto and Vigie; per user,
  no administrator, into `%LOCALAPPDATA%\Programs\Kerno`; uninstalling keeps `%APPDATA%\Kerno`), built by
  `scripts/bundle-windows.sh` after the zip and checked there (silent install, files, shortcut, uninstall entry,
  a launch, silent uninstall). On Windows Update now downloads the installer, checks its SHA-256 against
  `SHA256SUMS`, runs it for the user while Kerno quits, and the installer starts the new Kerno; a failure starts
  the old one, which says why. A Kerno running as administrator keeps to the release page (D-139).
- Proved on GitHub's runner (the probe of the branch `windows-installer`): the install check, and the update
  against a fake release served on the runner itself (a wrong checksum refused, another Kerno running stops
  the installer, 0.0.0 updated to this version). The Kerno that updated there is a test build (no click, says
  0.0.0, no elevation check: the runner is administrator only).
- Not proved: a standard user's update on a real Windows, the installer's pages clicked by a person, what
  Windows' Restart Manager does to a running Kerno, SmartScreen's words on the unsigned installer.
- K70 merged after its second review; the workflows take the installer: `release.yml`'s `windows` job uploads
  `Kerno-<version>-windows-x86_64-setup.exe` beside the zip and `SHA256SUMS` lists it (nine packages, ten
  files in a draft: without that line a Kerno on Windows refuses its update and opens the release page);
  `ci.yml`'s `windows-package` keeps the installer and the picture taken after the install. K68's `windows`
  job (the workspace's tests against the seeded servers) is in with it.

## 2026-11-25 — K75: the tree keeps no closed session, and Reconnect mends it

- The real-app pass saw every schema not read yet say "connection closed: the session was closed", Refresh and
  Reconnect no help. Reproduced in a UI test with the same row: on PostgreSQL by each way that opens an open
  connection again, alone (the Read-only switch from the toolbar or the menu, saving its settings, Reconnect);
  on MySQL by the toolbar's switch. The cancelled `pg_sleep` and K71's close play no part.
- Cause: those ways close and open the connection in one update, and the tree learns of it only once the status
  reads `Connecting`; it kept its sessions, closed. Now a tree read through another session of the connection
  than the one it holds is read again from the databases (`navigator/view.rs`, D-144).
- A tree read whose session the server ended opens a new one and reads once more, without asking; reads only.
  A second failure says "The connection to “name” was lost." and what to do, then the driver's words
  (`kerno_core::error::explain`, `FailureKind::Lost`).
- A data tab reading at the moment its connection opened again kept the session it was opening on the old one:
  it now drops that read and reads again on the new connection. Query tabs checked, unchanged.
- Tests: `ui_tests/k75.rs` (five, PostgreSQL and MySQL, SQLite for the tree), two unit tests; each seen failing
  with its part taken out.
- The first run of the Windows job over the whole workspace on `main` (after K63 and K70): three tests failed
  there, none a defect of the app. A folder-backup test needs a server that compares table names with case
  (it now says so and stops on one that folds them, as MySQL on Windows does); a plain-backup test is built on
  a stand-in mysqldump, a `/bin/sh` script (it stops on Windows as the other stand-in tests do); the tree
  filter's test looked at the tree before its schemas were read (it now waits for the row).
- Two reviews after: the installer's own code in the download's folder, the user's TEMP given back to the Kerno it
  starts, the install check behind a guard, Windows' programs only from C:\Windows, a bounded wait, and a failed
  version left on "Release page" until a check is asked for (D-139's review blocks).

## 2026-11-25 — K76: "Fill with test data" after a pass in the real app

- A pass in the real app found six faults in K72's dialog and around it, all fixed (D-141 amended): a line's
  max length ran under NULL % (every line now fits at the window's smallest size, 720 × 440, and the body
  scrolls under buttons that stay in view); the disabled NULL % looked enabled in the dark theme; "the same
  settings give the same rows" was not true of unique values, which continue after the rows already there (the
  sentence now says so, and the core makes it exact: only unique values are drawn after the rows there, and
  only they are drawn again on a repeated key); the seed and the settings were new at each opening (kept per
  table until Kerno quits, a sequence's start left as guessed following the table); a SQLite text column
  holding a date got random words (guessed from its default, then left to it, or from its name); the tree's
  "N databases hidden" tooltip stayed over the connection's dialog its click opened (the tree's tooltips now
  come from `tip.rs`, which draws nothing once a dialog is open).
- Proved by two core unit tests and three UI tests, each seen failing under a one-line mutation (M12, K72),
  K72's tests unchanged and passing, and headless captures light and dark at 720 × 440 and 1280 × 800.

## 2026-11-25 — K74: a MySQL folder backup restored again into a full database

- The pass in the real app found it: restored a second time into the same database, a folder backup failed 1
  run in 2 on a deadlock between two tables linked by a foreign key, and its message called an empty table
  "partly loaded". Reproduced in a driver test (MySQL 8.4: ERROR 1213 in 5 runs of 21 and 2 of 31; MariaDB 11:
  ERROR 1205 in 11 of 31; MySQL 5.7: none; none into an empty database in 93): the server's metadata locks
  between files that drop and create linked tables side by side. Dropping every table first was not enough
  (MariaDB, ERROR 1100 in 2 of 31) (D-143).
- The restore now runs every table's definition first, one after the other on one connection, then the rows on
  N clients, as before; it finds where each file's definition ends and feeds the client parts of the files, no
  temporary file. 31 restores in 31 on each of the three servers after the fix.
- A stopped restore says, as the database holds them then, which tables are loaded, partly loaded, empty,
  missing or as they were, and to run it again. The wizard reads the database's tables again after each run
  and says how many of the backup's tables are there and that they are dropped and replaced, and so does the
  confirmation; the backup's Speed hint no longer points at a hidden row.
- Not verified: a database of hundreds of tables (the definitions are now one after the other), a
  `mariadb-dump` folder, the real app.
