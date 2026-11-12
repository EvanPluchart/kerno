# Context menus everywhere, connection folders, navigator clicks (owner request of 2026-11-13)

Read `docs/plans/README.md` first, then CLAUDE.md §3 ("DBeaver Community is the primary reference",
"Context menus everywhere") and §4.B item 1, and `docs/GOAL.md` criterion 2: **every surface listed in §3
answers a right-click, proven by UI tests**. When in doubt about an entry, its wording or its order, look at
what DBeaver does (`~/VM/refs/dbeaver`, the `plugin.xml` menu contributions of `org.jkiss.dbeaver.ui.navigator`,
`ui.editors.sql`, `ui.editors.data`, `ui.editors.connection`) and do the same with Kerno's look. DBeaver is
Apache-2.0: behaviour and logic may be ported; no icon or wording copied verbatim where it is distinctive.

How a menu is built here: `.id(…)` then `.context_menu(|menu, window, cx| menu.item(PopupMenuItem::new(…)
.on_click(…)).separator()…)` on the element (trait `ContextMenuExt`); a disabled entry says why in its label
suffix or tooltip, as the navigator does (`Navigator::unavailable`). Each menu's content comes from a pure
function returning a list of entries (like `navigator/actions.rs::actions`), unit-tested, and the view only
renders that list. Each surface also gets **one real right-click UI test** (see
`a_right_click_on_a_cell_opens_the_menu_of_its_row`: `window.click_with_options(id,
ClickOptions::new().with_button(MouseButton::Right), cx)` then `window.try_find_by_label("…")`). Every menu
entry is also a palette command when it is not tied to a position (CLAUDE.md §3 "every action reachable
from the command palette").

## Part 0 — Navigator click behaviour (§4.B item 1) — in progress

Single click selects; chevron, Right / Left expand and collapse; double click or Enter runs the default
action (table / view / materialized view: data tab, or per `Settings::table_double_click`; routines: their
source; other objects: DDL; folders: toggle). Setting in the settings dialog. UI tests with real clicks and
keys. (Delegated on 2026-11-13; mark done here when committed.)

## Part 1 — Connection folders and the sidebar's menus — `opus`

Today a connection has `ConnectionConfig::group: Option<String>` and the sidebar lists groups by name
(`crates/kerno/src/sidebar.rs`); the connection row has a menu (connect, edit, duplicate, delete…: read it).

1. **Folders** (core `config.rs` / `storage.rs`, app `connections/manager.rs`, `sidebar.rs`,
   `connections/dialog.rs`): a folder is a path of names joined by `/` in `group` (`"Work/Clients"`), so
   existing stores keep working and export / import of connections carries folders for free. Empty folders
   are kept in app storage as a setting (`storage.set_setting("connection_folders", &Vec<String>)`).
   `Connections` gains `folders()`, `create_folder(path)`, `rename_folder(from, to)` (rewrites the group of
   every connection under it and the sub-folders), `delete_folder(path)` (only when empty of connections,
   or after a confirmation that says its N connections move to the parent), `move_connection(id,
   Option<path>)`, `rename_connection(id, name)`. The sidebar renders the tree of folders (collapsible,
   state kept in settings), connections inside, ungrouped ones at the root. The connection dialog's
   group field becomes a folder picker (existing folders + "New folder…").
2. **Menus**:
   - Empty area of the connections list: New connection, New folder, Import connections, Refresh (reloads
     the saved list and re-checks statuses), Collapse all.
   - Folder: New connection here (opens the dialog with the folder preset), New subfolder, Rename, Delete.
   - Connection: Connect, Disconnect, Reconnect (disconnect then connect), New SQL editor (a query tab
     bound to it; connects first when needed), Edit, Duplicate, Rename, Move to folder ▸ (submenu of
     folders + "No folder"), Delete, Refresh (reloads its navigator tree), Copy ▸ (Name; Connection URL
     **without the password**; Host).
     Entries that do not apply are disabled (Connect when connected, …).
   - Empty area of the navigator tree (below the objects): Refresh, Collapse all, New query here.
3. **Drag and drop**: a connection row can be dragged onto a folder (or the root area) to move it (GPUI
   `on_drag` / `on_drop`; look at how the kit's tabs or the table's column reordering do it). Keyboard
   alternative is "Move to folder".
4. Tests: unit tests of the folder operations in `connections/manager_tests.rs` (rename rewrites nested
   paths; delete rules; storage round trip; export / import keeps folders); UI tests: the three menus
   answer a real right-click with the listed entries; New folder → rename → move a connection into it →
   delete it; Copy URL has no password (the secret-grep rule of GOAL criterion 3); drag and drop if the
   test window can simulate it (`window.drag`-style helper in the kit's test module — check; otherwise test
   the drop handler directly and say so).

Files: `crates/kerno-core/src/config.rs`, `storage.rs` (only if the setting helper is not enough),
`crates/kerno/src/connections/{manager.rs,manager_tests.rs,dialog.rs,form.rs}`, `sidebar.rs`,
`navigator/view.rs` (empty-area menu), `actions.rs`, `workspace.rs`, `ui_tests.rs`.

## Part 2 — Tabs bar, editor and status bar menus — `opus`

Files: `crates/kerno/src/workspace.rs`, `query/tab.rs`, `data/tab.rs`, `structure/*.rs`, `actions.rs`,
`tabs.rs`, `ui_tests.rs`.

1. **Tab** (right-click on a tab in the tab bar): Close, Close others, Close to the right, Close all,
   Rename (query tabs: an inline or small dialog rename; the title is stored in the snapshot; data /
   structure tabs: disabled), plus Duplicate (query tabs) and Copy file path / Reveal in Finder for a
   file-backed tab. Closing several tabs asks once for those that hold something (see `docs/plans/M5.md`
   Part 4: transactions, pending edits, unsaved files — implement that confirmation here if it is not done
   yet, one dialog listing what would be lost). Middle click closes a tab. Empty area of the tab bar: New
   query tab, Reopen closed tab (keep the last 10 closed query-tab snapshots in memory), Close all.
2. **Editor** (right-click in the SQL editor): Run statement, Run script (run all), Explain, Explain
   analyze, Format SQL, — Cut, Copy, Paste, Select all, — Toggle comment, Find, Replace, — Save, Save as,
   Save as query… The kit's editor has its own context menu: read how to replace or extend it
   (`InputState::context_menu` / the element's `.context_menu`; a custom menu replaces the built-in one, so
   re-add cut / copy / paste by dispatching the kit's own actions).
3. **Status bar**: each item answers a click and a right-click with its own actions — connection item:
   Disconnect, Reconnect, Edit connection, Administration (when milestone 8's admin tab exists); database
   item: switch database (the list the tab toolbar offers); transaction item: Commit, Rollback, toggle
   auto-commit; read-only item: explanation; rows / timing item: Copy; pending-changes item: Review SQL,
   Discard; the "passwords are kept in memory only" notice: what it means and what to do.
4. Tests: one real right-click test per surface (tab, tab bar empty area, editor, each status item kind
   that has a menu), unit tests of the entry lists, close-others / close-to-the-right / close-all with the
   confirmation, rename stored and restored, reopen closed tab.

## Part 3 — Grid menus (results and table data) — `opus`

Files: `crates/kerno/src/query/grid.rs`, `query/tab.rs`, `data/tab.rs`, `ui_tests.rs`.

The row menu exists (owner extras, then Copy / Copy with column names / Copy as CSV, JSON, Markdown, SQL
INSERT / Copy column names); it is built per cell in `Grid::render_td` and knows only the row. Make it know
the **cell** (row and column) and build it from a pure function of (row, column, grid kind, editable):

- Both grids: Copy, Copy as ▸ (the formats, as a submenu), Copy column name, View value (the value
  viewer), — Order by this column ▸ Ascending / Descending / Clear (client-side on a result, server-side
  on table data), Hide column, Show all columns, Pin column, — Export… (milestone 8's dialog).
- Table data only: Edit cell, Set NULL, Set default, Paste (clipboard into the cell, or a TSV block from
  this cell — see `docs/plans/M6.md` Part 1), — Filter by this value (`column = value`, or `IS NULL`),
  Exclude this value (`column <> value` / `IS NOT NULL`), Filter ▸ (other operators prefilled in the filter
  bar), Clear filters, — Duplicate row, Delete row, Revert row, — the foreign-key entries (exist).
  Filter entries add a `Condition` to the filter bar and apply it; they are refused with the usual notice
  while changes are pending.
- Column header menu (exists: Copy name, Pin, Hide, Show all): add Order by ▸, Filter on this column…
  (adds an empty condition line on it and focuses its value), Fit width, Fit all widths.
- Selection of several cells: Copy / Copy as act on the selection; Set NULL / Paste too.
- Tests: unit tests of the entry lists for each case (read-only table, view, NULL cell, binary cell, new
  row, deleted row); real right-click tests on a result cell, a data cell and a header; "Filter by this
  value" and "Exclude this value" change the rows (PostgreSQL and MySQL); Order by from the menu.

Performance: the menu is attached per cell; keep the closure cheap (no allocation per cell beyond what
exists) and re-check scrolling in milestone 9.

## Part 4 — Docs — `sonnet`

`docs/FEATURES.md`: a section "Context menus" listing each surface of CLAUDE.md §3 with its entries, the
test that right-clicks it, and a capture (`ctx-<surface>-light.png`); section B item for click behaviour;
A items for folders and drag and drop. DECISIONS entry: folders as paths in `group`; menu content as pure
functions; what double click does and its setting. PROGRESS, PLAN.

## Acceptance

Every surface of §3 answers a right-click in a UI test; every entry does something or is disabled with a
reason; folders and drag and drop work and survive a restart and an export / import; workspace tests,
clippy, fmt clean.
