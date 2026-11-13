# Plan

Milestones follow CLAUDE.md §7. Status: `todo`, `doing`, `done`. A milestone is `done` only when its
checks ran and passed; the evidence is in `PROGRESS.md`.

| #  | Milestone                                                                 | Status |
| -- | ------------------------------------------------------------------------- | ------ |
| 0  | Environment, pinned GPUI hello window, workspace, licences, containers, fixtures, local check script | done  |
| 1  | Driver trait, PostgreSQL and MySQL/MariaDB drivers with tests              | done   |
| 2  | App shell: window, sidebar, tabs, theme tokens, command palette, storage, secrets, Tokio bridge | done  |
| 3  | Connections (§4 A)                                                         | done   |
| 4  | Schema navigator (§4 B)                                                    | done   |
| 5  | SQL editor, results grid, history, saved queries (§4 C, D)                 | done (Part 4 tab-close confirmations `5864bc9`, Ctrl+Space and catalog refresh `89cdd60`, `2b0f3bf`; the final captures come with milestone 10) |
| 6  | Table data view and editing (§4 E)                                         | done (lists for boolean and enum cells, paste, Delete, Cmd+Z, estimated and exact counts `e13ec36`, editing a query result `409c4fc`) |
| 7  | Structure (§4 F)                                                           | doing (left: the tests of `plans/M7.md` Part 1c item 4: renaming a table from the form with the structure tab following, and the per-engine wording of the confirmation; column order cannot be edited) |
| 8  | Import, export, backup, administration (§4 G, H)                           | doing (left: UI tests for the import dialog's cancel, its production confirmation and the MySQL "created first" sentence; the backup dialog has no tables list, one table is backed up from its tree node; export, import, backup and restore, the administration tab, maintenance tools, locks and idle sessions are done) |
| 9  | Performance pass, `docs/BENCH.md`                                          | doing (bench mode and its script `89cdd60`, engine logos drawn at their size, no slack in result batches `2b0f3bf`; left: the clean measurement on an idle machine and `docs/BENCH.md`) |
| 10 | Polish: UX pass with screenshots, icon, states, keyboard, settings, packaging, Linux build, README, docs | doing (done: packaging and the Linux container build, the brand colours, lockup and mark, the settings view, the keymap file, shortcuts in the README; left: a third list of visual defects in `plans/M10.md` Part 4, the final screenshots in `docs/screenshots/final/`) |
| —  | Owner feedback of 2026-11-13: context menus and DBeaver parity (`plans/CTX.md`, `plans/PARITY-GAPS.md`) | done: CTX Parts 0 to 3 and PARITY-GAPS G1 to G17 are delivered; `DBEAVER-PARITY.md` has 322 rows done and 96 on the roadmap, none left as a gap; the debt of tests (G18) is closed except the rows `FEATURES.md` still marks "no test" |

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

- The reliability of the test suite is being worked on (in progress): some tests depend on timing or on what
  other tests leave on the servers; the full run with the containers up has to pass repeatedly before the goal
  is verified (GOAL criterion 1).
- Still open for the goal: the third list of visual defects (`plans/M10.md` Part 4), a clean performance
  measurement on an idle machine with `docs/BENCH.md` (criterion 5), the final screenshots in
  `docs/screenshots/final/` (criterion 7), and the final verification of every criterion of `docs/GOAL.md`.

- `mysql` and `mysqldump` were thought missing: they are installed (Homebrew `mysql-client`, keg-only, not
  on `$PATH`); the backup tools find them in their fallback directories. Nothing to ask the owner.
- Done on 2026-11-13: `cargo deny check` is clean with no ignored advisory (russh built without its `rsa`
  feature; RSA key files and RSA-only host keys are refused, see DECISIONS D-013).
