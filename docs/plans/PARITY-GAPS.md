# DBeaver parity: the gaps as work orders (PARITY Part 2)

Read `docs/plans/README.md` first (rules, map of the code, UI test notes), then CLAUDE.md §3 ("DBeaver
Community is the primary reference", "Context menus everywhere") and the rows named in each item in
`docs/DBEAVER-PARITY.md`. Each part below is sized for one `opus` agent. When a part lands, its rows in
`docs/DBEAVER-PARITY.md` move to **done** with the test name (main session).

Parts are ordered by how much a daily DBeaver user would miss them: defaults and interactions first. Each
item gives what to build, where, how DBeaver behaves (with its source in `~/VM/refs/dbeaver/plugins`; porting
its logic is allowed with a comment naming the file), and the test to write. Catalog queries are written
from the engine docs. Every write goes through `sql_confirm.rs`; a read-only connection refuses it.

**Already ordered elsewhere, not repeated here**: the sidebar, folder, navigator-empty-area, tab, editor
and status-bar menus (`docs/plans/CTX.md` Parts 1 and 2); the grid menus, filter by value, order from the
menu, fit widths (`CTX.md` Part 3); paste, Delete-to-NULL, Cmd+Z, enum and boolean cells (`M6.md` Part 1);
estimated counts (`M6.md` Part 2); backup / restore (`M8.md` Part 3); sessions, roles, sizes, settings
(`M8.md` Part 4); settings, keyboard coverage, quick open (`M10.md` Part 3).

**File ownership.** Several parts touch files that a planned part also owns. Run them after it:
G1 after CTX Part 1 (`sidebar.rs`); G2, G3, G6 after CTX Part 3 and M6 Part 1 (`query/grid.rs`,
`data/tab.rs`); G4, G14 and the menu items of G15 after CTX Part 2 (`workspace.rs`, the editor menu);
G16 after M8 Part 4 (`admin/`). Two parts that share a file never run at the same time.

## Order of the work

The parts are numbered in the order a DBeaver user would miss them. The ten items that matter most:

1. A double click on a connected connection disconnects it; DBeaver expands it (G1, NAV-26).
2. The tree has no F4, F2, Delete, F5 or Cmd+C (G1, NAV-29 to NAV-32).
3. Cmd+S does not apply data edits, and the row commands have no keys (G2, RES-53, RES-57 to RES-59).
4. Running in a tab of a disconnected connection does not connect it (G4, SQL-04).
5. Every run replaces the results: no `Cmd+\`, no pinned result tabs (G4, SQL-06, SQL-19).
6. No record mode (G3, RES-02).
7. A query result cannot be filtered or ordered on the server (G5, RES-04, RES-09).
8. The result of a single-table SELECT cannot be edited (G8, RES-51).
9. No Constraints, Foreign keys, References or Triggers under a table in the tree (G10, NAV-04 to NAV-07).
10. No SQL generated from selected rows, no CALL for routines (G7, RES-66, NAV-53).

Next in line: auto-refresh (G5, RES-25), the PostgreSQL and MySQL maintenance tools (G12), the commit mode
of production connections and bootstrap queries (G13), the value editor (G9), selection statistics (G6).

## G1 — Navigator and sidebar: default actions and keys — `opus`

Rows: NAV-26, NAV-29, NAV-30, NAV-31, NAV-32, NAV-33, NAV-36, NAV-48, CM-33, CM-36, CM-53 (F4 also noted in rows CM-16,
CM-46, CM-65, CM-79). Files: `crates/kerno/src/sidebar.rs`, `navigator/view.rs`,
`navigator/actions.rs`, `actions.rs`, `ui_tests.rs`. After CTX Part 1.

1. **Double click on a connection connects, never disconnects** (NAV-26). Today `sidebar.rs` toggles: a
   double click on a connected connection disconnects it. DBeaver's default is `EXPAND`
   (`ui.navigator` `UINavigatorPreferencesInitializer`, pref `NAVIGATOR_CONNECTION_DOUBLE_CLICK`): connect
   when needed, then show the connection's tree; disconnecting is a menu action. Build: double click (and
   Enter on the selected row) connects a disconnected connection and makes it the navigator's connection;
   on a connected one it only makes it the navigator's connection. No setting.
   Test `a_double_click_on_a_connection_connects_and_never_disconnects` (PG): double click twice, still
   connected, the tree shows its databases.
2. **Tree keys** (NAV-29 to NAV-32). DBeaver: F4 is `core.object.open` (`ui.navigator` plugin.xml, context
   `ui.context.navigator`); F2, Delete, F5 and Ctrl+C are Eclipse's default keys of `edit.rename`,
   `edit.delete`, `file.refresh` and `edit.copy`, which `core` contributes to the navigator popup. Build in
   `navigator/view.rs`, next to the Enter handler: F4 opens structure for tables, views and materialized
   views, the source for routines, the DDL for other objects (a `navigator/actions.rs::edit_action(node)`
   beside `default_action`, unit-tested); F2 runs `Rename`, Delete and Backspace run `Drop`, F5 runs
   `Refresh`, Cmd+C runs `CopyName`, each only when `actions(node)` holds it and through the same
   `Navigator::perform` path (read-only and production rules unchanged). In the sidebar, F4 on a connection
   row opens Edit. Test `navigator_keys_run_the_actions_of_the_selected_node` (PG): on `shop.orders`, F4
   opens its structure tab, F2 opens the rename prompt, Delete opens the drop confirmation showing
   `DROP TABLE` (cancel it), Cmd+C puts `orders` on the clipboard, F5 reloads.
3. **Copy qualified name** (NAV-33). DBeaver's "Copy Advanced Info" (`core.edit.copy.special`,
   Ctrl+Shift+C) copies the full name. Add `Action::CopyQualifiedName` "Copy qualified name" after "Copy
   name" on every object node: `schema.table` (MySQL `db.table`) quoted with the dialect's `quote_ident`
   only where needed; Cmd+Shift+C in the tree. Unit test in `navigator/actions.rs`; UI test on a table
   whose name needs quotes (create it in a `Scratch`).
4. **New SQL editor on any node** (NAV-48, CM-53, CM-35). DBeaver offers SQL Editor ▸ on every node
   (`ui.editors.sql` `popup:…?after=sql_tools`), Ctrl+] for a new script and Ctrl+Alt+Enter for a console
   bound to the selection. Add `NewQueryHere` to PostgreSQL database nodes and to tables, views and
   routines (the tab is bound to the node's database and schema); Cmd+] in the tree runs it. Extend
   `each_kind_of_node_gets_the_actions_that_make_sense_for_it`; UI test: Cmd+] on `shop.orders` opens a
   query tab whose target schema is `shop`.
5. **Set as default** (NAV-36, CM-36). DBeaver `core.navigator.set.default` (Ctrl+Shift+A) makes a
   database or schema the active one of the connection. Kerno's targets are per tab: on a schema or a
   MySQL database node, "Set as default for this tab" sets the database / schema of the active query tab
   when it belongs to the same connection; otherwise the entry is disabled and says why
   (`Navigator::unavailable`). Test: the active tab's schema selector shows the chosen schema and a run
   resolves an unqualified name in it.

## G2 — Data editing keys and Cmd+S — `opus`

Rows: RES-53, RES-55, RES-57, RES-58, RES-59 (shortcut also noted in row RES-42). Files:
`crates/kerno/src/actions.rs`, `data/tab.rs`, `query/grid.rs`, `workspace.rs` (Save routing only),
`kerno-core/src/pending.rs`, `ui_tests.rs`. After CTX Part 3 and M6 Part 1.

DBeaver binds these in the result-set context (`ui.editors.data` plugin.xml, `ui.context.resultset.focused`).

1. **Cmd+S applies pending changes in a data tab** (RES-59). DBeaver `core.resultset.applyChanges` is
   Ctrl+S. In Kerno Cmd+S is `save_file`; route it: when the active tab is a data tab with pending changes,
   Save opens the same Review-and-apply confirmation as the Apply button; in a query tab it saves the file
   as today. Test: edit a cell, Cmd+S shows the review dialog with the `UPDATE`.
2. **Default keys for the row commands** (RES-53, RES-57). Give `add_row` "alt-insert", `duplicate_rows`
   "ctrl-alt-insert", `delete_rows` "alt-delete", `set_default` "ctrl-backspace" (DBeaver's
   `row.add`, `row.copy`, `row.delete`, `cell.setDefault`; DBeaver has no macOS variant). Most Mac
   keyboards have no Insert key: also bind `add_row` to "secondary-shift-n", `duplicate_rows` to
   "secondary-d" and `delete_rows` to "secondary-backspace" (all free in `actions.rs` today; check again). They act only when a data grid has the focus
   (bind them in the data tab's key context, not globally). `actions.rs`'s
   `command_ids_are_unique_and_shortcuts_valid` keeps passing.
3. **Esc reverts the selected cell** (RES-58). DBeaver `core.resultset.cell.reset` (Esc) resets the cell to
   its original value. With no editor open and a selected cell that has a pending value, Esc drops that
   value (`Pending::revert_cell(row, column)`, unit-tested); with an editor open Esc keeps cancelling the
   editor.
4. **Copy from the row above / below** (RES-55). DBeaver `row.copy.from.above` (Ctrl+D) and
   `.from.below` (Ctrl+Alt+D) copy the neighbour row's values into the current row. Same here, for the
   writable columns only (key columns of an existing row included only when the row is new), as pending
   edits; entries in the row menu's edit group.
5. **Cmd+Shift+C copies with column names** in both grids (DBeaver's advanced-copy key; the advanced
   dialog itself is proposed for the roadmap, RES-42).

Test `data_keys_edit_rows_and_apply` (PG and MySQL): Alt+Insert adds a row, Ctrl+D fills it from the row
above, Esc reverts one cell, Alt+Delete marks a row deleted, Cmd+S opens the review with the three
statements; cancel leaves the pending changes.

## G3 — Record mode — `opus`

Rows: RES-02, RES-18, CM-113. Files: new `crates/kerno/src/query/record.rs`, `query/mod.rs`,
`query/tab.rs`, `data/tab.rs`, `actions.rs`, `ui_tests.rs`. After CTX Part 3 and M6 Part 1.

DBeaver: Tab in a focused result toggles grid and record mode (`core.resultset.toggleMode`,
`ToggleModeAction` in `ResultSetViewer.fillLayoutMenu`); record mode shows the current row as a two-column
list (column, value), with first / previous / next / last row (`row.first` … `row.last`,
Ctrl+Alt+Left / Right, Ctrl+Alt+Shift+Left / Right) in the result toolbar.

Build a read view over the same `Grid` delegate (no second store): `RecordView` lists the columns of the
selected row, with type-aware rendering from `format.rs`, NULL styled as in the grid, long values
truncated with the value viewer on double click. Toggle with Tab when the grid has the focus, a toolbar
button and a palette command "Toggle Record View"; Previous / Next row buttons and the keys above. In a
data tab, editable cells are edited in place through the existing edit path (`DataTab::edit_cell`), so
pending changes, Review SQL and Apply work unchanged. Test
`record_view_shows_one_row_and_edits_it` (PG): toggle, the view lists `shop.customers`' columns of row 1,
Next shows row 2, editing a value adds one pending change, Tab returns to the grid with the same row
selected.

## G4 — Running in the SQL editor and result tabs — `opus`

Rows: SQL-04, SQL-06, SQL-09, SQL-12, SQL-18, SQL-19, SQL-21, CM-97, CM-128, CM-129, CM-130 ("to the left" also noted
in row CM-126). Files: `crates/kerno/src/query/tab.rs`, `actions.rs`, `workspace.rs` (tab menu
entry only), `ui_tests.rs`. After CTX Part 2.

1. **Run connects first** (SQL-04). DBeaver connects an editor's connection on execute
   (`ui.editors.sql` pref `EDITOR_CONNECT_ON_EXECUTE`, default true). Today a tab of a disconnected saved
   connection says what to do. Build: Run / Run all / Explain on a tab whose connection is saved but not
   connected starts the connection (password prompt when needed, the usual errors), then runs.
   `a_tab_without_an_open_connection_says_what_to_do` keeps covering the "no connection chosen" case.
   Test `running_in_a_tab_of_a_closed_connection_connects_first`.
2. **Run in a new result tab** (SQL-06). DBeaver `run.statementNew` (`Ctrl+\`) keeps the previous results and
   adds a tab; a plain run replaces the current results (pref `RESULT_SET_REPLACE_CURRENT_TAB`, true).
   Command `run_statement_new_tab` "Run Statement in New Result Tab", key `secondary-\`: the new panes are
   appended. Test: two runs with `Cmd+\` leave two result tabs.
3. **Result tab menu and pinning** (SQL-19, CM-128 to CM-130). DBeaver's result tab menu (`SQLEditor`,
   `action_result_tabs_*`): Close, Close others, Close tabs to the left / right, Close all, Pin / Unpin
   (Ctrl+Shift+P, `toggle.pinned.tab`), Unpin all, Set tab name; pinned tabs are not replaced by the next
   run. Build the same on Kerno's result `TabBar` (right-click and a middle click closes); a renamed tab
   keeps its name; pinned panes survive `results.clear()`. Detach is proposed for the roadmap (SQL-22).
   Test `result_tabs_close_pin_and_rename`: pin result 1, run again, result 1 is still there.
4. **Statement navigation and partial runs** (SQL-18, SQL-09). DBeaver `query.next` / `query.prev`
   (Alt+Down / Alt+Up) move the cursor to the next / previous statement; `run.scriptFromPosition` (Alt+P)
   runs from the statement at the cursor to the end. Build both with `split_statements` and
   `statement_at`; commands in the "Query" group. Test on a three-statement script.
5. **Row count of the statement** (SQL-12). DBeaver `run.count` counts the rows the query under the cursor
   would return. Command "Count Rows of Statement": for a statement classified as a read, run
   `SELECT COUNT(*) FROM (<statement>) kerno_count` on the tab's session and show the number in the
   messages and a notification; refused with a reason for other statements. Test on PG and MySQL.
6. **Results area** (SQL-21, CM-97). DBeaver toggles, maximises and focuses the results panel
   (`toggle.result.panel`, `maximize.result.panel`, `switch.panel`). Commands "Toggle Results Panel",
   "Maximize Results Panel" (editor hidden until toggled back), "Focus Results / Editor". Test the
   visible state.
7. **Close tabs to the left** (CM-126) on the window tab menu that CTX Part 2 builds.

## G5 — Filters, order, counts and auto-refresh on results — `opus`

Rows: RES-04, RES-06, RES-09, RES-11, RES-23, RES-25 (distinct values also noted in row CM-106).
Files:
`crates/kerno-core/src/browse.rs` (one wrapping function, unit-tested), `crates/kerno/src/query/tab.rs`,
`data/tab.rs`, `query/grid.rs` (header entry), `kerno-core/src/storage.rs` only through
`set_setting`, `ui_tests.rs`. After CTX Part 3.

1. **Filter and order a query result on the server** (RES-04, RES-09). DBeaver puts the same filter panel
   over an editor result (`ResultSetFilterPanel`) and re-runs the query with the criteria and the order
   when it cannot do it on the rows it holds. Build: for a result of one read statement, show the filter
   bar of the data tab; Filter and a header sort when not every row is fetched re-run
   `SELECT * FROM (<statement>) kerno_q WHERE … ORDER BY …` (`browse::wrap_sql(dialect, sql, filter,
   sort)`, values as literals like `page_sql`); when every row is fetched the header sort stays on the
   client. The original statement is kept and shown in the tooltip. Test (PG, MySQL): filter
   `status = 'shipped'` on `select * from shop.orders` returns only those rows; sort on a column of a
   partially fetched result returns the first rows in order.
2. **Filter history** (RES-06). DBeaver keeps the previous filters of the panel in a dropdown. Keep the
   last 20 applied raw WHERE texts per table (data tabs) or per statement text hash (results) in app
   storage (`set_setting`), offered in a dropdown on the raw input. Test: apply two filters, reopen the
   tab, the history lists both.
3. **Filter by distinct values** (RES-11). DBeaver `core.resultset.filterMenu.distinct` (Ctrl+F11,
   `valuefilter/`) shows the column's distinct values with counts as a checklist. Header menu entry
   "Filter by values…": `SELECT col, COUNT(*) FROM <source> GROUP BY col ORDER BY 2 DESC LIMIT 200`, a
   searchable checklist; OK adds an `IN (…)` condition (`IS NULL` for the NULL entry). Test on
   `shop.orders.status`.
4. **Count a query result** (RES-23). DBeaver `core.resultset.count` counts the rows of the result. A
   "Count" button in the result footer while more rows remain: counts through the wrapped statement and
   shows "1,234,567 rows". Test.
5. **Auto-refresh** (RES-25). DBeaver's refresh button has an auto-refresh menu with 1, 5, 10, 15, 30 and
   60 seconds (`ui` `AutoRefreshControl.AUTO_REFRESH_DEFAULTS`). Same choices on the data tab's Refresh and
   on a read result's footer; the timer (`cx.spawn` with the executor timer) re-reads the page or re-runs
   the read statement; it pauses while changes are pending, a dialog is open or a load runs, and stops on
   an error with a notice. Test with the simulated clock: 5 s re-reads once a row inserted outside
   (`on_server`) is visible.

## G6 — Grid navigation and panels — `opus`

Rows: RES-17, RES-27, RES-28, RES-31, RES-46, CM-111, CM-116, CM-123, CM-124. Files:
`crates/kerno/src/query/grid.rs`, `query/tab.rs`, `data/tab.rs`, `actions.rs`, `ui_tests.rs`. After CTX
Part 3.

1. **Selection statistics** (RES-27, RES-31). DBeaver's aggregate panel (`panel/aggregate`, toggled with
   Ctrl+Alt+F6; functions declared in `ui.editors.data` plugin.xml `aggregateFunction`: count and count
   distinct on by default, sum, average, minimum, maximum, median, mode on demand) works over the selected
   cells. Show count, count distinct, and for numeric cells sum, average, minimum and maximum in the result
   footer whenever more than one cell is selected, computed off the UI thread for large selections. Unit test of the computation; UI test on a
   numeric range.
2. **Metadata panel** (RES-28, CM-111). DBeaver `panel/metadata` lists the result's columns with their
   type, source table, key and nullability. A "Columns" panel (toggle in the result toolbar and a command)
   listing name, type and position from `ResultStore::columns()`; table, key and nullability when the
   result comes from a table (data tab: from `TableStructure`). Test.
3. **Go to row and column** (RES-17). DBeaver `grid.gotoRow` (Ctrl+G) and `grid.gotoColumn`
   (Ctrl+Shift+G). Commands "Go to Row…" (number prompt; fetches more when needed) and "Go to Column…"
   (filterable column list) with those keys in the grid context. Test.
4. **Row-number column menu** (RES-46, CM-123, CM-124). DBeaver's row header menu has "Copy row
   number(s)" plus the row entries (`ResultSetViewer.fillContextMenu`, `ROW_HEADER`). A right-click on the
   row number opens the row's menu with "Copy row number" first. Real right-click test.
5. **Refresh at the end of the cell menu** (CM-116), as DBeaver adds `file.refresh` last.

## G7 — Generate SQL from rows and routines — `opus`

Rows: NAV-46, NAV-53, RES-66, CM-52, CM-68, CM-80, CM-81, CM-115. Files:
`crates/kerno-core/src/sql/generate.rs` (+ tests), `crates/kerno/src/query/grid.rs` (menu entry),
`data/tab.rs`, `navigator/actions.rs`, `navigator/view.rs`, `ui_tests.rs`; drivers' `introspect.rs` only if
routine parameters lack names.

1. **Rows to SQL** (RES-66, CM-115). DBeaver's result-row generators (`ui.editors.sql` plugin.xml
   `sqlGenerator`: dataSelect, dataSelectMany, dataInsert, dataUpdate, dataDeleteByUniqueKey,
   ddlByResultSet) turn the selected rows into SELECT … WHERE key = …, SELECT … WHERE key IN (…), INSERT,
   UPDATE (all columns, by key) and DELETE (by key). Add "Generate SQL ▸" to the row menu of table data
   with those five, built from the table's key (the identity the data tab already uses) and the dialect's
   literals; the script opens in a new query tab bound to the table's schema and runs nothing. On query
   results the entry exists only for editable results (G8), else it is disabled with the reason. Unit
   tests of the statements (PG and MySQL quoting, NULLs, several rows); UI test.
2. **Call a routine** (NAV-53, CM-80, CM-81). DBeaver `procedureCall` and `core.procedure.execute` open a
   console with the call and its parameters. Add "Generate CALL" and "Execute…" on functions and
   procedures: PostgreSQL procedure `CALL s.p(:a, :b)`, function `SELECT * FROM s.f(:a)`; MySQL `CALL
   db.p(:a)` / `SELECT db.f(:a)`; parameter names from `RoutineInfo::arguments` (OUT parameters left out on
   PostgreSQL functions, `NULL` placeholders for MySQL OUT); "Execute…" runs it so the parameter prompt
   asks. Unit tests per engine; UI test calling a fixture function.
3. **Read data in SQL console** (NAV-46, CM-52, CM-68). DBeaver `core.sql.editor.forSelection` opens a
   console with a SELECT of the table and runs it. Entry "Read in SQL console" on tables and views:
   Generate SELECT, then run. Test.

## G8 — Edit the result of a query — `opus` (largest part)

Rows: RES-51. Files: `crates/kerno-core/src/sql/classify.rs` (single-table detection, unit-tested),
`crates/kerno/src/query/tab.rs`, `query/grid.rs`, `data/tab.rs` (share, do not copy, the edit plumbing),
`ui_tests.rs`. After G2.

DBeaver makes an editor result editable when it comes from one table and holds a unique key
(`ResultSetViewer` single-source model; pref `RS_EDIT_DISABLE_IF_KEY_MISSING` false: without a key it
asks to use all columns — Kerno keeps "read-only and why"). Build: a result whose statement is a plain
`SELECT` of columns from one table (no join, group, distinct, union, set-returning function; aliases and
`WHERE` / `ORDER BY` / `LIMIT` allowed) and whose columns include the table's primary key (or a NOT NULL
unique index, as `DataTab` decides) turns on the grid's edit layer: inline edit, Set NULL / default, add /
duplicate / delete rows, Review SQL, Apply and Discard through `sql::edit_statements`, `apply_edits` and
`sql_confirm`, exactly like a data tab. Other results stay read-only and the footer says why ("joins are
not editable", "the key column `id` is not in the result"). Tests (PG and MySQL): edit
`select id, name from shop.customers where id < 5`, apply, the server has the change; a join result says
why it is read-only.

## G9 — Value editor — `opus`

Rows: RES-50. Files: `crates/kerno/src/query/viewer.rs`, `data/tab.rs`, `query/grid.rs`, `ui_tests.rs`.
After G2 (and coordinate with `docs/plans/M10.md` Part 4: viewer text must be selectable).

DBeaver `core.resultset.row.edit` (Shift+Enter) opens a value editor for the cell: text, JSON with
formatting, hex for binary (`ui.editors.data` value managers). Build: Shift+Enter, and "Edit value…" in the
cell menu, open the viewer in edit mode for a writable cell: a multi-line editor; JSON columns
pretty-printed, validated before OK (an invalid JSON is refused with the parser's message); OK sets one
pending edit; binary stays view-only. Test: edit a JSON cell through the dialog, the pending change holds
the new value, Apply writes it.

## G10 — Navigator tree completeness — `opus`

Rows: NAV-04, NAV-05, NAV-06, NAV-07, NAV-21, NAV-42, NAV-44, CM-07, CM-72, CM-73, CM-76, CM-77. Files:
`crates/kerno/src/navigator/model.rs`, `view.rs`, `actions.rs`, `structure/editor.rs` (open on a column),
`kerno-core/src/dialect.rs` (cascade flag on `drop_sql` if needed), `ui_tests.rs`.

1. **Folders under each table** (NAV-04 to NAV-07). DBeaver's trees (`ext.postgresql` and `ext.mysql`
   plugin.xml) put Columns, Constraints, Foreign Keys, Indexes, References and Triggers under a table.
   Kerno shows Columns and Indexes from the `TableStructure` it loads once; add Constraints (primary,
   unique, check, with their columns), Foreign keys (columns → referenced table), References (other
   tables' keys pointing here) and Triggers, from the same load. A double click on a foreign key or a
   reference opens the other table's structure. Extend `a_table_loads_its_columns_and_indexes_once` and
   `the_schema_tree_opens_level_by_level` (PG and MySQL).
2. **Create the other object kinds** (NAV-42). DBeaver's Create ▸ on Functions, Procedures, Sequences,
   Triggers and Events folders opens a new-object editor. Like "Create view…" today: a statement template
   per engine in a query tab (PostgreSQL `CREATE FUNCTION … LANGUAGE plpgsql`, `CREATE PROCEDURE`,
   `CREATE SEQUENCE`, `CREATE TRIGGER … EXECUTE FUNCTION`; MySQL `CREATE FUNCTION` / `PROCEDURE` with a
   `BEGIN … END` body that the splitter keeps whole, `CREATE TRIGGER`, `CREATE EVENT`). Unit test of the
   menu; UI test that each template runs on a scratch schema after filling the names.
3. **Column nodes** (CM-72, CM-73, CM-76, CM-77). DBeaver offers Edit (F4), Create ▸ Column, Delete and
   Rename on a column. Add "Rename column…" and "Drop column…" (a `TableDraft` change through
   `Dialect::alter_table_sql` and `sql_confirm`), "Add column…" and Edit (F4) opening the table form with
   the column selected. Tests on PG and MySQL.
4. **Drop with cascade** (NAV-44). DBeaver's delete confirmation has a cascade option and shows the script
   (`NavigatorObjectsDeleter`, `confirm_deleting_delete_cascade_checkbox_label`). On PostgreSQL the drop confirmation gets a "Cascade" checkbox that
   switches the previewed statement to `… CASCADE` (MySQL has no cascade for these drops: no checkbox).
   Test: dropping a table referenced by a view fails without cascade and succeeds with it.
5. **Reveal in navigator** (NAV-21, CM-07). DBeaver `core.navigator.linkeditor` (Ctrl+Shift+,) selects the
   active editor's object in the tree. Command "Reveal in Navigator" (`secondary-shift-,`) for a data or
   structure tab: switches the navigator to that connection, expands the path, selects the table. Test.

## G11 — Navigator object filters — `opus`

Rows: NAV-19, CM-20, CM-37, CM-60. Files: `crates/kerno-core/src/config.rs`, `crates/kerno/src/navigator/
model.rs`, `actions.rs`, `view.rs`, `connections/manager.rs`, `ui_tests.rs`.

DBeaver's Filter ▸ (`core.object.filter.add.exclude` "Hide selected object(s)", `.add.include` "Show only
selected objects", `.config` "Configure filter", `.toggle`, `.clear`) stores include / exclude masks per
connection and per container kind (databases, schemas, tables), with `*` and `?` wildcards; a filtered
folder shows that it is filtered. Build: `ConnectionConfig::navigator_filters` (per kind: include and
exclude lists; carried by export / import, no secret), applied by the tree model when children load; menu
entries on databases, schemas and tables (Hide, Show only this, Configure… with two text lists, Clear),
folder label suffix "(filtered)". Tests: unit tests of the matching; UI test hiding a schema, restart, it
stays hidden; Clear shows it again.

## G12 — Engine maintenance tools — `opus`

Rows: TOOL-05, TOOL-06, TOOL-08, TOOL-09, TOOL-10, TOOL-11, CM-40, CM-56, CM-57, CM-70, CM-90. Files:
`crates/kerno-core/src/dialect.rs` and both drivers' `dialect.rs` (statements, unit-tested),
`crates/kerno/src/navigator/actions.rs`, `view.rs`, `sql_confirm.rs` (option checkboxes if the existing
variants do not fit), `ui_tests.rs`.

DBeaver groups them under Tools ▸ (`ext.postgresql.ui` and `ext.mysql.ui` plugin.xml, extension
`org.jkiss.dbeaver.tools`) and shows each tool's output in a log. Build each as a menu entry whose
statement is previewed and confirmed (they write; a read-only connection refuses), then run in a query tab
of that target so notices and result rows show in its Messages and Result tabs:

- PostgreSQL **Refresh materialized view** (TOOL-09): `REFRESH MATERIALIZED VIEW s.v WITH DATA | WITH NO DATA`
  (`PostgreToolMViewRefresh`, which offers that one option); add CONCURRENTLY as a second option when the
  view has a unique index (the server refuses it otherwise).
- PostgreSQL **Analyze** (TOOL-05) on a table (`ANALYZE VERBOSE s.t`) and on a database (`ANALYZE
  VERBOSE`), as `PostgreToolBaseVacuum` builds it.
- PostgreSQL **Vacuum** (TOOL-06) with options FULL, FREEZE, VERBOSE, ANALYZE, DISABLE_PAGE_SKIPPING,
  SKIP_LOCKED, INDEX_CLEANUP (`PostgreToolBaseVacuum`: `VACUUM (…) s.t`); it cannot run in a transaction
  block, so it runs on an auto-commit session.
- PostgreSQL **Truncate options** (TOOL-08): ONLY, RESTART IDENTITY, CASCADE on the existing Truncate
  confirmation (`PostgreToolTableTruncate`).
- PostgreSQL **Enable / Disable trigger** (TOOL-10, CM-90): `ALTER TABLE s.t ENABLE | DISABLE TRIGGER trg`
  on trigger nodes (`PostgreToolTriggerEnable` / `Disable`); the tree shows a disabled trigger as such.
- MySQL / MariaDB **Analyze, Check, Optimize, Repair** (TOOL-11): `ANALYZE TABLE`, `CHECK TABLE`,
  `OPTIMIZE TABLE`, `REPAIR TABLE` (InnoDB answers "not supported" for Repair: shown as returned), whose
  result rows (Table, Op, Msg_type, Msg_text) show in the result grid.

Tests: dialect unit tests (exact strings); UI tests on scratch objects: refresh a materialized view after
inserting into its source (the new row appears), vacuum with ANALYZE, disable then enable a trigger, MySQL
OPTIMIZE returns its status rows.

## G13 — Connection settings — `opus`

Rows: CON-07, CON-12, CON-13, CON-16, CON-17, CON-33, CON-34, CON-47, CM-12. Files:
`crates/kerno-core/src/config.rs`, `connector.rs` (bootstrap), `crates/kerno/src/connections/form.rs`,
`dialog.rs`, `manager.rs`, `query/tab.rs` (initial commit mode), `workspace.rs` (tab badge), `sidebar.rs`,
`actions.rs`, `ui_tests.rs`. After CTX Part 1.

1. **Commit mode default** (CON-07, CON-13). DBeaver's connection types set auto-commit: Development and
   Test on, Production off (`DBPConnectionType` in the model plugin); the Initialization page overrides it
   per connection (`ConnectionPageInitialization`). Add `ConnectionConfig::auto_commit: Option<bool>`
   (`None` = from the environment: production → manual; serde default keeps old stores); a select in the
   dialog's Advanced section; new query tabs of that connection start in that mode. Test: a production
   connection's new tab is in manual commit.
2. **Bootstrap queries and default schema** (CON-17, CON-16). DBeaver runs "Bootstrap queries" after each
   connect, with an "Ignore SQL errors" option (`EditBootstrapQueriesDialog`), and sets a default schema
   (Initialization page). Add `init_sql: Vec<String>`, `ignore_init_errors: bool` and `default_schema:
   Option<String>` (PostgreSQL: `SET search_path TO <schema>, public`; MySQL uses the default database) run
   on every session the connection opens (meta session and tab sessions); a failing statement aborts the
   connect with its text unless errors are ignored. No secret is allowed in these fields (reuse the
   options check of `form.rs`). Tests: `SET TIME ZONE 'UTC'` / `SET time_zone = '+00:00'` is in effect in a
   tab; a failing bootstrap says which statement failed.
3. **Read-only toggle** (CON-12). DBeaver `core.connection.readonly` flips it from the Database menu. Entry
   "Read-only" (checked state) in the connection menu and a palette command: saves the flag; open sessions
   are re-opened so the server-side read-only mode follows. Test: toggle on, a write in an open tab is
   refused.
4. **Disconnect all / others / folder** (CON-33, CON-34, CM-12). DBeaver `core.disconnectAll`,
   `core.disconnectOther`, `folder.disconnect`. Commands "Disconnect All" and "Disconnect Others" (the
   selected connection stays), and "Disconnect" on a folder. Each asks first when a tab holds an open
   transaction or pending edits (M5 Part 4 rules). Test.
5. **Connection colour on tabs and the status bar** (CON-47). DBeaver tints editors and toolbars with the
   connection type colour (`CSSUtils.markConnectionTypeColor`). Show the connection's tag colour (else its
   environment colour) as a stripe on each tab bound to it and on the status bar's connection item; check
   the contrast rule of M10 Part 4. Test: the tab exposes its colour through a test accessor.

## G14 — Transaction log — `opus`, small

Rows: CON-45, CM-133. Files: `crates/kerno/src/query/tab.rs`, `workspace.rs` (status item menu, after CTX
Part 2), `ui_tests.rs`.

DBeaver `core.txn.log` lists the statements run in the current transaction with time, duration and result
(opened from the transaction monitor and the Commit / Rollback dropdowns). Build: while a tab's transaction
is open, keep its statements (text, start time, duration, rows affected, error); clear them at commit /
rollback; a "Transaction log" entry beside Commit / Rollback and on the status bar's transaction item opens
a list of them. Test: in manual mode run an INSERT and an UPDATE, the log lists both; after Rollback it is
empty.

## G15 — Editor commands — `opus`

Rows: SQL-33, SQL-35, SQL-41, SQL-48, SQL-52, CM-93, CM-95, CM-100, CM-101 (keys also noted in row SQL-03).
Files: `crates/kerno/src/query/tab.rs`, `query/comment.rs`, `query/completion.rs`, `actions.rs`, `main.rs`
(Edit menu), `ui_tests.rs`. Menu items after CTX Part 2.

1. **Block comment** (SQL-33): DBeaver `comment.multi` (Ctrl+Shift+/) wraps the selection in `/* */` or
   unwraps it. Command with `secondary-shift-/`, one undo step; unit test in `comment.rs`.
2. **Upper / lower case** (SQL-35): DBeaver's Format ▸ upper / lower case (Eclipse text actions). Commands
   "Upper Case" / "Lower Case" on the selection (`secondary-shift-x` / `secondary-shift-y` if free), one
   undo step.
3. **Copy statement under the cursor** (SQL-48): DBeaver `copy.query`. Command and editor-menu entry.
4. **Open declaration** (SQL-41): DBeaver `navigate.object` (F4, and Ctrl+click) opens the object under the
   cursor. Resolve the identifier at the cursor against the tab's completion catalog (schema-qualified or
   through the search path) and open its structure tab (tables, views) or source (routines); F4 and
   Cmd+click. Test on `select * from shop.orders` with the cursor on `orders`.
5. **Refresh completion metadata** (SQL-52): DBeaver `refresh.current.schema` / `refresh.all.schemas`.
   Command "Refresh Completion Metadata" reloads the tab's catalog. Test: a table created outside the app
   is proposed after the command.
6. **Target selector keys** (SQL-03): DBeaver Ctrl+9 / Ctrl+0 open the connection / schema choosers
   (`ui.tools.select.connection` / `.schema`). Bind `ctrl-9` and `ctrl-0` to open the tab's connection and
   schema dropdowns.
7. **Editor menu entries** (CM-93, CM-95, CM-100, CM-101): add to the editor menu that CTX Part 2 builds:
   Run in new result tab, Run from cursor, Count rows, Export from query; Set connection ▸ / database ▸ /
   schema ▸ (the toolbar's lists); Copy statement; Open declaration. Real right-click test listing them.

## G16 — Administration additions — `opus`, small

Rows: TOOL-13, TOOL-14 (also noted in rows NAV-12, CM-22). Files: `crates/kerno-core/src/
driver.rs` (`AdminView::Locks`), both drivers' admin queries and `tests/`, `crates/kerno/src/admin/`,
`ui_tests.rs`. After M8 Part 4.

1. **Hide idle sessions** (TOOL-13). DBeaver's PostgreSQL session manager has "Show idle" and MySQL's
   "Hide sleeping" (`PostgreSessionEditor.ShowIdleAction`, `MySQLSessionEditor` `hideSleeping`). A toggle
   on the Sessions sub-tab: PostgreSQL hides `state = 'idle'`, MySQL / MariaDB hide `Command = 'Sleep'`;
   remembered in settings. Test.
2. **Locks** (TOOL-14). DBeaver's lock manager (`PostgreLockEditor`, `ui.editors.locks`) shows waiting and
   blocking sessions. Add `AdminView::Locks` and a "Locks" sub-tab: PostgreSQL from `pg_locks` joined to
   `pg_stat_activity` with `pg_blocking_pids()` (waiting pid, blocking pids, lock mode, relation, query,
   wait time); MySQL 8.4 from `performance_schema.data_lock_waits` joined to `data_locks` and threads;
   MariaDB from `information_schema.INNODB_LOCK_WAITS` / `INNODB_LOCKS` (check what MariaDB 11 still
   provides; if neither works, say so in the tab). Cancel / terminate reuse the Sessions actions. Test:
   two scratch sessions, one blocked on a row lock, the blocked one is listed with its blocker.

## G17 — Properties sub-tab and database options — `opus`, small

Rows: OBJ-03, OBJ-14 (properties also noted in row CM-33). Files: `crates/kerno-core/src/schema.rs`,
`driver.rs`, both drivers' `introspect.rs` and `dialect.rs` with tests, `crates/kerno/src/structure/tab.rs`,
`navigator/view.rs` (create database prompt), `sql_confirm.rs` if a form variant is needed, `ui_tests.rs`.

1. **Properties** (OBJ-03). DBeaver's object editor opens on a properties page (owner, comment, row
   estimate, sizes; MySQL engine, collation, auto-increment, row format). Add a "Properties" sub-tab first
   in the structure tab: PostgreSQL owner, persistence, estimated rows, table / index / total size,
   comment; MySQL / MariaDB engine, collation, row format, estimated rows, data / index length,
   auto-increment, comment (queries from `pg_class` / `pg_tables` and `information_schema.tables`). Test
   on the three engines.
2. **Create database options** (OBJ-14). DBeaver's create-database dialogs ask owner, encoding and template
   on PostgreSQL, character set and collation on MySQL. Extend "Create database…" with these optional
   fields (lists from `pg_roles` / `pg_encoding_to_char` and `SHOW CHARACTER SET` / `SHOW COLLATION`),
   previewed in the statement. Dialect unit tests; UI test creating a scratch database with a collation
   and dropping it.

## G18 — Test debt: what exists without a test — `opus` or `sonnet`

Rows: every **done, untested** row of `docs/DBEAVER-PARITY.md`. Files: `crates/kerno/src/ui_tests.rs`
(test accessors in the views when needed). The sidebar menu rows (CM-16, CM-24, CM-25, CM-28, CM-30) and
the header menu rows (CM-117, CM-120, CM-121) are covered by the right-click tests that `docs/plans/CTX.md`
Parts 1 and 3 already order: check them off when those land. Write the others:

- CON-24 SSH: keep-alive interval: `SshConfig::keepalive_secs` passed to the SSH client (`kerno-core/src/ssh.rs`); no test checks it (G18)
- SQL-03 Active connection / schema selectors (Ctrl+9 / Ctrl+0): Toolbar selectors `tab-connection`, `tab-database`, `tab-schema` in `query/tab.rs`; no test clicks them (G18); keys: G15
- SQL-42 Bracket and quote auto-closing: The kit's editor closes brackets (M5 done list); no test (G18)
- SQL-46 Multi-cursor: The kit's editor (Cmd+Alt+Up / Down, M5 done list); no test (G18)
- RES-21 Fetch next segment on scroll: `Grid::load_more` on reaching the end; no test scrolls to it (G18)
- RES-40 Move column left / right: Drag reorder in `query/grid.rs` (M5 done list); no test (G18)

Tests to add: `the_tab_toolbar_changes_its_connection_database_and_schema` (clicks `tab-connection`,
`tab-database`, `tab-schema`; a run then uses the new target), `the_editor_closes_brackets_and_quotes`,
`the_editor_adds_a_cursor_above_and_below`, `scrolling_to_the_end_of_a_result_fetches_the_next_page`,
`a_dragged_column_keeps_its_new_place` (or through the delegate's move function if a drag cannot be
simulated: say so). Core: `ssh_keepalive_is_passed_to_the_client` (unit test in `kerno-core/src/ssh.rs` that
`keepalive_secs` becomes the client's keep-alive interval, and 0 turns it off).

## Moved to the roadmap on 2026-11-13 (decided)

These rows were accepted for `docs/ROADMAP.md` with the reasons below and are no longer gaps.

| ID | Feature | PG / MySQL | Why it can wait |
| -- | ---- | ---- | ---- |
| CON-09 | Edit connection types | PG, My | Kerno has three fixed environments plus free colour tags (CLAUDE.md §4.A1); custom types add little |
| CON-14 | Default isolation level | PG, My | Rarely changed; a `SET TRANSACTION` in bootstrap queries (G13) covers it |
| CON-18 | Keep-alive (database) | PG, My | Off by default in DBeaver; Kerno re-establishes a dropped connection (`a_lost_connection_is_re_established`) |
| CON-19 | Close idle connection after N seconds | PG, My | Resource saving, not a daily need; needs an idle timer per connection |
| CON-20 | Auto-close idle transactions | PG, My | Safety net for manual mode; Kerno already asks before closing a tab with an open transaction (M5 Part 4) |
| CON-26 | SSH: jump servers | PG, My | Beyond CLAUDE.md §4.A4 (one tunnel); bastion chains are uncommon |
| CON-27 | SSH: local / remote host and port overrides, connect timeout | PG, My | Automatic local port is enough for a client; rare settings |
| CON-28 | SSH: share tunnel between connections | PG, My | Optimisation only |
| CON-29 | SOCKS proxy and network profiles | PG, My | Not in CLAUDE.md §4.A; enterprise networks only |
| CON-42 | Smart commit mode | PG, My | Off by default in DBeaver; manual mode per tab covers the need |
| CON-46 | Pending transactions | PG, My | Kerno shows the open transaction per tab in the status bar; a cross-app list is rarely needed |
| CON-48 | Change user password | PG, My | An `ALTER USER` in the editor does it; rare |
| CON-49 | Shell commands on connection events | PG, My | Runs local programs from a connection; outside CLAUDE.md §4.A and a security surface |
| NAV-08 | Partitions and child tables | PG, My | Partitions are listed as tables of the schema; a nested view is a nice-to-have |
| NAV-09 | Rules, policies, dependencies | PG | Rare in daily use; visible through Show DDL and the catalogs |
| NAV-17 | Filter by object type | PG, My | The filter keeps matches and their path; type scoping is rarely used |
| NAV-18 | Show connected only | PG, My | Small list of connections in a client; folders (CTX Part 1) organise them |
| NAV-23 | Object descriptions in the tree | PG, My | Off by default in DBeaver |
| NAV-24 | Show system objects | PG, My | System catalogs are reachable from the editor; the tree hides them by design |
| NAV-34 | Multiple selection | PG, My | Single selection covers daily use; multi-object actions multiply confirmation flows |
| NAV-35 | Drag an object into the SQL editor | PG, My | Completion inserts names; drag between GPUI views needs a custom drop target in the editor |
| NAV-43 | New Index / Constraint from selected columns | PG, My | The table form adds indexes and keys (M7 Part 3); needs multi-selection (NAV-34) |
| NAV-51 | Generate SQL ▸ MERGE, JOIN, INNER JOIN | PG, My | JOIN needs multi-selection (NAV-34); completion proposes joins on foreign keys already |
| NAV-52 | Generate SQL ▸ INSERT ON CONFLICT, UPDATE FROM, DELETE USING | PG | PostgreSQL-only variants of templates that exist |
| NAV-59 | Connection view ▸ (simple / advanced / custom) | PG, My | One tree layout; system objects (NAV-24) and descriptions (NAV-23) proposed separately |
| SQL-10 | Execute script natively (Alt+N) | PG, My | The client tools are used for backup / restore (M8 Part 3); running editor scripts through them adds little |
| SQL-14 | Evaluate SQL expression | PG, My | Selecting `select <expr>` and running it does the same |
| SQL-17 | Load execution plan | PG, My | Plan files from elsewhere are rare; raw output can be pasted |
| SQL-20 | Multiple results per tab | PG, My | Layout preference; one tab per statement is the default in both apps |
| SQL-22 | Toggle editor layout | PG, My | Layout preference |
| SQL-26 | Script variables and commands (`@set`, `@include`, `@echo`, `@export`, variables panel) | PG, My | DBeaver-specific script language; not standard SQL |
| SQL-27 | Outline | PG, My | Nice-to-have for long scripts |
| SQL-36 | Format ▸ Morph to delimited list, trim spaces | PG, My | Text utilities, not SQL client features |
| SQL-40 | Templates | PG, My | Saved queries cover reusable SQL |
| SQL-43 | Go to / select to matching bracket | PG, My | Editor-kit feature; low use |
| SQL-44 | Foldings | PG, My | Depends on the kit's editor |
| SQL-49 | Advanced copy in the editor | PG, My | Plain copy is enough for code |
| SQL-51 | Set connection from navigator, auto-sync | PG, My | Off by default; "New query here" binds a tab to a node |
| SQL-53 | Disable SQL syntax parser | PG, My | Kerno parses only the statement at the cursor (M9 checks large scripts), so there is nothing to turn off |
| RES-03 | Text presentation, switch presentation | PG, My | Copy as Markdown gives a text table; second renderer to maintain |
| RES-07 | Filter autocompletion | PG, My | The builder lists the columns |
| RES-13 | Save / reset default filter | PG, My | Saved queries hold a filtered SELECT |
| RES-14 | Customize filters dialog (Alt+Shift+F) | PG, My | The filter bar and the header menus cover it |
| RES-29 | Panels: grouping | PG, My | A GROUP BY in the editor does it |
| RES-32 | View / Format ▸ display format, transformers, binary format | PG, My | Kerno shows server text and hex for binary (CLAUDE.md §4.D2); formats are a preference set |
| RES-33 | Row colours by value, colorize data types | PG, My | Visual nicety |
| RES-39 | Hide columns with no data | PG, My | Rare |
| RES-42 | Advanced Copy (Ctrl+Shift+C) | PG, My | Copy with column names and Copy as CSV / TSV cover the usual options; the shortcut is in G2 |
| RES-48 | Advanced Paste (Ctrl+Shift+V) | PG, My | Paste into cells comes with M6 Part 1; inserting new rows from a spreadsheet is less common |
| RES-54 | Add / duplicate row before | PG, My | Row position in an unsorted table is not meaningful to the server |
| RES-71 | Virtual keys and logical structure | PG, My | Keyless tables stay read-only with the reason (CLAUDE.md §4.E2) |
| RES-72 | Generate UUID | PG, My | `gen_random_uuid()` / `uuid()` as a default covers it |
| OBJ-07 | Column order | My | Noted as not editable in M7; MySQL-only |
| OBJ-09 | Edit trigger source | PG, My | PostgreSQL trigger bodies are functions (editable); MySQL triggers are re-created from Show DDL |
| OBJ-11 | Sequence editor | PG | Show DDL gives the definition; current value is a query away |
| OBJ-18 | Dependencies page | PG | See NAV-09 |
| TOOL-03 | Global backup (pg_dumpall) | PG | Server-wide dumps are a DBA task outside a client's daily use |
| TOOL-04 | Execute script with the local client | PG, My | Restore covers SQL files (M8 Part 3) |
| TOOL-15 | Session details: query text and plan | PG, My | M8 Part 4 shows the query column; the plan of another session is rarely needed |
| TOOL-17 | MySQL status, engines, charsets, collations, plugins | My | Available through SHOW statements in the editor |
| TOOL-27 | SQL terminal presentation | PG, My | Alternative presentation of results; outside CLAUDE.md §4 |
| CM-17 | Create ▸ (database, schema) | PG, My | Create database / schema exist on the navigator's database and schema nodes (`dropping_renaming_and_truncating_show_the_sql_and_ask`); the sidebar connection menu need not repeat them |
| CM-19 | Connection view ▸ | PG, My | See NAV-59 |
| CM-31 | Security ▸ Change user password | PG, My | See CON-48 |
| CM-51 | Generate SQL ▸ MERGE, INSERT ON CONFLICT, UPDATE FROM, DELETE USING | PG, My | See NAV-51, NAV-52 |
| CM-74 | New Index / Constraint from selection | PG, My | See NAV-43 |
| CM-112 | View / Format ▸, Hints, Logical structure | PG, My | See RES-32, RES-33, RES-71 |

## Added after the first documentation pass (2026-11-13)

- **CM-15 — Refresh on a connection folder** (gap, small): add "Refresh" to the folder menu of
  `sidebar::menu` (reloads the saved list, as the empty-area entry does); extend
  `the_empty_area_and_a_folder_offer_what_dbeaver_does`. Do it with CTX Part 3 or G18.
- **CM-09 — Paste in the sidebar's empty area**: decided for the roadmap. DBeaver pastes a copied
  connection; Kerno's "Duplicate" on the connection does the same in one step. Mark the row `roadmap` with
  that reason at the next update of `docs/DBEAVER-PARITY.md` and list it in `docs/ROADMAP.md`.

## G10 — delivered on 2026-11-13, and what it left (small part, `opus`, app: `actions.rs`, `workspace.rs`,
`structure/editor.rs`, `sql_confirm.rs`)

Done: NAV-04 to NAV-07 (folders under a table), NAV-42 (create templates), CM-76 / CM-77 (rename and drop
a column), NAV-44 as a separate "Drop with cascade…" entry on PostgreSQL. Left:
- NAV-21 / CM-07 "Link with editor": `Navigator::reveal(&target, &table, kind, cx)` exists; add the command
  (`secondary-shift-,`, as DBeaver's Ctrl+Shift+,) and its workspace handler taking the active data or
  structure tab, and keep the sidebar's selected connection in step. UI test.
- CM-72 / CM-73: F4 on a column and "Add column…" should open the table form (`open_table_editor`) with
  that column selected or a new column row added, instead of the structure tab and an `ALTER` template.
- NAV-44: DBeaver offers cascade as a checkbox in the drop confirmation; fold the separate entry into a
  checkbox of `sql_confirm` when that dialog is next touched (PostgreSQL only).
- Tests: the column test on MariaDB; the table-level trigger and MariaDB sequence templates on a server.

## G13 — delivered on 2026-11-13, and what it left (one `opus` agent, app: `sidebar.rs`, `actions.rs`,
`workspace.rs`, `tabs.rs`, `connections/manager.rs`)

Done: CON-07 and CON-13 (commit mode per connection, manual by default on production), CON-16 (default
schema on PostgreSQL), CON-17 (bootstrap statements; Kerno refuses ones that write, change the schema or
name a secret — a deviation from DBeaver recorded in DECISIONS). Left:
- CON-12: toggle read-only from the connection's menu and the palette (save the flag, then reconnect so
  every session reopens read-only).
- CON-33, CON-34, CM-12: disconnect / reconnect commands that first ask about open transactions of that
  connection's tabs (the same question as closing a tab), and "Disconnect all".
- CON-47: the connection's tag colour on its tabs (a stripe) and on the status bar's connection item.
- The commit mode is read when a query tab is created: a tab switched to another connection keeps its
  mode — make `set_target` re-read it when the tab has no open transaction.
- `SET password_encryption = …` is refused as a bootstrap statement because the word contains
  "password": match secrets as values (`PASSWORD '…'`, `IDENTIFIED BY`), not as substrings of identifiers.
- A tunnel test with a bootstrap statement; a capture of the dialog's Advanced section.

## G12 — app half delivered on 2026-11-13, and what it left (one `opus` agent, app: `maintenance_dialog.rs`,
`navigator/actions.rs`, `navigator/view.rs`, `actions.rs`, `workspace.rs`, `ui_tests.rs`; not yet committed
when this note was written; the core half is `7ac5318`)

Done: TOOL-05, TOOL-06, TOOL-08, TOOL-09, TOOL-10, TOOL-11, CM-40, CM-56, CM-57, CM-70, CM-90. The Tools ▸
menu of the tree (and a palette command) lists what the engine offers for each object and opens a
maintenance dialog that runs the tool on a session of its own, shows the SQL, the log and the server's
status rows, and can cancel a waiting statement; a read-only connection refuses, production asks twice.
Left:
- Closing the dialog while its statement waits (a lock) cancels it on the server; foreign tables keep Truncate (PostgreSQL 14+ truncates them through their wrapper).
- A trigger shows twice in the tree and only the copy acted on is refreshed after Enable / Disable.
- Concurrently is offered on Refresh materialized view without checking for a unique index; the server's
  error is shown instead.
- The "Done in" time includes opening the session.

## G7, G15, the G10 leftovers, CM-09 and CM-15 — delivered on 2026-11-13, and what they left (one `opus`
agent; core: `sql/generate.rs`, `sql/text.rs`; app: `tabs.rs`, `query/tab.rs`, `query/grid.rs`,
`data/tab.rs`, `navigator/actions.rs`, `navigator/view.rs`, `structure/editor.rs`, `sidebar.rs`,
`actions.rs`, `main.rs`, `workspace.rs`, `ui_tests.rs`)

Done:
- G7: Generate SQL ▸ (SELECT by key, SELECT … IN, INSERT, UPDATE, DELETE) on the rows of table data,
  values as literals through `edit_statements`, opened in a query tab on the table's schema, nothing run;
  on a query result every entry is disabled with "query result"; a table without a key offers INSERT
  only (RES-66, CM-115). Generate CALL and Execute… on functions and procedures (NAV-53, CM-80, CM-81):
  PostgreSQL `CALL s.p(:a, NULL)` (OUT as NULL) / `SELECT * FROM s.f(:a)`; MySQL `SELECT db.f(:a)` /
  `CALL db.p(:a, @b); SELECT @b;` — MySQL refuses NULL for an OUT argument (error 1414), so OUT and INOUT
  are user variables, INOUT set first. Read data in SQL console on tables and views (NAV-46, CM-52, CM-68).
- G15: toggle block comment (Cmd+Shift+/), upper / lower case (Cmd+Shift+X / Y) leaving strings, quoted
  names and `:params` alone, copy statement at cursor, refresh completion metadata, export from query,
  revert to the saved file; the editor menu grouped as DBeaver's: Execute ▸ (with Active connection ▸ /
  database ▸ / schema ▸), Format ▸, File ▸ (SQL-33, SQL-35, SQL-48, SQL-52, CM-93, CM-95, CM-96, CM-98,
  CM-100).
- G10 leftovers: Link with editor (Cmd+Shift+, and a checked entry of the tree's empty-area menu; stored
  in the settings; reveals on a tab activation, never while tabs are restored) (NAV-21, CM-07). Edit on a
  column and Add column… open the table form on that column or on a new row (CM-72, CM-73); the
  `ALTER … ADD COLUMN` template is gone.
- CM-09: Copy ▸ on a connection remembers it inside the app (its id only); Paste in the list's empty area
  saves a copy, as Duplicate does (this overrides the roadmap decision above). CM-15: Refresh on a folder
  reads the list again and the tree when it shows a connection of the folder; nothing reconnects.

Left:
- RES-66's "CREATE from result" (DBeaver `ddlByResultSet`) is not written.
- SQL-52: one command; DBeaver's "current schema" and "all schemas" are the same here, because the
  completion catalog is read whole.
- CM-96: "Import script" is Open SQL file (a new tab, DBeaver loads into the current editor); "Export
  script" is Save as.
- CM-93: "all rows" is Fetch all rows of the result on screen, not DBeaver's run-and-fetch-all;
  "Evaluate expression" stays on the roadmap (SQL-14).
- The tree's entries (Read data in SQL console, Generate CALL, Execute…) have no palette command.
- Cmd+Shift+/ may be taken by macOS for the Help menu's search field; not checked in the real app.

## G3, G6, G9 — delivered on 2026-11-13, and what they left (one `opus` agent, app: `query/grid.rs`,
`query/grid_view.rs` (new), `query/viewer.rs`, `data/tab.rs`, `query/tab.rs`, `actions.rs`, `main.rs`,
`workspace.rs`, `settings.rs`; core: `aggregate.rs` (new))

- G3: Tab in a grid (results and table data) toggles the record view: one line per shown column (name,
  type, value), the current row's; Up / Down move between fields, Enter / F2 / a double click edit in place
  through the grid's own editor (pending changes, Review SQL, Apply unchanged), Tab goes back on the same
  row. First / Previous / Next / Last row (Ctrl+Alt+Shift+Left, Ctrl+Alt+Left / Right,
  Ctrl+Alt+Shift+Right) in both modes, buttons in the record view; a "Record" button in the result and
  data footers; Layout ▸ in the cell and row menus: Record view, Selection statistics, the three zoom
  entries (RES-02, RES-18, CM-113). Test `record_view_shows_one_row_and_edits_it`.
- G6: the footer shows "N cells (R rows, C columns) · D distinct · sum · avg · min · max" whenever more
  than one cell is selected (numbers only for numeric columns; off with Layout ▸ / "Toggle Selection
  Statistics", stored in the settings); more than 100,000 cells are counted off the UI thread with
  "computing…", stopped when the selection changes (RES-31). Panels ▸ Aggregate and Columns (palette:
  Show Aggregate / Columns Panel): count, count distinct, sum, average, minimum, maximum; position, name,
  type, and for a table's rows its table, primary key and nullability (RES-27, RES-28, CM-111). Go to Row…
  (Ctrl+G; a row not fetched yet is asked of the server and selected when it arrives; a table's page says
  the row is not on it) and Go to Column… (Ctrl+Shift+G, a filtered list) (RES-17). A right-click on a row
  number opens the row's menu: Copy row numbers first, Copy / Copy as, add / duplicate / delete / revert
  row and Clear filters on table data, Layout, Generate SQL, Export, Refresh; "Copy Row Numbers" in the
  palette (RES-46, CM-123, CM-124). Refresh ends the cell menu of table data (CM-116). Tests
  `selection_figures_show_in_the_footer_and_the_panels`, `go_to_row_and_column_move_the_selection`,
  `a_right_click_on_a_row_number_opens_the_menu_of_its_row`; units `aggregate::tests`,
  `grid_view::tests::the_columns_panel_adds_what_the_table_says`,
  `grid::tests::a_row_number_opens_the_menu_of_its_row`,
  `grid::tests::the_record_view_and_the_value_panel_are_in_the_cell_menu`.
- G9: Shift+Enter ("View or Edit Value of the Selected Cell") and "Edit in value panel" in the cell menu
  open the value panel editable on a cell that takes typed values: multi-line text, JSON laid out, Format
  / Compact JSON, an invalid JSON refused with the parser's message, Set NULL when the column takes it;
  Save is one pending change (one undo step), as typing in the cell. Binary values, query results and
  read-only rows open the viewer as before (RES-50, CM-109). Test
  `the_value_panel_edits_a_json_cell_and_apply_writes_it`.

Differs from DBeaver:
- Panels are dialogs, not panes docked beside the grid (the value viewer already was one); the footer is
  the live aggregate. No median or mode; minimum and maximum of numbers only (DBeaver also compares text).
- Count distinct stops counting past 1,000,000 different values ("over 1000000"): an exact set of a whole
  1M x 20 result would take most of a gigabyte.
- A whole column of a result still arriving keeps the figures of the rows it had when selected.
- Go to Row and Go to Column are not in the cell menu (DBeaver has them in its Navigate submenu).

Left:
- After Apply, the page is read into a new grid and the keyboard is not given back to it (as before this
  part); the tests focus the grid again.
- The record view shows one row; DBeaver's record mode with several selected rows shows them side by side.

## G11, G14, G16, G17 and CM-58 — delivered on 2026-11-13, and what they left (one `opus` agent; core:
`config.rs`, `dialect.rs`, `driver.rs`, `storage.rs`, both drivers' `admin.rs`, `dialect.rs` and
`tests/introspect.rs`; app: `navigator/` (`filter_dialog.rs` new), `database_dialog.rs` (new),
`query/tx_log.rs` (new), `admin/tab.rs`, `structure/tab.rs`, `sql_confirm.rs`, `backup_dialog.rs`,
`connections/`, `query/tab.rs`, `tabs.rs`, `actions.rs`, `main.rs`, `workspace.rs`, `settings.rs`)

- G14 (CON-45, CM-133): a query tab in manual commit keeps the statements of its open transaction (text
  redacted as the history is, time, duration, rows, status, error); emptied by Commit / Rollback and by a
  statement that ends the transaction (typed COMMIT, MySQL's implicit commits). "Transaction log…" on the
  status bar's transaction item and the palette / Database menu command "Transaction Log". Tests
  `the_transaction_log_lists_the_statements_until_the_transaction_ends`,
  `tx_log::tests::the_log_keeps_what_the_history_keeps`.
- G16 (TOOL-13, TOOL-14): "Hide idle" (PostgreSQL `state = 'idle'`) / "Hide sleeping" (MySQL, MariaDB
  `Command = 'Sleep'`) on the Sessions sub-tab, with the count hidden, kept in the settings. A Locks
  sub-tab (`AdminView::Locks`): one row per waiting session and session in its way, from
  `pg_blocking_pids()` and `pg_locks` (PostgreSQL), `performance_schema.data_lock_waits` and `threads`
  (MySQL 8.4), `information_schema.INNODB_LOCK_WAITS` / `INNODB_TRX` / `INNODB_LOCKS` (MariaDB 11),
  with both statements; Cancel / Terminate of the blocking session on the toolbar, and of either session
  in the row's menu, through the usual confirmation; auto-refresh. Tests
  `the_lock_manager_lists_who_waits_for_whom_and_ends_the_blocker`, drivers'
  `the_lock_view_names_the_waiting_session_and_its_blocker`.
- G17 (OBJ-03, CM-33 for tables and views, OBJ-14): a Properties sub-tab first in the structure tab
  (PostgreSQL owner, kind, persistence, tablespace, access method, estimated rows, sizes, row security,
  options, last vacuum / analyze, OID, comment; MySQL / MariaDB kind, engine, collation, row format, rows,
  sizes, next auto-increment, created, updated, options, a view's definer / security / updatable,
  comment); "Edit…" sets or removes a table's comment through the confirmation. F4 and a new "Properties"
  entry open a table or view there; "Open structure" still opens on the columns. Create database… is a
  form with the server's lists (PostgreSQL owner, template, encoding, LC_COLLATE, LC_CTYPE, tablespace;
  MySQL / MariaDB character set and its collations) and a live preview, then the confirmation. Tests
  `properties_open_first_on_f4_and_the_comment_is_set_from_there`,
  `a_database_is_created_with_the_options_of_the_form`, drivers' `properties_describe_tables_and_views`,
  `a_database_is_created_with_the_options_the_server_lists` /
  `a_database_is_created_with_a_character_set_and_collation`.
- G11 (NAV-19, CM-20, CM-37, CM-60): Filter ▸ on every object and folder of the tree (Hide this object,
  Show only this object, Configure…, Turn on or off, Clear) and five palette commands; masks with `*` and
  `?` per connection and kind (`databases`, `schemas`, the folder names) in
  `ConnectionConfig::navigator_filters` (exported with the connection, no migration: a JSON field with a
  default); applied when the tree is drawn, so a change shows at once; a filtered folder's hint says
  "N hidden" (or "filtered"), databases hidden say so in a line below them. Matching ported from
  DBeaver's `DBSObjectFilter`. Tests `object_filters_hide_schemas_and_last_after_a_restart`,
  `model::tests::object_filters_hide_by_kind_and_say_how_many`,
  `config::tests::object_filters_match_as_dbeaver_masks_do`.
- CM-58: "Back up…" on a table of the tree opens the backup with that table alone (`pg_dump --table`,
  `mysqldump <database> <table>`), the names passed as they are, never parsed from text; "Every table"
  goes back to the whole database. Test `a_table_is_backed_up_alone_from_the_tree`.

Differs from DBeaver:
- Object filter masks take `*` and `?` only: DBeaver also reads `%` and `_`, which would make hiding
  `order_items` hide `orderXitems` too. No filter per schema of another kind than its folder (DBeaver's
  global "filter for all schemas" is the same here).
- The lock manager is a table of waiting → blocking pairs, not DBeaver's tree, and shows InnoDB row and
  table locks only on MySQL / MariaDB (no metadata locks).
- A PostgreSQL database made with another encoding or locale than its template needs `template0`: the
  form says so; Kerno does not switch it silently.
- "Hide idle" leaves out PostgreSQL's `idle` sessions only, never `idle in transaction` ones: those hold
  locks.

Left:
- CM-33 on databases and schemas: F4 there still shows the DDL (no properties page of a database or
  schema).
- The tree selects one node at a time: a backup of several selected tables is typed in the dialog.
- "Link with editor" on a table hidden by a filter selects nothing (the node is loaded, not shown).
- The connection's sidebar menu has no "Lock Manager" entry: the Locks sub-tab is in Administration.
- The Properties page is read-only but for a table's comment; a view's comment is not set from it.
- The transaction log is also emptied when a new transaction begins (after a lost connection or a
  rollback on close); that path has no test.

## G5 — delivered on 2026-11-13, and what it left (one `opus` agent; core: `browse.rs`, `storage.rs`
(migration 2, `filter_history`); app: `query/tab.rs`, `query/grid.rs`, `query/distinct.rs` (new),
`data/tab.rs`, `actions.rs`, `main.rs`, `workspace.rs`, `ui_tests.rs`)

Done:
- RES-04, RES-09: the result of one SELECT (not one that writes, locks rows or holds several statements)
  has a filter bar (`WHERE`, Filter, History, Clear); a filter, a header sort while not every row is
  here (DBeaver's smart ordering: `ResultSetViewer.toggleSortOrder`), Filter by / Exclude value and
  Filter ▸ re-run `SELECT * FROM (\n<statement>\n) kerno_q WHERE … ORDER BY …`
  (`browse::Query`, `select_from`), which keeps paging. A fully fetched result sorts here; the footer
  says which ("ordered by the server" / "ordered here"). Duplicate column names are refused on MySQL and
  MariaDB (error 1060) and when named on PostgreSQL, with the reason. Test
  `a_select_result_is_filtered_ordered_and_counted_on_the_server` (all three engines: comment and `;`,
  CTE with ORDER BY and LIMIT, server order on the 1M-row table, duplicate names refused); units
  `browse::tests::*`, `grid::tests::a_select_result_filters_on_the_server_but_never_edits`.
- RES-23: "Count" in a result's footer and "Count exactly" on table data (palette "Calculate Total Row
  Count"); a result being paged is counted on a session of its own (its schema set first), so it keeps
  its pages; with a transaction open there it is refused with the reason. Count Rows of Statement now
  uses the same derived table. Same test, and `a_large_table_shows_its_estimate_and_counts_on_demand`.
- RES-11, CM-106: "Distinct values…" (Ctrl+F11 in a grid, as DBeaver) in Filter ▸ (with Clear filters)
  and in the header menu, on table data and SELECT results: the 200 most frequent values with their counts, searchable, picked values
  added as `col IN (…)` / `IS NULL` with literals (`browse::one_of_sql`). Test
  `distinct_values_of_a_result_and_of_a_table_become_a_filter` (PG, MySQL).
- RES-06: the last 20 filters per table (`data:` key) and per statement (`query:` key) in the app storage
  (migration 2), never one the history's redaction would mask; a History dropdown on both bars. Tests
  `the_filters_applied_to_a_table_are_offered_again`, `storage::tests::filters_are_kept_per_key_latest_first_without_secrets`.
- RES-25: Auto-refresh ▸ 1, 5, 10, 15, 30, 60 s on the data toolbar and a result's footer (and "Toggle
  Auto-Refresh", 10 s); ticks never overlap a read; stops on a pending change, a failure, the tab hidden,
  the result replaced or another result tab on screen, and says why in the footer; what is typed in the
  filter stays, and only the first run goes to the history. Test
  `auto_refresh_reads_again_until_a_change_or_another_tab`.

Differs from DBeaver:
- The result filter is one SQL text (DBeaver's panel too); menu filters are added to it as SQL. DBeaver
  first tries to patch the statement's own WHERE (JSQLParser); Kerno always wraps it.
- MySQL and MariaDB ignore the statement's own ORDER BY without LIMIT inside a derived table: the bar says
  so ("ignored the statement's ORDER BY: order by a column header").
- The distinct list is searched among the 200 values read, not on the server; it uses the current filter
  but for the column's own conditions (table data).
- Auto-refresh stops when the tab is hidden (DBeaver keeps it on in the background).

Left:
- A re-run result moves to the end of the result tabs (it stays active).
- DBeaver also lists "Calculate total row count" under Navigate ▸ while more rows remain; that submenu
  was being added to `grid.rs` by another part during G5, so the entry waits for it (the footer and the
  palette have it).
- Auto-refresh does not pause while a dialog is open (the plan asked for it); it skips a tick while a
  read runs.
- Distinct values on a JSON column of PostgreSQL fail on the server (`json` has no equality): the header
  menu does not know the column's type; the cell menu disables it.
- The filter history of results is keyed by the statement text; a statement differing by more than
  whitespace has its own.

## G18 and the leftovers of G10–G17 and M10 Part 4 — delivered on 2026-11-13, and what they left (one
`opus` agent; core: `ssh.rs`, `dialect.rs`; drivers: `dialect.rs`, `tests/introspect.rs`; app:
`navigator/actions.rs`, `navigator/view.rs`, `structure/tab.rs`, `sidebar.rs`, `admin/tab.rs`,
`actions.rs`, `main.rs`, `workspace.rs`, `data/tab.rs`, `query/grid.rs`, `ui_tests.rs`)

- G18, the six "done, untested" rows, now tested; all worked as listed. CON-24
  `ssh::tests::ssh_keepalive_is_passed_to_the_client` (the client config is built by `client_config`;
  0 turns keep-alive off). SQL-03 `the_tab_toolbar_changes_its_connection_database_and_schema`
  (PostgreSQL schema and database, then MySQL and one of its databases; each run checked). SQL-42
  `the_editor_closes_brackets_and_quotes` (the kit pairs on single typed characters, before
  whitespace or a closer, and steps over a typed closer). SQL-46
  `the_editor_adds_a_cursor_above_and_below` (Cmd+Alt+Up / Down). RES-21
  `scrolling_to_the_end_of_a_result_fetches_the_next_page` (the table's list scrolled to its last row;
  no wheel event). RES-40 `a_dragged_column_keeps_its_new_place` (a real header drag, then a sort).
- CM-33: Properties (F4 and a menu entry) on a database and a schema opens a dialog of what the
  catalogs say, read through the dialect (`database_properties_sql`, `schema_properties_sql`) as a
  table's Properties are, drawn with the same lines (`structure::tab::open_properties`). PostgreSQL
  database: owner, encoding, collation, character type, tablespace, connection limit, template,
  connections allowed, size, OID, comment; schema: owner, tables, views, size, OID, comment. MySQL /
  MariaDB database: character set, collation, tables, views, data and index size, and MariaDB's
  comment (MySQL keeps none, neither has an owner). Show DDL stays in the menu. Tests
  `f4_on_a_database_or_a_schema_shows_its_properties`, drivers' `properties_describe_databases_and_schemas`
  / `properties_describe_databases`.
- NAV-48: DBeaver's Ctrl+] (`core.sql.editor.create`, `SQLEditorHandlerOpenEditor`) creates a script
  file in the project's Scripts folder; Ctrl+Alt+Enter in the navigator (`core.sql.editor.console`,
  `SQLEditorHandlerOpenConsole`) opens a console: an editor without a file, bound to the selected
  object's connection and schema. A Kerno query tab has no file until saved, so both are the same
  thing here: Cmd+Alt+Enter is a second key of "New Query on Selected Object" beside Cmd+]. Test
  `cmd_alt_enter_opens_a_console_on_the_selected_node`. The row can close.
- Session question: a table form's draft is not a transaction and is not asked about; it survives a
  disconnect and a reconnect untouched (`a_table_form_draft_survives_a_disconnect_and_reconnect`).
  MySQL case of the read-only toggle and of its question: `read_only_and_its_question_on_mysql`.
- Link with editor on a table an object filter hides: a warning says so and names the filter's
  toggle; the filter is left alone (`link_with_editor_says_when_a_filter_hides_the_table`).
- Palette commands on the selected tree node: Read Data in SQL Console, Generate CALL, Execute
  Selected Routine…, Add Column to Selected Table…, Back Up Selected Object… (Database ▸ Schema
  Tree); Edit column is F4's `edit_object`, the Filter ▸ entries had theirs
  (`tree_entries_have_palette_commands_on_the_selected_node`).
- Lock Manager in a connection's sidebar menu and as a command in the Database menu: Administration
  on its Locks sub-tab (`the_lock_manager_opens_from_the_connection_menu_and_the_palette`).
- After Apply in table data the new page's grid takes the keyboard, unless the user moved to
  something else while it was read (`the_grid_has_the_keyboard_after_apply`; it fails without the fix).
- Go to row… / Go to column… in a cell's Navigate ▸ submenu, after the order, as DBeaver's
  `fillNavigateMenu` (`go_to_row_and_column_are_in_the_navigate_menu_of_a_cell`).
- The transaction log of a new transaction after a lost connection holds only its own statements
  (`a_new_transaction_after_a_lost_connection_starts_an_empty_log`).

Left:
- The database and schema properties are read-only (no comment edit from the dialog, unlike a
  table's); a schema's properties on MySQL are its database's.
- Navigate ▸ holds the two go-to entries only; DBeaver's also lists the row moves and the foreign
  keys, which Kerno keeps at the top level of the menu and on keys.
- The scroll test drives the table's list, not a wheel event.

## G8 — delivered on 2026-11-13, and what it left (one `opus` agent; core: `result.rs`
(`Column::origin`, `ResultStore::renamed`), `editable.rs` (new), `sql/classify.rs`
(`single_table_select`), `sql/edits.rs` (`apply_edits_in_transaction`), `dialect.rs` (`origin_sql`);
drivers: `session.rs`, `dialect.rs`, `tests/edits.rs`; app: `query/edit.rs` (new), `query/grid.rs`,
`query/tab.rs`, `data/tab.rs`, `workspace.rs`, `ui_tests.rs`; `sqlparser` removed from the manifests)

Done (RES-51):
- Origins from the server, not from the SQL: PostgreSQL's row description (table OID, attribute
  number, from the statement's existing describe), MySQL / MariaDB's schema, original table and
  column. One catalog query per result names the table and its kind (`Dialect::origin_sql`), then the
  table's structure; run off the UI thread on the connection's own session when it is in the result's
  database, else on a session opened and closed for it; never on the tab's.
- Editable when the statement is a plain SELECT (`single_table_select`: no JOIN or comma in FROM, no
  DISTINCT, GROUP BY / HAVING, set operation, WITH, subquery or function as the FROM item, known
  set-returning function in the select list; DBeaver's `SQLQuery.isPotentiallySingleSourceSelect`),
  the columns from a table are one table's, that table is a table (not a view), and the result holds
  its key as `DataTab` decides it (`row_identity`). Aliases are written under the table's names; an
  expression, a column shown twice and a generated column are read-only cells.
- Read-only results say why in the footer, one sentence (`editable::ReadOnly`): read-only
  connection, not a single SELECT, joins tables / uses DISTINCT / groups rows / combines several
  results / uses WITH / reads from a subquery or a function / repeats rows with a set-returning
  function, no column comes from a table, columns of several tables, a view or materialized view,
  no primary or unique key, the key column `x` of `s.t` is not in the result, the table not found or
  not read. Generate SQL ▸ is disabled with the short form; enabled on editable results.
- The edit layer is the data tab's, shared (`query/edit.rs`, and `Grid::pending_sql`,
  `rows_sql`, `generated_columns`, `EditTarget`): typing, Delete / Set NULL / default, paste, add /
  duplicate / delete / revert rows, copy from row, undo, Esc, the value panel and record view, the
  row commands of the workspace, Review SQL and `sql_confirm` (production asks twice).
- Apply: in auto-commit, one transaction of its own (`apply_edits`); in manual commit or with a
  transaction open, inside it under a savepoint, nothing committed, the statements in the transaction
  log; a failed apply takes back only what it did and keeps the pending changes. Then the result is
  read again with its filter and order. While a result is paged in an open transaction, Apply asks to
  fetch all rows first (stopping the paged statement would cancel it, which ends a PostgreSQL
  transaction). Changes are applied only on the connection and database their rows were read on: a
  tab switched elsewhere refuses until it is switched back (or the changes discarded).
- Filtered and ordered results stay editable: MariaDB names the derived table, so the origins of the
  statement's first run are kept in `Requery`.
- Tests: `editable::tests::*`, `classify::tests::single_table_selects_are_told_from_the_rest`,
  `grid::tests::a_result_of_one_table_edits_as_table_data_and_a_read_only_one_says_why`, drivers'
  `results_say_which_table_columns_they_show`, `edits_join_an_open_transaction_without_ending_it`;
  UI `a_select_result_of_one_table_is_edited_reviewed_and_applied` (all three engines),
  `a_result_that_is_no_rows_of_one_table_says_why_it_is_read_only`,
  `a_manual_commit_tab_applies_result_edits_inside_its_transaction`,
  `a_result_filtered_or_ordered_by_the_server_stays_editable`,
  `result_changes_stay_where_their_rows_were_read`.

Differs from DBeaver:
- A table without a key stays read-only (DBeaver offers to use all columns); a join to another table,
  even with only one table's columns, is read-only (DBeaver may edit it as a multi-source result).
- An editable result is ordered by the server on a header click, even when every row is here:
  pending changes are kept by row as fetched.
- While a result holds pending changes, the tab runs nothing else and its results are not closed:
  apply or discard first (DBeaver asks to save).

Left:
- New rows wait until every row is fetched ("Fetch all rows first").
- A tab on another database than the connection's own session opens a session per editable-looking
  result for the lookup (PostgreSQL); a cache per table would save it.
- No FK navigation from result rows (DBeaver's references panel on a result).
- A table created in the tab's own uncommitted transaction is not seen by the lookup session: its
  result says the table was not found.
