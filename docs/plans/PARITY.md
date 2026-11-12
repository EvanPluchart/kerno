# DBeaver parity inventory (owner request of 2026-11-13; GOAL criterion 2)

Read `docs/plans/README.md` first, then CLAUDE.md §3 (first two bullets), §4 and §1 "Open source, clean IP".

GOAL criterion 2 now requires `docs/DBEAVER-PARITY.md`: an inventory of DBeaver Community's **navigator,
SQL editor, results and connection** features and context menus **that apply to PostgreSQL and MySQL**,
built from its source (`plugin.xml` menu contributions included), each row marked *done in Kerno* or *moved
to `docs/ROADMAP.md` with a reason*. Rows that are neither are gaps: they become plan items and are built.

DBeaver Community is Apache-2.0 and cloned at `~/VM/refs/dbeaver` (outside the repository, read-only).
Reading it, describing its behaviour and porting its logic are allowed; do not copy its icons, and do not
paste long passages of its text into Kerno's docs — describe each feature in a few of your own words and
name the source file it comes from.

## Part 1 — Build the inventory — `opus` (research and writing only: no code, no build, no containers)

Sources to read, in this order (`plugins/` under the clone):

1. Menus, commands, key bindings and handlers declared in `plugin.xml` (and the `OSGI-INF/l10n/bundle.properties`
   or `plugin.properties` beside it for the labels) of: `org.jkiss.dbeaver.ui.navigator` (database navigator:
   node actions, create / delete / rename / refresh / filter / link with editor / copy / paste / drag and
   drop, folders, "Open SQL console", "Generate SQL"), `org.jkiss.dbeaver.ui.editors.sql` (SQL editor: run
   statement / script / in new tab, explain, format, templates, comment, navigate, outline, parameters,
   history, "copy as source code", output viewer, auto-commit and transaction commands),
   `org.jkiss.dbeaver.ui.editors.data` and the result-set viewer in `org.jkiss.dbeaver.ui.editors.base` /
   `ui.editors.data` (`ResultSetViewer`, `ResultSetHandlerMain`, filters, ordering, panels: value, grouping,
   aggregation, metadata, references; record mode; copy special; advanced paste; generate SQL from rows;
   refresh / auto-refresh; fetch size; row colouring; edit and save / reject / script),
   `org.jkiss.dbeaver.ui.editors.connection` and `org.jkiss.dbeaver.registry` / `ui.navigator` for connection
   management (folders, connection types and colours, edit, duplicate, invalidate / reconnect, read-only,
   auto-commit default, bootstrap queries, keep-alive, SSH / SSL tabs, driver properties, URL), and the
   engine plugins `org.jkiss.dbeaver.ext.postgresql(.ui)` and `org.jkiss.dbeaver.ext.mysql(.ui)` (object
   types in the tree, per-object editors and tabs, tools: backup / restore / analyze / vacuum / truncate,
   session manager, lock manager, user and role editors, engine-specific create dialogs).
   Useful commands: `grep -n "<command \|<menuContribution\|<handler \|<key " plugin.xml`, then follow a
   `commandId` to its label in the `.properties` file and to its handler class.
2. The Java behind what is not obvious from the XML (only as far as needed to describe the behaviour and
   its defaults): e.g. `NavigatorHandlerObjectOpen` (what a double click opens, and the preference behind
   it), `ResultSetViewer` context-menu construction (`fillContextMenu`), `SQLEditor` (`processQueries`,
   result tabs policy), preferences pages for defaults that matter (fetch size 200, auto-commit, "open
   table in new tab", read-only confirmations).

What to leave out: Eclipse workbench plumbing (perspectives, views layout, workspace / project resources,
team / Git), DBeaver PRO and Enterprise features, engines other than PostgreSQL and MySQL / MariaDB,
data-transfer wizards beyond what CLAUDE.md §4.G covers (list them once under roadmap), ERD, dashboards,
AI, debuggers, task scheduler, mock data: one roadmap row each with the reason "outside v1 (CLAUDE.md §4,
last paragraph)".

Kerno's side: for each DBeaver feature find whether Kerno has it — `docs/FEATURES.md`, `crates/kerno/src/actions.rs`
(`COMMANDS`: every command), `navigator/actions.rs` (menu entries per node kind), `query/grid.rs` (row and
header menus), `sidebar.rs`, `query/tab.rs`, `data/tab.rs`, `structure/`, the UI tests' names
(`grep -n "^fn \|^async fn " crates/kerno/src/ui_tests.rs`), and the plans in `docs/plans/` (a feature that
a plan part already covers is "planned: <file> Part n", not a gap).

Output 1 — `docs/DBEAVER-PARITY.md`:

- A short method note (DBeaver version / commit of the clone: `git -C ~/VM/refs/dbeaver log -1 --format=%h\ %cs`,
  what was read, what was left out and why).
- One section per area: **Connections**, **Navigator**, **SQL editor**, **Results and data editing**,
  **Object editors (structure)**, **Tools and administration**, and **Context menus** (one sub-table per
  surface, listing DBeaver's entries in its order: navigator empty area, folder, connection, database /
  schema, table, view, column, routine, sequence, type, trigger; SQL editor; result grid cell, column
  header, row header; tab; status bar).
- Each row: `Feature (as DBeaver names it)` | `What it does (your words)` | `DBeaver source (plugin, file or
  commandId)` | `PG / MySQL` (which engines it applies to) | `Kerno` — exactly one of: **done** (the
  command id, menu entry or file, and the covering test name), **planned** (`docs/plans/<file>` Part n),
  **gap** (not planned anywhere), **roadmap** (with the reason: outside v1 scope per CLAUDE.md §4; needs
  another engine; Eclipse-specific; …). Be strict: *done* needs a test that covers it.
- A closing summary: counts per status and per area.

Output 2 — `docs/plans/PARITY-GAPS.md`: every **gap** row grouped into parts sized for one `opus` agent
each (by area and by the files they touch), ordered by how much a daily user of DBeaver would miss them
(defaults and interactions first: double click, Enter, F2 / F4 / Ctrl+Enter-style shortcuts, auto-refresh,
filters from the grid, record mode, …), each item with: what to build, where (files), how DBeaver behaves
(defaults included), the test to write. For a gap you judge not worth v1 despite applying to both
engines, do not decide alone: list it in a last section "Proposed for the roadmap" with the reason; the
main session decides.

Output 3 — `docs/ROADMAP.md` (create it if absent; milestone 10 completes it): the **roadmap** rows, grouped,
with their reason, under a heading "From the DBeaver inventory".

Scope: only those three files. No code change, no cargo command, no container, no app. Report: the counts
per status and area, the ten gaps you rank highest with one line each, anything in DBeaver's source you
could not interpret, and what you did not read.

## Part 2 — Build the gaps — `opus`, one agent per part of `PARITY-GAPS.md`

After the main session has reviewed Output 2 (and merged it with `docs/plans/CTX.md`, which already covers
the context menus the owner listed, and with the milestone plans): each part is implemented like any other
plan part, with UI tests; when a part lands, its rows in `docs/DBEAVER-PARITY.md` move from *gap* /
*planned* to *done* with the test name.

## Acceptance (GOAL criterion 2)

`docs/DBEAVER-PARITY.md` has no row left as *gap* or *planned*: each is *done* with its test, or in
`docs/ROADMAP.md` with a reason the owner can read.
