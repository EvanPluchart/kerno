# Plan

Milestones follow CLAUDE.md §7. Status: `todo`, `doing`, `done`. A milestone is `done` only when its
checks ran and passed; the evidence is in `PROGRESS.md`.

| #  | Milestone                                                                 | Status |
| -- | ------------------------------------------------------------------------- | ------ |
| 0  | Environment, pinned GPUI hello window, workspace, licences, containers, fixtures, CI file | done  |
| 1  | Driver trait, PostgreSQL and MySQL/MariaDB drivers with tests              | done   |
| 2  | App shell: window, sidebar, tabs, theme tokens, command palette, storage, secrets, Tokio bridge | done  |
| 3  | Connections (§4 A)                                                         | done   |
| 4  | Schema navigator (§4 B)                                                    | done   |
| 5  | SQL editor, results grid, history, saved queries (§4 C, D)                 | doing (left: `plans/M5.md`) |
| 6  | Table data view and editing (§4 E)                                         | doing (left: `plans/M6.md`) |
| 7  | Structure (§4 F)                                                           | todo   |
| 8  | Import, export, backup, administration (§4 G, H)                           | todo   |
| 9  | Performance pass, `docs/BENCH.md`                                          | todo   |
| 10 | Polish: UX pass with screenshots, icon, states, keyboard, settings, packaging, Linux build, README, docs | todo |

## Implementation plans (owner request of 2026-11-12, CLAUDE.md §7 "Model budget")

`docs/plans/README.md` holds what every plan shares; `docs/plans/M5.md` to `M10.md` hold the remaining work
of each milestone, split into parts. The main session plans, reviews and commits; each part is implemented
by a subagent (`opus`, effort high; `sonnet` or `haiku` for mechanical work), at most two at once. If the
main session stops, the work continues from these files.

## How each feature is built

For every feature of §4, in this order:

1. Read how DBeaver Community and Beekeeper Studio do it, in the clones under `~/VM/refs/` (outside this
   repository). Rules, from CLAUDE.md §1:
   - DBeaver (Apache-2.0): its logic may be ported to Rust. A Kerno file that ports non-trivial DBeaver logic
     names the source file in a comment, and `THIRD_PARTY_NOTICES.md` lists DBeaver with its notice.
   - Beekeeper Studio (GPL-3.0): read for behaviour and UX only, then write from that understanding. No
     line-by-line translation. `src-commercial/` is never opened (the clone excludes it from the checkout).
   - Catalog queries are written from the engine documentation; the references are a completeness check.
2. Write the core logic test-first in `kerno-core` or the driver crate.
3. Build the view on `gpui-component` parts; add the GPUI UI test for the flow.
4. Run the release app against the `kerno-*` containers, capture the window, look at it, fix, recapture.
5. Add the row to `docs/FEATURES.md` (status, covering test, screenshot).

## Open points

- `mysql` and `mysqldump` were thought missing: they are installed (Homebrew `mysql-client`, keg-only, not
  on `$PATH`); the backup tools find them in their fallback directories. Nothing to ask the owner.
- Milestone 10, required by GOAL criterion 1 (owner's decision of 2026-11-12): `cargo deny check` must be
  clean with no ignored advisory. `deny.toml` ignores RUSTSEC-2023-0071 for now (timing side channel in
  `rsa`, reached only when an SSH tunnel authenticates with an RSA key file; russh pins the affected
  release candidate). To do: upgrade russh to a version built on the fixed `rsa`, or replace the affected
  dependency (drop russh's `rsa` feature and state that RSA keys go through the agent, or another SSH
  client), then remove the ignore. Reasoning in DECISIONS D-013.
