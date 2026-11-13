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
| 5  | SQL editor, results grid, history, saved queries (§4 C, D)                 | doing (left: `plans/M5.md` Part 4 tab-close confirmations, Ctrl+Space and catalog leftovers of Part 2, captures; Parts 1, 2, 3 and the docs are done) |
| 6  | Table data view and editing (§4 E)                                         | doing (left: `plans/M6.md` Part 1 editing comfort, Part 2 counting large tables; the table view, editing and the docs are done) |
| 7  | Structure (§4 F)                                                           | doing (left: two tests of `plans/M7.md` Part 1c item 4, captures `m7-*`; Parts 1 to 4 are done) |
| 8  | Import, export, backup, administration (§4 G, H)                           | doing (left: `plans/M8.md` Part 3 backup / restore dialog, Part 4 administration tab, UI tests left over from Parts 1 and 2, captures; export, import and the backup core are done) |
| 9  | Performance pass, `docs/BENCH.md`                                          | todo   |
| 10 | Polish: UX pass with screenshots, icon, states, keyboard, settings, packaging, Linux build, README, docs | doing (packaging and the Linux container build done, `plans/M10.md` Part 6; the rest todo) |
| —  | Owner feedback of 2026-11-13: context menus and DBeaver parity (`plans/CTX.md`, `plans/PARITY-GAPS.md`) | doing: done are CTX Parts 0 and 1 (clicks, folders, drag and drop, sidebar menus) and PARITY-GAPS G1 (keys of the tree and the connection list); left are CTX Parts 2 and 3 (tabs bar, editor, status bar, grid menus), PARITY-GAPS G2 to G18, and 99 gap rows in `DBEAVER-PARITY.md` |

## Implementation plans (owner request of 2026-11-12, CLAUDE.md §7 "Model budget")

`docs/plans/README.md` holds what every plan shares; `docs/plans/M5.md` to `M10.md` hold the remaining work
of each milestone, split into parts. The main session plans, reviews and commits; each part is implemented
by a subagent (`opus`, effort high; `sonnet` or `haiku` for mechanical work), at most two at once. If the
main session stops, the work continues from these files.

## Owner feedback of 2026-11-13 (spec commit `6100e4f`)

After testing a release build: (1) a double click or Enter on a table opens its data, as DBeaver; the
chevron and the arrow keys expand; a setting chooses data / structure / expand — `plans/CTX.md` Part 0, done; (2) context menus on every surface (sidebar, folders, connections, tab bar, editor, grids, status
bar), connection folders and drag and drop — `plans/CTX.md` Parts 1 (done) to 3 (Parts 2 and 3 left); (3) DBeaver Community is the
primary reference: `docs/DBEAVER-PARITY.md` is built from its source and its gaps become plan parts —
`plans/PARITY.md`. GOAL criterion 2 requires both.

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
- Done on 2026-11-13: `cargo deny check` is clean with no ignored advisory (russh built without its `rsa`
  feature; RSA key files and RSA-only host keys are refused, see DECISIONS D-013).
