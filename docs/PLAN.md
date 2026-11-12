# Plan

Milestones follow CLAUDE.md §7. Status: `todo`, `doing`, `done`. A milestone is `done` only when its
checks ran and passed; the evidence is in `PROGRESS.md`.

| #  | Milestone                                                                 | Status |
| -- | ------------------------------------------------------------------------- | ------ |
| 0  | Environment, pinned GPUI hello window, workspace, licences, containers, fixtures, CI file | done  |
| 1  | Driver trait, PostgreSQL and MySQL/MariaDB drivers with tests              | done   |
| 2  | App shell: window, sidebar, tabs, theme tokens, command palette, storage, secrets, Tokio bridge | doing |
| 3  | Connections (§4 A)                                                         | todo   |
| 4  | Schema navigator (§4 B)                                                    | todo   |
| 5  | SQL editor, results grid, history, saved queries (§4 C, D)                 | todo   |
| 6  | Table data view and editing (§4 E)                                         | todo   |
| 7  | Structure (§4 F)                                                           | todo   |
| 8  | Import, export, backup, administration (§4 G, H)                           | todo   |
| 9  | Performance pass, `docs/BENCH.md`                                          | todo   |
| 10 | Polish: UX pass with screenshots, icon, states, keyboard, settings, packaging, Linux build, README, docs | todo |

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

- `mysql` and `mysqldump` are not installed on the development Mac; installing them is a system-level
  install (CLAUDE.md §8). To settle in milestone 8 (see DECISIONS D-006).
- MySQL/MariaDB TLS: `verify-ca` currently behaves like `verify-full` (refuses on a name mismatch) and no
  test server has a CA-signed certificate. To fix in milestone 3 with a TLS fixture for both engines
  (DECISIONS D-015).
- **For the owner to confirm:** `deny.toml` ignores one security advisory, RUSTSEC-2023-0071 (timing side
  channel in `rsa`, reached only when an SSH tunnel authenticates with an RSA key file). The fix exists but
  russh does not use it yet. Reasoning and alternatives in DECISIONS D-013.
