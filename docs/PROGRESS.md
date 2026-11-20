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
