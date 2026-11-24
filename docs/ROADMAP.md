# Roadmap

What Kerno does not do in v1, in the order it would be built, and how the driver trait
([ARCHITECTURE.md](ARCHITECTURE.md)) extends to each item. The rows of the DBeaver inventory that were moved
out of v1 follow in the sections below; IDs refer to `docs/DBEAVER-PARITY.md`.

## Beyond v1, in order

1. **Data compare** (TOOL-22): built (K60, D-126; see below). Still to come: another engine pairing and
   SQLite (an order for its storage classes), a filter or a sample on either side, applying the differences
   from the tab with the pending-edit apply instead of a script, saving a comparison, more than 10,000
   differences in one script.
2. **Visual query builder** (TOOL-26). Uses the schema tree, foreign keys and the dialect's quoting and
   `LIMIT/OFFSET`; its output is SQL text in a query tab. No trait change.
3. **SQLite**: built (K29, D-102; see below), still to come for it: backup and restore (`VACUUM INTO`,
   `.dump`); the engine's tools under Tools ▸ (ANALYZE, VACUUM, integrity check); copying tables to or from a
   SQLite connection (J13 needs a type mapping of its own); opening a file from the shell (`kerno shop.db`,
   `sqlite://` URLs); attached databases and `temp`; the 12-step table rebuild behind the alters SQLite cannot
   do in place (a column's type, NULL, default, keys, a column's place); keying a keyless table on `rowid`;
   editing the rows of a query result (column origins, `column_metadata`); loadable extensions; check
   constraints in the structure tabs; a column whose rows mix storage classes shown cell by cell (the result
   model has one kind per column).
4. **Other engines**, one crate each, through the same layer: SQL Server, Oracle, ClickHouse, DuckDB. Each
   brings its own `Dialect` (quoting, `LIMIT` or `TOP`/`FETCH`, `explain`, `parse_plan`), its introspection
   queries, a `TypeKind` mapping and a cancel mechanism. Where a feature has no equivalent, the provided
   `Dialect` defaults refuse and the UI hides the entry (as it does for engines without schemas). If a new
   engine needs a capability the trait cannot express, add a method with a refusing default rather than a
   branch on the engine name in the UI.
5. **Dashboards, scheduled tasks** (TOOL-20, TOOL-30); the mock data generator (TOOL-23) is built (below). Built
   on `execute`; the scheduler needs a background runner and a place in the storage migrations.
6. **Cloud sync** of connections, saved queries and settings. Extends `storage.rs` (a sync log beside the
   migrations) and never carries secrets: those stay on the machine, in Kerno's file or the keychain (D-106). Opt-in, with a visible account.
7. **PostgreSQL debugger** (TOOL-25). PostgreSQL-only; needs the debugger extension on the server and a new
    session capability, so it waits for a use for it.

Built in v1.2 and no longer on this list: **Schema compare** (NAV-57, CM-21, CM-59, TOOL-22; K30, D-103): two
schemas of the same engine family, the script that brings one to the other opened in a query tab, never run;
still to come: another engine pairing (a type mapping, as J13's copy has), a SQL file against a live schema,
whole servers (several schemas at once), privileges and owners, saving a comparison, `ALTER TYPE … ADD VALUE` for
new enum labels, and a column type changed under a view (the view dropped and created around it).
**Data compare** (item 1; TOOL-22; K60, D-126): the rows of two tables of the same engine family matched by a
key, read as two ordered streams merged in bounded memory (1,000,000 rows against 1,000,000 in about 2 s), the
differences in a tab and the script that makes the target like the source opened in a query tab, never run;
what is still to come is under item 1. No trait change: it runs on `Session::execute` and the pending-edit SQL.
**Test data** (item 5; TOOL-23; K72, D-141): "Fill with test data…" on a table of PostgreSQL, MySQL, MariaDB or
SQLite (Generate Test Data… in the palette), rows made from a seed, exact about the types, keys and foreign
keys, written as a script in a query tab or inserted in one transaction. Still to come: a sample from a file or
another table, `CHECK` rules read and kept, values that refer to each other across columns (a city that matches
its country), saving the settings as a task. No trait change.

Built in v1.1 and no longer on this list: **ER diagrams** (NAV-58, OBJ-20, CM-47; J18, D-069), read-only; still to come: editing the schema
from the diagram, layouts saved between runs, a diagram of several schemas or of a multi-selection of the tree (needs NAV-34).
**AI assistant** (TOOL-24; J17, D-068), opt-in and local first; streamed answers came with K51 (D-120); left for later: per-connection opt-out, tool calls.
Their FEATURES rows are J18 and J17.
**SQLite** (item 3; K29, D-102), the first engine beyond v1: a `kerno-sqlite` crate on the bundled `rusqlite`,
a connection that is a file (`ConnectionConfig::file`), `has_schemas` false with one database `main`, cancel as
SQLite's interrupt, `admin_query` `None` for every view, and the capabilities it needed added to the trait with
defaults (`Dialect::begin_sql`, `transactional_ddl`, `has_routines`, `ExecSink::stop_signal`). Its FEATURES row
is K29.

## Windows (the owner's word, 2026-11-24: releases for macOS, Linux and Windows from CI)

Step 1 is done (K64, D-131): the workspace compiles for `x86_64-pc-windows-msvc`, clippy is clean there and
`kerno-core`'s tests pass there. Step 2 is done (K66, D-133): the release carries
`kerno-<version>-windows-x86_64.zip` (`kerno.exe` with its icon and the C runtime inside), built and launched
once by the CI on every push; a picture of that launch is the only look at the Windows app so far: nobody has
used it. Step 3a is done (K67, D-136): the home folder (`%USERPROFILE%`: `~\.ssh`, libpq's
`%APPDATA%\postgresql\pgpass.conf`, no MySQL option file), the update check (`curl.exe` of `System32`) and the
local clients (`pg_dump.exe`, `mysqldump.exe`, … in the installers' folders of Program Files and on `PATH`; a
backup, a restore and a cancel proved on a runner). Step 3b is done (K68, D-137): the whole test suite runs on
the runner against a PostgreSQL 17 and a MySQL 8.4 seeded as the containers (`ci.yml`'s `windows` job), what it
showed is fixed, and the packaged app is driven there by keys for pictures (a connection, a query, a table, the
connection dialog, Settings). A preview until the list below is done. Next, in order:

1. A pass in the real app on Windows, by a person: the secret store (Credential Manager through `keyring`),
   the instance's pipe, the built-in SSH client and its agent pipe, the backup wizards and Settings ▸ Local
   clients; whether a menu bar is wanted there (GPUI draws none outside macOS).
2. A line for Windows in the READMEs' install tables, once a person has used the zip.
3. The password command's presets (`aws.exe`, `op.exe`), a MariaDB client whose `mysqldump.exe` is not a link
   (D-136), and the start-failure page's advice (it says "Set HOME").
4. An installer and the in-app update's install, as Recto and Vigie do on Windows.
5. The system OpenSSH tunnel (a loopback TCP forward and the askpass questions on a named pipe), Pageant.

## The AI assistant, next (agreed with the owner, 2026-11-23)

Today (J17) the assistant answers one question at a time: write SQL from a sentence, explain a query, a plan
or an error. It is off by default and reached from the palette. Agreed as the next steps, in this order; each
starts when the owner names it, and all keep J17's rules (opt-in, the endpoint the user set, schema names only
unless sample rows are ticked, "What is sent", nothing run by itself):

1. **A conversation in a side panel.** Follow-ups refine the answer ("add the date filter", "no, by
   customer"), with the editor's query as context. Done (K51, D-120), answers streamed.
2. **Ask in the editor.** A shortcut opens a field at the cursor; the proposal is shown as a before / after
   diff to accept or refuse. Done (K52, D-121): "AI: Ask Here…" (⌘⇧H), Accept as one undo step, refined in
   place.
3. **Fix an error in one click.** From a failed statement, the fix is proposed as a diff in the editor, with
   no panel in between. Done (K52, D-121): "Fix with AI" under the error and beside its mark.
4. **From a right-click.** On a table or a result: ask about this table, explain these columns, generate test
   data. Done (K53, D-122): the tree, a data or structure tab and a result grid.
5. **A richer context.** Foreign keys, indexes and types sent with the schema, for joins that hold and real
   advice on plans. Done (K50, D-119): nullability, primary keys and indexes added for up to 30 tables.

## Limits found on the way

- **RSA SSH key files with the built-in client** are refused (RUSTSEC-2023-0071, D-013); they work through the
  OpenSSH client (J2, D-056), and agent-held RSA keys work with both.
- **Linux release builds need an arm64 Docker host.** The image of `packaging/linux/Dockerfile` builds arm64
  natively and cross-compiles x86_64 (D-066); on an x86_64 host Docker would emulate the whole build. The
  mirror recipe (native x86_64, cross arm64) waits for a machine that needs it.
- **Quit from the Dock, a logout and SIGTERM do not ask** (K1, 2026-11-17). Cmd+Q, the Quit menu item and
  the window's close button ask before dropping an open transaction or pending changes; the Dock's Quit and a
  logout send `terminate:` straight to AppKit, and the pinned gpui has no `applicationShouldTerminate` to
  answer it. The server rolls the transaction back when the socket closes; pending edits and an unapplied
  table form are lost. Closing it needs that hook in the kit (the owner's fork) or a second `unsafe`.
- **The macOS build is not notarized** (owner, 2026-11-16): a notarized build (an Apple developer account,
  the owner's) would remove the "Open Anyway" step of the first launch. The plain inside of the disk image
  noted here at the same time is gone: it opens on a window of its own (K73, D-142).

- **What old MySQL servers still miss in Kerno** (K22, D-094). On MySQL 5.1 the navigator lists functions and
  procedures without their arguments (they are in `mysql.proc.param_list`, readable only with SELECT on the
  `mysql` database); before 5.6.5 there is no plan tree (Kerno would have to read the classic `EXPLAIN` table, as
  DBeaver's `MySQLPlanNodePlain` does); before MySQL 8.0.3 / MariaDB 10.5.2 a column renamed alone in the
  structure editor is refused (`CHANGE COLUMN` needs the column's definition, which
  `TableChange::RenameColumn` would have to carry); `mysqldump` 8.4 finds no columns in a 5.1 table, so a 5.1
  backup needs a MySQL 5.7 client (5.7.11 or later) chosen in Settings ▸ Local clients, which the backup now
  says before it runs and names (K63, D-135); MySQL 8.0's clients were not tried there. MariaDB
  before 11 uses the same version facts but is not tested.

## From the competitors' issues (triage of 2026-11-14): not now

`docs/COMPETITOR-ISSUES.md` sorts what DBeaver's and Beekeeper Studio's users ask for. The "do" list is
v1.1 (CLAUDE.md §4 J, `docs/plans/M11.md`). What was left out, and why:

- **Parquet export.** A heavy dependency (Arrow and Parquet crates) for little demand in the sample; XLSX
  (J12) answers the larger request first. It would be one more `ExportFormat` on the streamed export path.
- **Wayland and fractional-scaling checks.** They need a real Linux desktop session; the Linux build is
  checked in a container, which has no compositor.
- **Flathub, Snap, an apt repository.** Publishing is the owner's step (CLAUDE.md §8). The packages of J16
  (tarballs, `.deb`, AppImage for x86_64 and arm64, D-066) are built and checked; a repository would carry
  them (an apt repository also needs a signing key and a `Maintainer` e-mail in `packaging/linux/control.in`).
- **PostGIS geometry viewer, role and sequence editors, row-level security in the navigator.** Real but
  smaller demand; the next round. Each is a view over catalog queries the drivers already have the shape
  for (`admin_query`, `table_structure`).
- **Schema compare and data compare** (items 1 and 2 above), **notebooks, PRQL, an HTTP proxy for
  connections, Emacs keys, real bind parameters.** Low demand in the sample.
- **Other engines** (MongoDB, Oracle, SQL Server, ClickHouse…). The biggest block of demand and out
  of v1's scope: item 5 above, through the driver layer (SQLite, item 4, is built: K29).

## From the general critique of DBeaver (triage of 2026-11-17): not now

`docs/COMPETITOR-ISSUES.md` (section of 2026-11-17) checks C1-C22 against the code; `docs/plans/M12.md` plans the
gaps, K1-K10 for the next milestone and K11-K16 after it. What is left out of both, and why:

- **Linux look (C7).** No capture of the Linux packages exists: they were start-checked in containers without
  a display. Look at them on a real GNOME and KDE session (Wayland and X11, fractional scaling) with the
  Wayland check above.
- **Smart rename in SQL (C11).** Renaming an alias or a CTE across a statement, from J7's occurrences. Low
  demand in the sample; the "also dropped" list before a cascade came first because it protects data (done in
  K14, D-086, with a text search of what names an object before a rename).
- **AWS RDS IAM and 1Password against the real services (C18).** Tested with fakes; a real run needs the
  owner's accounts (CLAUDE.md §8).
- **Idle memory margin (C1).** 149.9 MB for a 150 MB bar; the fix left is a second `unsafe` call, with the
  owner (D-042).

## From the DBeaver inventory

Rows of `docs/DBEAVER-PARITY.md` marked **roadmap**, grouped by reason (IDs refer to that file). The 67 gaps the inventory
proposed were accepted here on 2026-11-13; their reasons are in the last section below and in
`docs/plans/PARITY-GAPS.md`.

### Outside v1 (CLAUDE.md §4, last paragraph)

The order and the driver-trait extension of each are in "Beyond v1, in order" above.

| ID | Feature | PG / MySQL | Reason |
| -- | ---- | ---- | ---- |
| NAV-57 | Compare / Migrate | PG, My | Schema compare is outside v1 (CLAUDE.md §4, last paragraph) |
| NAV-58 | View Diagram | PG, My | Done in v1.1: "View diagram" opens the ER diagram tab (J18, D-069) |
| OBJ-20 | ER diagram page | PG, My | Done in v1.1: "View diagram" opens the ER diagram tab (J18, D-069) |
| TOOL-20 | Dashboards | PG, My | Outside v1 (CLAUDE.md §4, last paragraph) |
| TOOL-30 | Task scheduler | PG, My | Saved tasks are done (TOOL-21); DBeaver Community has no scheduler either, and one needs a background runner while Kerno is closed |
| TOOL-31 | Maintenance tools and data transfers as tasks | PG, My | The four client-tool wizards save tasks; the maintenance dialog and the export wizard have no serialisable settings yet (M10 Part 11b) |
| TOOL-22 | Data compare, schema compare | PG, My | Done in v1.2: schema compare (K30, D-103), data compare (K60, D-126) |
| TOOL-23 | Mock data generator | PG, My | Done in v1.2: "Fill with test data…" (K72, D-141), SQLite too |
| TOOL-24 | AI assistant | PG, My | Done in v1.1 (J17, D-068): Settings → AI, editor menu ▸ AI: Write SQL / Explain query, plan, error; opt-in, to the endpoint the user configures only |
| TOOL-25 | Debugger | PG | Outside v1 (CLAUDE.md §4, last paragraph) |
| TOOL-26 | Visual query builder | PG, My | Outside v1 (CLAUDE.md §4, last paragraph) |
| CM-21 | Compare / Migrate ▸ | PG, My | Outside v1 (CLAUDE.md §4, last paragraph) |
| CM-47 | View Diagram | PG, My | Done in v1.1: "View diagram" opens the ER diagram tab (J18, D-069) |
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
| TOOL-19 | Data transfer beyond files | PG, My | Database to database is done in v1.1 (J13, D-063: Tools ▸ Copy tables to another connection…); the other data-transfer wizards stay out |

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
| CON-26 | SSH: jump servers | PG, My | Done in v1.1 (J2, D-056): the Jump host field and `ProxyJump` through the system OpenSSH client |
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
| SQL-20 | Multiple results per tab | PG, My | Done in v1.1 (J11): Settings ▸ Grid stacks a script's results in one panel; one tab per statement stays the default |
| SQL-22 | Toggle editor layout | PG, My | Layout preference |
| SQL-26 | Script variables and commands (`@set`, `@include`, `@echo`, `@export`, variables panel) | PG, My | DBeaver-specific script language; not standard SQL |
| SQL-27 | Outline | PG, My | Nice-to-have for long scripts |
| SQL-36 | Format ▸ Morph to delimited list, trim spaces | PG, My | Text utilities, not SQL client features |
| SQL-40 | Templates | PG, My | Saved queries cover reusable SQL |
| SQL-43 | Go to / select to matching bracket | PG, My | Editor-kit feature; low use |
| SQL-44 | Foldings from the keyboard (Fold, Fold all), and folds on indentation or marker comments (DBeaver #17240, #23137) | PG, My | Folding from the gutter works (a statement, a subquery, a `BEGIN … END` body; tested, J7) and Unfold All is a command; gpui-kit 0.9.1 has no call to fold a line, only its gutter (D-057) |
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
| OBJ-07 | Column order | My | Done in M11 (J8, D-062) on MySQL and MariaDB; PostgreSQL cannot |
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
