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
