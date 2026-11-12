# /goal criteria

Build Kerno end to end, autonomously, by following CLAUDE.md (master spec). Work through milestones 0 to 10 in
order without stopping for approval; ask only under CLAUDE.md §8.

The goal is reached only when ALL of these are true and verified by you:

1. `nice -n 10 cargo build --release` succeeds; `cargo fmt --check`, `cargo clippy --workspace --all-targets
   -- -D warnings` and `cargo deny check` are clean with no ignored advisory (RUSTSEC-2023-0071 in the SSH stack fixed in
   milestone 10, by upgrading or replacing the affected dependency); `nice -n 10 cargo test --workspace -- --test-threads=3`
   passes with the `kerno-*` containers up (integration tests ran, not skipped).
2. Every v1 feature of CLAUDE.md §4 (A to I) works end to end against PostgreSQL 17, MySQL 8.4 and
   MariaDB 11. `docs/FEATURES.md` lists each item with its status, the test that covers it, and the
   screenshot that shows it. No item is missing or stubbed.
3. Secrets: no password or passphrase on disk outside the OS secret store or in logs (a test greps the data
   dir and log file after a session with a known password). Destructive actions and pending edits always
   preview SQL and confirm (UI tests cover drop, truncate and apply edits).
4. Cancel works for a running long query on both engines (integration test with `pg_sleep` / `SLEEP`), and
   the UI stays responsive during it (UI test).
5. Performance targets of CLAUDE.md §5 met and measured in `docs/BENCH.md` with the method, machine and
   numbers (cold start, idle RSS, first rows, 1M-row scroll frame time, 1M-row memory).
6. SSH tunnel and TLS connections tested against the `kerno-ssh` container and a TLS-enabled PostgreSQL.
7. `docs/screenshots/final/` holds every main screen (connections, new-connection dialog, navigator, query
   editor with results, EXPLAIN plan, table data with pending edits, structure editor, import/export,
   admin, command palette, settings, empty and error states) in light and dark; you looked at each one and
   fixed what was wrong.
8. The workspace builds in a Linux container; `.github/workflows/ci.yml` exists; `scripts/bundle-macos.sh`
   produces a launchable `Kerno.app` with the icon; `scripts/bundle-linux.sh` produces a tarball.
9. README (what Kerno is, screenshots, install, build from source on macOS and Linux, run the test
   containers, keyboard shortcuts, roadmap link, licence); `CONTRIBUTING.md`; `docs/ROADMAP.md`;
   `docs/ARCHITECTURE.md` (crates, driver trait, async bridge, how to add an engine).
10. `docs/PLAN.md`, `PROGRESS.md`, `DECISIONS.md` current; at least one commit per milestone; no remote added,
    nothing pushed; no app instance, container, watcher or browser left running.
