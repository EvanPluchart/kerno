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
