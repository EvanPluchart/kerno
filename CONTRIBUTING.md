# Contributing to Kerno

Kerno is a native database client in Rust with GPUI. Code, comments, commits, docs and UI text are in
English. [CLAUDE.md](CLAUDE.md) is the master spec (hard constraints, v1 scope, quality bars); this file
is the practical part.

## Build

The Rust toolchain is pinned in `rust-toolchain.toml` and installed by rustup on first use.

```sh
# Linux (Debian or Ubuntu): GPUI's native dependencies, once, as root
sudo scripts/install-linux-deps.sh

nice -n 10 cargo build -p kerno
nice -n 10 cargo run -p kerno
```

`.cargo/config.toml` caps cargo at 3 jobs. Running long commands under `nice -n 10`, checking `uptime` before
a heavy step, running one instance of the app at a time and stopping what you started are project rules
(CLAUDE.md §1, "Machine hygiene").

For checks of the real app without touching your own data, point it at a scratch directory and the in-memory
secret store:

```sh
KERNO_DATA_DIR=/tmp/kerno-scratch KERNO_SECRET_STORE=memory nice -n 10 cargo run -p kerno
```

`scripts/capture.sh` runs the app that way and captures its window.

## Test

Unit tests need nothing. Integration tests of the drivers and the app run against containers named `kerno-*`
(PostgreSQL 17 on 55432, MySQL 8.4 on 53306, MariaDB 11 on 53307, an SSH server on 52222), defined in
`docker-compose.yml`, bound to 127.0.0.1, with test-only credentials. Start and stop them with:

```sh
scripts/test-env.sh up       # generates test certificates and SSH keys, starts, waits until seeded
scripts/test-env.sh status
scripts/test-env.sh down     # stops and removes the containers
scripts/test-env.sh reset    # down, and deletes the data volumes (forces a re-seed)
```

Then:

```sh
nice -n 10 cargo test --workspace -- --test-threads=3
```

Run `scripts/check.sh` before a commit: it runs `cargo fmt --check`, clippy, `cargo deny` and the tests (with
the containers) locally and stops at the first failure. There is no hosted CI. `--no-tests` skips the tests.

Tests that need a container return early with a message when it is down. Set `KERNO_REQUIRE_CONTAINERS=1`
(`scripts/check.sh` does) to make that a failure. Stop the containers when you finish. Never point a test or the app at any
other database: Kerno's tests connect only to the `kerno-*` containers.

Write the test first for core logic (SQL generation, parsing, type conversion, statement splitting). UI flows
use GPUI's test contexts: `crates/kerno/src/ui_tests.rs` holds the shared helpers and `ui_tests/*.rs` the tests,
one file per area (sidebar tree, menus, environments, toolbar, tool wizards, tasks, updates, and so on).

The suite has to pass every time, on a busy machine too. The rules are in
[docs/plans/README.md](docs/plans/README.md), "Reliability of the suite". The short version:

- Create the objects a test asserts on under names of its own (`kerno_ui_<test>`, `kerno_test_<test>`) through
  `Scratch` or `common::scratch_schema`, and assert only on those, never on a server-wide list or count another
  test can change.
- Wait for a state, not for time: `app.wait_until(cx, "what", |app, cx| condition)` with a condition that names the
  end state. No `sleep`, no retry around a whole test.
- A test that cancels waits until the statement is running on the server first.
- Three tests run at once against the same servers; no test changes a server-wide setting, and a test that
  terminates sessions or holds locks does it only on its own.
- Tests never build the real update runner and never reach the network.

## Lint

All three must pass before a commit:

```sh
cargo fmt --all --check
nice -n 10 cargo clippy --workspace --all-targets -- -D warnings
cargo deny check
```

`cargo deny` checks that every dependency licence is compatible with MIT OR Apache-2.0 (`deny.toml`).

## Code layout

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). In short:

- `crates/kerno-core`: driver traits, value and result model, config, SSH, secrets, storage, SQL helpers,
  environments, client tools, update rules. No GPUI.
- `crates/kerno-postgres`, `crates/kerno-mysql`: the drivers, each with `tests/` against its containers.
- `crates/kerno`: the app. Database, file, SSH and secret-store work goes through `bridge.rs`; the UI thread
  never waits on I/O.
- `assets/brand/` holds the identity files (see `BRAND.md`; use them, do not redraw them);
  `crates/kerno/assets/engines/` holds the engine logos.
- `docs/`: `FEATURES.md` (what works, with its tests), `DECISIONS.md` (choices and why), `PLAN.md` and
  `PROGRESS.md` (milestones and log), `ROADMAP.md`, `DBEAVER-PARITY.md`.

To add an engine, follow "How to add an engine" in the architecture document.

## Clean-IP rules

Kerno is MIT OR Apache-2.0 and must stay clean (CLAUDE.md §1).

- **DBeaver Community** (Apache-2.0) is the primary reference for features and interactions. Porting its
  logic to Rust is allowed. When a file ports non-trivial DBeaver logic, say so in a comment naming the
  DBeaver source file, and keep DBeaver listed with its Apache-2.0 notice in `THIRD_PARTY_NOTICES.md`.
- **Beekeeper Studio** (GPL-3.0) may be read to understand behaviour and UX, then Kerno's code is written
  from that understanding. Never translate its code, line by line or otherwise, and never open its
  `src-commercial/` directory (proprietary licence).
- SQL against system catalogs (`pg_catalog`, `information_schema`) is written from the engine's
  documentation; the references are for checking nothing was missed.
- No icons, screenshots, names, colours or wording copied from either project. Engine logos are the one
  exception, under the rules in CLAUDE.md §1 and `THIRD_PARTY_NOTICES.md`.
- Keep reference clones outside this repository (for example `~/VM/refs/`); never commit them or add them
  as submodules.

## Safety rules the code must keep

- Passwords and passphrases live only in the OS secret store (or in memory); never in plain text on disk, in
  logs or in crash output.
- Destructive actions show the SQL they will run and ask for confirmation; read-only connections refuse
  writes; production connections ask twice.
- Nothing leaves the machine: no telemetry, no analytics, no crash upload. The only request Kerno makes that is
  not to a database the user configured is the update check (one `curl` to the repository's `releases/latest`,
  D-048, with an off switch); a change that adds any other network call will not be accepted.
- A client tool folder runs only if Kerno detected it or the user added it; it never travels with an export
  (D-046). Extra arguments to a tool cannot set the server, user, TLS, option files or a password.
- `unsafe` is denied workspace-wide; the only exception is the Dock icon call in `app_icon.rs` (D-042).

## Releases

There is no hosted CI and no GitHub Actions workflow: `scripts/check.sh` is the check. A release is built locally
with `scripts/release.sh [version]` (clean tree, version equal to `Cargo.toml`, macOS `.dmg` and Linux tarball,
`dist/SHA256SUMS`); it prints the `gh release create` command, and publishing is the owner's. Never add a remote,
push, tag or publish from a change.

## Commits

Conventional messages, one commit (or more) per logical change: `feat(app): ...`, `fix(core): ...`,
`docs: ...`, `test(postgres): ...`, `refactor`, `chore`. The scope is the crate or area. Keep `docs/FEATURES.md`
and `docs/PROGRESS.md` in step with what a commit changes.
