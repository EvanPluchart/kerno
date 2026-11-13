# Roadmap

What Kerno does not do in v1, in the order it would be built, and how the driver trait
([ARCHITECTURE.md](ARCHITECTURE.md)) extends to each item. The rows of the DBeaver inventory that were moved
out of v1 follow in the sections below; IDs refer to `docs/DBEAVER-PARITY.md`.

## Beyond v1, in order

1. **ERD diagrams** (NAV-58, OBJ-20, CM-47). Built from `Session::table_structure`, which already returns
   columns, keys and foreign keys in both directions. No trait change: a layout and a canvas view in the app,
   fed by the structure of the tables of a schema.
2. **Schema compare** (NAV-57, CM-21, CM-59, TOOL-22). Reads `table_structure` and `ddl` from two sessions,
   diffs the engine-neutral `TableStructure` values in `kerno-core`, and writes the migration with the
   dialect's `alter_table_sql` and `create_table_sql`. No new trait method; the diff is core code with unit
   tests.
3. **Data compare** (TOOL-22). Runs one ordered `SELECT` per side through `Session::execute`, keyed by the
   row identity the table view already computes, and merges the two streams. Applying differences reuses the
   pending-edit SQL and its transactional apply. No trait change.
4. **Visual query builder** (TOOL-26). Uses the schema tree, foreign keys and the dialect's quoting and
   `LIMIT/OFFSET`; its output is SQL text in a query tab. No trait change.
5. **SQLite** as the first engine beyond v1 (it needs no server). A new `Engine` variant and a
   `kerno-sqlite` crate following the walkthrough in ARCHITECTURE.md. It stresses the trait where PostgreSQL
   and MySQL do not: there is no host, port, user or tunnel (`ConnectParams` and the connection form gain a
   file-path variant), `has_schemas` is false, cancel is an interrupt of the running statement, and
   `admin_query` returns `None` for the views it has no equivalent of.
6. **Other engines**, one crate each, through the same layer: SQL Server, Oracle, ClickHouse, DuckDB. Each
   brings its own `Dialect` (quoting, `LIMIT` or `TOP`/`FETCH`, `explain`, `parse_plan`), its introspection
   queries, a `TypeKind` mapping and a cancel mechanism. Where a feature has no equivalent, the provided
   `Dialect` defaults refuse and the UI hides the entry (as it does for engines without schemas). If a new
   engine needs a capability the trait cannot express, add a method with a refusing default rather than a
   branch on the engine name in the UI.
7. **Mock data generator, dashboards, scheduled tasks** (TOOL-20, TOOL-30, TOOL-23). Built on `execute` and
   the dialect's `literal`; the scheduler needs a background runner and a place in the storage migrations.
8. **AI assistant** (TOOL-24). Not before the owner decides how to square it with "nothing leaves the
   machine": it would be opt-in, per connection, and either local or an explicit endpoint the user
   configures. It reads `completion_catalog` and the structure for context, and anything it proposes runs
   through the same guards (read-only, confirmation, production) as typed SQL. No trait change.
9. **Cloud sync** of connections, saved queries and settings. Extends `storage.rs` (a sync log beside the
   migrations) and never carries secrets: those stay in the OS secret store. Opt-in, with a visible account.
10. **PostgreSQL debugger** (TOOL-25). PostgreSQL-only; needs the debugger extension on the server and a new
    session capability, so it waits for a use for it.

## Limits found on the way

- **AppImage for Linux.** `scripts/bundle-linux.sh` produces a tarball (binary, `.desktop` file, icons,
  licences). An AppImage needs `appimagetool` and a desktop-integration step that the build image does not
  carry; it comes with the first public release.
- **amd64 Linux build from an Apple Silicon Mac.** The container build runs on the host's architecture
  (arm64 here); amd64 needs an amd64 Linux host (there is no hosted CI). Cross-building locally would run under
  emulation.
- **RSA SSH key files** are refused (RUSTSEC-2023-0071, D-013); agent-held RSA keys work.

## From the DBeaver inventory

Rows of `docs/DBEAVER-PARITY.md` marked **roadmap**, grouped by reason (IDs refer to that file). The 67 gaps the inventory
proposed were accepted here on 2026-11-13; their reasons are in the last section below and in
`docs/plans/PARITY-GAPS.md`.

### Outside v1 (CLAUDE.md §4, last paragraph)

The order and the driver-trait extension of each are in "Beyond v1, in order" above.

| ID | Feature | PG / MySQL | Reason |
| -- | ---- | ---- | ---- |
| NAV-57 | Compare / Migrate | PG, My | Schema compare is outside v1 (CLAUDE.md §4, last paragraph) |
| NAV-58 | View Diagram | PG, My | ERD is outside v1 (CLAUDE.md §4, last paragraph) |
| OBJ-20 | ER diagram page | PG, My | ERD is outside v1 (CLAUDE.md §4, last paragraph) |
| TOOL-20 | Dashboards | PG, My | Outside v1 (CLAUDE.md §4, last paragraph) |
| TOOL-30 | Task scheduler | PG, My | Saved tasks are done (TOOL-21); DBeaver Community has no scheduler either, and one needs a background runner while Kerno is closed |
| TOOL-31 | Maintenance tools and data transfers as tasks | PG, My | The four client-tool wizards save tasks; the maintenance dialog and the export wizard have no serialisable settings yet (M10 Part 11b) |
| TOOL-22 | Data compare, schema compare | PG, My | Outside v1 (CLAUDE.md §4, last paragraph) |
| TOOL-23 | Mock data generator | PG, My | Outside v1 (CLAUDE.md §4, last paragraph) |
| TOOL-24 | AI assistant | PG, My | Outside v1 (CLAUDE.md §4, last paragraph) |
| TOOL-25 | Debugger | PG | Outside v1 (CLAUDE.md §4, last paragraph) |
| TOOL-26 | Visual query builder | PG, My | Outside v1 (CLAUDE.md §4, last paragraph) |
| CM-21 | Compare / Migrate ▸ | PG, My | Outside v1 (CLAUDE.md §4, last paragraph) |
| CM-47 | View Diagram | PG, My | Outside v1 (CLAUDE.md §4, last paragraph) |
| CM-59 | Compare / Migrate ▸ | PG, My | Outside v1 (CLAUDE.md §4, last paragraph) |

### Eclipse workbench, projects and JDBC plumbing

Kerno is not an Eclipse application and ships native drivers: these have no counterpart to build.

| ID | Feature | PG / MySQL | Reason |
| -- | ---- | ---- | ---- |
| CON-50 | Driver manager, change connection driver | PG, My | Kerno has native drivers, no JDBC (Eclipse / JDBC plumbing) |
| CON-51 | Connection permissions, lock password, project sync | PG, My | Eclipse project / team features |
| NAV-38 | Bookmarks | PG, My | Workspace / project feature (Eclipse resources) |
| NAV-39 | Browse from here | PG, My | Eclipse view plumbing |
| NAV-40 | Projects, active project, resources, scripts folder | PG, My | Eclipse workspace / project resources (left out by the brief) |
| SQL-30 | Delete this script, revert file | PG, My | Scripts as workspace files are Eclipse project resources; Kerno keeps tabs, files and saved queries |
| TOOL-28 | Git / team | PG, My | Eclipse team plumbing |
| CM-02 | Driver Manager | PG, My | No JDBC drivers (CON-50) |
| CM-03 | Active Project ▸ | PG, My | Eclipse projects (NAV-40) |

### Data transfer beyond CLAUDE.md §4.G

v1 exports CSV, TSV, JSON, NDJSON, SQL INSERT and Markdown, and imports CSV.

| ID | Feature | PG / MySQL | Reason |
| -- | ---- | ---- | ---- |
| RES-44 | Copy as ▸ HTML, XML, TXT, source code, DbUnit | PG, My | Data-transfer formats beyond CLAUDE.md §4.D3 / §4.G |
| RES-68 | Open with | PG, My | Writes a temp file and launches another app; data-transfer feature beyond §4.G |
| TOOL-19 | Data transfer beyond files | PG, My | Data-transfer wizards beyond CLAUDE.md §4.G (left out by the brief) |

### Other scope decisions

Read from CLAUDE.md (§1 "Nothing leaves the machine", the §4.B1 tree list, §4.H2 "read view") or PostgreSQL-only specialised features; the main session may revisit them.

| ID | Feature | PG / MySQL | Reason |
| -- | ---- | ---- | ---- |
| NAV-10 | Aggregates, event triggers, foreign data wrappers, publications, subscriptions, tablespaces | PG | Not in CLAUDE.md §4.B1 tree list; specialised features. Foreign data wrappers, servers and user mappings are listed in Tools ▸ Foreign data wrappers… (M10 Part 11b): showing them in the tree needs new node kinds and driver introspection; DBeaver's foreign tables built one by one from another connection's tables (`PostgreFDWConfigWizard`) need its type mapping between engines, and Kerno imports a whole schema instead |
| SQL-50 | Search in web | PG, My | Sends text off the machine (CLAUDE.md §1 "Nothing leaves the machine") |
| OBJ-17 | Role / user editors | PG, My | CLAUDE.md §4.H2 asks for a read view of users and grants; editing them is beyond v1 |
| TOOL-18 | Refresh publications (subscriptions) | PG | Replication objects are outside CLAUDE.md §4 |

### Accepted on 2026-11-13 (rarely used, off by default in DBeaver, covered another way in Kerno, or outside CLAUDE.md §4)

The owner can pull any row back by naming its ID.

#### Connections

| ID | Feature | PG / MySQL | Reason |
| -- | ---- | ---- | ---- |
| CON-09 | Edit connection types | PG, My | Kerno has three fixed environments plus free colour tags (CLAUDE.md §4.A1); custom types add little Update: done since (D-044), environments are the user's: Settings ▸ Environments. |
| CON-14 | Default isolation level | PG, My | Rarely changed; a `SET TRANSACTION` in bootstrap queries (G13) covers it Update: the toolbar's transaction mode sets the isolation level of a session (D-050); a default applied at connect is still not offered. |
| CON-18 | Keep-alive (database) | PG, My | Off by default in DBeaver; Kerno re-establishes a dropped connection (`a_lost_connection_is_re_established`) |
| CON-19 | Close idle connection after N seconds | PG, My | Resource saving, not a daily need; needs an idle timer per connection |
| CON-20 | Auto-close idle transactions | PG, My | Safety net for manual mode; Kerno already asks before closing a tab with an open transaction (M5 Part 4) Update: done since (D-044), an environment's "close idle transactions" rule. |
| CON-26 | SSH: jump servers | PG, My | Beyond CLAUDE.md §4.A4 (one tunnel); bastion chains are uncommon |
| CON-27 | SSH: local / remote host and port overrides, connect timeout | PG, My | Automatic local port is enough for a client; rare settings |
| CON-28 | SSH: share tunnel between connections | PG, My | Optimisation only |
| CON-29 | SOCKS proxy and network profiles | PG, My | Not in CLAUDE.md §4.A; enterprise networks only |
| CON-42 | Smart commit mode | PG, My | Off by default in DBeaver; manual mode per tab covers the need |
| CON-46 | Pending transactions | PG, My | Kerno shows the open transaction per tab in the status bar; a cross-app list is rarely needed |
| CON-48 | Change user password | PG, My | An `ALTER USER` in the editor does it; rare |
| CON-49 | Shell commands on connection events | PG, My | Runs local programs from a connection; outside CLAUDE.md §4.A and a security surface |

#### Navigator

| ID | Feature | PG / MySQL | Reason |
| -- | ---- | ---- | ---- |
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

#### SQL editor

| ID | Feature | PG / MySQL | Reason |
| -- | ---- | ---- | ---- |
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
| SQL-53 | Disable SQL syntax parser | PG, My | Kerno splits and reads only what a run needs on its own tolerant lexer (`a_long_script_splits_in_linear_time`), so there is nothing to turn off |

#### Results and data editing

| ID | Feature | PG / MySQL | Reason |
| -- | ---- | ---- | ---- |
| RES-03 | Text presentation, switch presentation | PG, My | Copy as Markdown gives a text table; second renderer to maintain |
| RES-07 | Filter autocompletion | PG, My | The builder lists the columns |
| RES-13 | Save / reset default filter | PG, My | Saved queries hold a filtered SELECT |
| RES-14 | Customize filters dialog (Alt+Shift+F) | PG, My | The filter bar and the header menus cover it |
| RES-29 | Panels: grouping | PG, My | A GROUP BY in the editor does it |
| RES-32 | View / Format ▸ display format, transformers, binary format | PG, My | Kerno shows server text and hex for binary (CLAUDE.md §4.D2); formats are a preference set |
| RES-33 | Row colours by value, colorize data types | PG, My | Visual nicety |
| RES-39 | Hide columns with no data | PG, My | Rare |
| RES-42 | Advanced Copy (Ctrl+Shift+C) | PG, My | Copy with column names and Copy as CSV / TSV cover the usual options; the shortcut is in G2 |
| RES-48 | Advanced Paste (Ctrl+Shift+V) | PG, My | Paste into cells is done (`pasting_fills_cells_from_the_clipboard_and_delete_empties_them`); inserting new rows from a spreadsheet is less common |
| RES-54 | Add / duplicate row before | PG, My | Row position in an unsorted table is not meaningful to the server |
| RES-71 | Virtual keys and logical structure | PG, My | Keyless tables stay read-only with the reason (CLAUDE.md §4.E2) |
| RES-72 | Generate UUID | PG, My | `gen_random_uuid()` / `uuid()` as a default covers it |

#### Object editors (structure)

| ID | Feature | PG / MySQL | Reason |
| -- | ---- | ---- | ---- |
| OBJ-07 | Column order | My | Noted as not editable in M7; MySQL-only |
| OBJ-09 | Edit trigger source | PG, My | PostgreSQL trigger bodies are functions (editable); MySQL triggers are re-created from Show DDL |
| OBJ-11 | Sequence editor | PG | Show DDL gives the definition; current value is a query away |
| OBJ-18 | Dependencies page | PG | See NAV-09 |

#### Tools and administration

| ID | Feature | PG / MySQL | Reason |
| -- | ---- | ---- | ---- |
| TOOL-03 | Global backup (pg_dumpall) | PG | Server-wide dumps are a DBA task outside a client's daily use Update: done since (D-046), the global backup wizard. |
| TOOL-04 | Execute script with the local client | PG, My | Restore covers SQL files (M8 Part 3) Update: done since (D-046), the execute-script wizard. |
| TOOL-15 | Session details: query text and plan | PG, My | M8 Part 4 shows the query column; the plan of another session is rarely needed |
| TOOL-17 | MySQL status, engines, charsets, collations, plugins | My | Available through SHOW statements in the editor |
| TOOL-27 | SQL terminal presentation | PG, My | Alternative presentation of results; outside CLAUDE.md §4 |

#### Context menus

| ID | Feature | PG / MySQL | Reason |
| -- | ---- | ---- | ---- |
| CM-17 | Create ▸ (database, schema) | PG, My | Create database / schema exist on the navigator's database and schema nodes (`dropping_renaming_and_truncating_show_the_sql_and_ask`); the sidebar connection menu need not repeat them |
| CM-19 | Connection view ▸ | PG, My | See NAV-59 |
| CM-31 | Security ▸ Change user password | PG, My | See CON-48 |
| CM-51 | Generate SQL ▸ MERGE, INSERT ON CONFLICT, UPDATE FROM, DELETE USING | PG, My | See NAV-51, NAV-52 |
| CM-74 | New Index / Constraint from selection | PG, My | See NAV-43 |
| CM-112 | View / Format ▸, Hints, Logical structure | PG, My | See RES-32, RES-33, RES-71 |
