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
