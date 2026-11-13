# Kerno — master prompt

Kerno is a fast, native, open-source database client for macOS and Linux, written in Rust with GPUI. It aims
for the breadth of DBeaver and the look and ease of Beekeeper Studio, with the speed of a native app. v1
targets PostgreSQL and MySQL/MariaDB; every other engine comes later through the same driver layer. Owner:
Evan (open-source and portfolio piece). This file is the master spec and overrides your defaults. Code,
comments, commits, docs and UI text in English.

The bar: the database client people switch to because it opens instantly, never freezes, and feels good to
use all day. Not a toy demo with a grid. Every v1 feature listed in §4 works end to end against real
servers.

## 1. Hard constraints (never break)

### Safety: other people's data

- Connect only to databases you created yourself for tests (the `kerno-*` Docker containers of §6). Never
  connect to, scan or read a database that already exists on this machine or the network.
- Passwords, SSH passphrases and private-key passphrases live in the OS secret store (`keyring` crate: macOS
  Keychain, Linux Secret Service). Never in plain text on disk, never in logs, never in crash output. If no
  secret store is available, ask the user for the password at connect time and keep it in memory only.
- Every destructive action (DROP, TRUNCATE, DELETE without WHERE, applying pending edits, ALTER) shows the SQL
  it will run and asks for confirmation. Connections can be flagged `read_only` (block writes in the client
  and open the session read-only on the server) and tagged with an environment colour (`production` asks
  for a second confirmation on any write).
- Editing data happens in a transaction: pending edits are shown, previewed as SQL, applied atomically, or
  discarded. A failed apply rolls back and keeps the pending edits.

### Performance is a feature

- The UI thread never waits on I/O. All database, file, SSH and secret-store work runs off the main thread;
  results stream back to the UI.
- Result sets stream and render virtually: only visible rows and columns are laid out. Memory grows with what
  was fetched, not with what the query could return; large results fetch in pages (default 1,000 rows, "fetch
  all" on demand with a visible row count and a cancel button).
- Every running query can be cancelled (PostgreSQL cancel request, MySQL `KILL QUERY` on a side connection).
- Measured targets are in §5. Do not ship a feature that breaks them.

### Open source, clean IP

- Licence: MIT OR Apache-2.0 (Rust convention), `LICENSE-MIT` and `LICENSE-APACHE` at the root, SPDX headers
  not needed. Check every dependency licence is compatible (`cargo deny` with a `deny.toml`).
- Study the open-source code of both references to see how each feature really works (queries used for
  introspection, edge cases per engine, SQL generation, type handling, UX flows), then implement it in
  idiomatic Rust. Clone them shallow into `~/VM/refs/` (outside this repo, never committed, never a
  submodule) and grep there; or use `gh api` for single files.
  - DBeaver Community (`github.com/dbeaver/dbeaver`, Java, Apache-2.0): porting its logic to Rust is allowed.
    When a Kerno file ports non-trivial DBeaver logic, say so in a comment naming the source file, and list
    DBeaver in `THIRD_PARTY_NOTICES.md` with its Apache-2.0 notice.
  - Beekeeper Studio (`github.com/beekeeper-studio/beekeeper-studio`, TypeScript/Electron, GPL-3.0): read it
    to understand behaviour and UX, then write Kerno's code from that understanding. Never translate its code
    line by line into Rust: a translation is a derivative work and would force Kerno under GPL-3.0. Never
    open `src-commercial/` (proprietary licence).
  - SQL queries against system catalogs (`pg_catalog`, `information_schema`) are facts about the engine:
    write them from the engine docs, using the references to check you missed nothing.
- No icons, screenshots, names, colours or wording copied from either.
- Engine logos (owner request): show each engine's real logo wherever an engine is named — the engine picker
  of the new-connection dialog, saved connections in the sidebar, the navigator root, tab and status-bar
  connection badges. This is nominative use (identifying the engine Kerno connects to), the way every
  database client does it. Rules: unmodified official artwork as SVG, taken from the vendor's press or brand
  page or from Simple Icons; one file per engine in `assets/engines/`; source URL and the vendor's trademark
  notice in `THIRD_PARTY_NOTICES.md`; follow each vendor's usage guidelines (no recolouring beyond what
  they allow, clear space, no distortion). Never use an engine logo as part of Kerno's own branding. Keep a
  monochrome variant only where the vendor provides one, for small sizes and dark backgrounds.
- Nothing leaves the machine: no telemetry, no analytics, no update pings, no crash upload.

### Machine hygiene (the owner's Mac froze once — strict)

- Every long command through `nice -n 10`: `cargo build`, `cargo test`, `cargo clippy`, the app itself during
  checks, Docker builds. `.cargo/config.toml` caps `jobs = 3`; keep it. Tests with `--test-threads=3` at most.
- Before any heavy step run `uptime`; if the load average exceeds `sysctl -n hw.ncpu`, wait and retry later.
- One running instance of Kerno at a time during checks; quit it after each series of screenshots and before
  you finish. One automated browser or GUI driver at a time. At most 2 heavy subagents at once.
- Docker containers are named `kerno-*`, use non-standard host ports, and are stopped
  (`docker compose down`) at the end of each work session and before you finish. Never touch other containers.
- No background process left running when you stop: no app, no watcher, no container.

## 2. Stack

Check current versions before pinning (context7, crates.io, the GitHub repos). GPUI's API moves fast: read the
source of the exact version you pin (in `~/.cargo/registry` or `~/.cargo/git`) instead of trusting memory.

- Rust stable. `rustup update stable` is allowed if the pinned GPUI needs a newer compiler.
- UI: `gpui` (Zed's framework) plus `gpui-component` (longbridge) for the virtualized table, tree, inputs,
  code editor, dock, tabs, menus, dialogs, notifications and theming. Use its components before writing your
  own; write your own only where it falls short, and record why in `docs/DECISIONS.md`. Pin both to versions
  that work together (crates.io or an exact git rev).
- macOS shaders: GPUI compiles Metal shaders at build time and needs the Metal toolchain (`xcrun metal`). If it
  is missing, build with GPUI's `runtime_shaders` feature and note it in `docs/DECISIONS.md`; the owner was
  asked to install the toolchain (`xcodebuild -downloadComponent MetalToolchain`).
- Async: GPUI has its own executors; database drivers need Tokio. Run one multi-threaded Tokio runtime
  (≤ 3 worker threads) and bridge results to GPUI entities through channels. One place owns this bridge.
- PostgreSQL: `tokio-postgres` (cancel tokens, notices, COPY, simple and extended protocol) with
  `tokio-postgres-rustls` or `postgres-native-tls`. MySQL/MariaDB: `mysql_async`. You may choose `sqlx`
  instead if you show it covers cancel, streaming and dynamic typing as well; write the choice down.
- SSH tunnels: `russh` (password, key file with passphrase, SSH agent), local port forward per connection.
- Secrets: `keyring`. App storage (connections without secrets, history, saved queries, open tabs,
  settings): SQLite via `rusqlite` (bundled) in the platform data dir (`directories` crate). Migrations
  versioned in code.
- SQL tooling: `sqlparser` for statement splitting, statement-at-cursor and completion context; tree-sitter
  SQL grammar for highlighting if `gpui-component`'s editor uses tree-sitter.
- Errors: `thiserror` in libraries, `anyhow` at the app edge. Logs: `tracing` to a rotating file in the data
  dir, secrets redacted.

### Workspace layout (keep it this small)

- `crates/kerno` — the app (binary): windows, views, actions, keymap, theme.
- `crates/kerno-core` — driver trait, value and type model, connection config, SSH tunnel, secrets, app
  storage, SQL helpers. No GPUI dependency, fully unit-tested.
- `crates/kerno-postgres`, `crates/kerno-mysql` — the two drivers.

The driver trait is the one abstraction v1 needs: two implementations now, more engines later. It covers
connect, execute streaming results with column metadata, cancel, transactions, introspection (databases,
schemas, tables, views, materialized views, columns, indexes, constraints, foreign keys, triggers, functions,
procedures, sequences, enums/types, extensions, users/roles, sessions), DDL generation, and dialect details
(quoting, LIMIT/OFFSET, EXPLAIN syntax). No other speculative layers.

## 3. Design direction

- Calm, modern, dense where it matters. A left sidebar with saved connections (grouped, colour-tagged) and the
  schema tree of the active connection; a central area of tabs (query, table data, structure, admin); a
  bottom results panel; a status bar (connection, database, transaction state, row count, timing).
- Dark and light themes plus "follow system", designed as tokens in one theme file (surface levels, text
  levels, accent, success/warning/danger, connection tag colours, grid lines, selection, NULL styling).
  Contrast at least WCAG AA for text.
- One typeface for UI, one monospace for SQL and data, both embedded and licensed for redistribution (for
  example Inter and JetBrains Mono, OFL). Tabular figures in the grid.
- Every view has real empty, loading and error states that say what happened and what to do next. Errors
  from the server show the message, the SQL position (highlighted in the editor) and the hint/detail fields.
- DBeaver Community is the primary reference for features and interactions (owner request): when in doubt
  about what an action, menu or default should be, do what DBeaver does, with Kerno's look.
- Context menus everywhere (owner request), modelled on DBeaver's: every surface answers a right-click with
  the actions that make sense there. Sidebar empty area: New connection, New folder, Import connections,
  Refresh, Collapse all. Folder: New connection here, New subfolder, Rename, Delete. Connection: Connect,
  Disconnect, Reconnect, New SQL editor, Edit, Duplicate, Rename, Move to folder, Delete, Refresh, Copy.
  Navigator objects: as §4 B. Tabs bar: Close, Close others, Close to the right, Close all, Rename. Editor:
  Run statement, Run script, Explain, Format, Cut/Copy/Paste, Comment, Save. Results grid: Copy, Copy as…,
  Paste, Set NULL, Edit cell, Filter by this value, Exclude this value, Order by column, Hide column, Export.
  Status bar items: their own actions. Drag and drop connections between folders.
- Keyboard first: every action reachable from the command palette (Cmd/Ctrl+K), sensible default
  shortcuts (Cmd+Enter run statement, Cmd+Shift+Enter run all, Cmd+. cancel, Cmd+T new query, Cmd+W close tab,
  Cmd+S save, Cmd+P quick open table), a keymap file the user can override, native menu bar on macOS.
- Motion is short and functional (tab/panel transitions under 150 ms), never blocks input.
- Brand (owner-approved, 2026-11-13): identity, icons and colours are in `assets/brand/BRAND.md`; use those
  files, do not redraw them. The theme accent moves from teal to the brand: dark theme primary = Lime
  `#C8F03C` with Ink text, selection tints from lime; light theme primary buttons = Lime with Ink text,
  while text accents, links and focus rings use Olive `#4D6B00` and selected rows use Lime wash `#F1F8D6`.
  Keep WCAG AA (BRAND.md lists the ratios). Show the horizontal lockup in the About window and the README
  header, and the mark in the empty state of the main window.
- Owner's test of 2026-11-13 (afternoon), all required before the final screenshots:
  1. No vertical bars at the left of connection rows or tabs: they read as generated filler. A connection
     with no colour shows no marker; a colour tag is a small dot beside the engine logo (sidebar, tabs, status
     bar). Production keeps a distinct warning (its tinted tab or status bar), not a bar.
  2. One tree, as DBeaver's Database Navigator: folders and connections are the roots, expanding a connection
     connects it and shows its databases, schemas and objects under it. No separate "not connected" panel
     taking half the sidebar. Every empty area of the sidebar answers a right-click.
  3. Folders have a colour (the connection palette) and an icon: a few built-in glyphs or a custom image the
     user picks (PNG, JPEG or SVG, copied into the app data directory, drawn at 16 px), e.g. a project's logo.
     Set them in the New folder dialog and in the folder's "Edit folder…" menu entry.
  4. The app icon also shows when the bare binary runs (Dock and app switcher), not only inside `Kerno.app`.
  5. Connection errors say what happened in plain words first (host not found, connection refused, timed
     out, wrong password, TLS failure) with the next step, then the server's raw message below. Example: host
     not found → "Host “studio” was not found. Check the host name; for a server on this computer, use
     localhost."
  6. Environments are the user's, as DBeaver's connection types (`DBPConnectionType`, Apache-2.0, port with
     attribution): Settings → Environments lists them; the three built-ins (local, staging, production) can be
     edited but not deleted; the user adds their own. Each has a name, a colour (light and dark), and its
     rules: auto-commit by default, confirm every SQL execution, confirm data edits, read-only by default,
     close idle transactions. Existing connections keep their environment by id.
  7. A non-local environment is visible everywhere while it is active, as in DBeaver: the window's chrome for
     that connection (its tabs, the editor and grid frame, the status bar) takes the environment's colour,
     strong for production, softer for staging, none for local. Readable in both themes (WCAG AA).
  8. A connection with no default database opens on the database the server gives (often `postgres`, which
     is empty) and the owner thought their data was gone. The Database field lists the server's databases once
     the fields allow a test (a dropdown, still free text); the navigator shows the default database first and
     marked, and the others under it unexpanded.
  9. The macOS keychain asked the owner's login password on every connect. Read each secret at most once per
     app run (keep it in memory for the session, never on disk); `scripts/bundle-macos.sh` signs with a stable
     identity when one is configured (`KERNO_SIGN_IDENTITY`), ad hoc otherwise, and the README says that ad-hoc
     builds make the keychain ask again after each rebuild ("Always allow" stops it for that build).
  10. Backup, restore and the other tool dialogs are redone as DBeaver's (its `tasks` wizards for PostgreSQL
      and MySQL): objects picked from a checkbox tree (schemas, tables) instead of a text field, the engine's
      real options grouped (format, compression, encoding, clean/create, if exists, no owner, no privileges,
      jobs for directory format; for MySQL single transaction, routines, events, triggers, extended inserts),
      the output file chosen up front with a native save dialog, then a progress view with the tool's log,
      elapsed time, cancel, and "Show in Finder" at the end. Local clients, as DBeaver's "Local client":
      Settings → Local clients lists every detected install per engine (Homebrew `libpq`/`postgresql@N`,
      Postgres.app versions, EDB installers, `mysql-client`, `mariadb`, PATH) with its version, lets the user
      add one by folder and pick the default per engine; a connection can override it (Advanced tab); the
      dialog warns before running when the client is older than the server (pg_dump refuses a newer server).

## 4. v1 features (all required, priority order)

### A. Connections

1. New-connection dialog: engine picker (PostgreSQL, MySQL, MariaDB; searchable grid that will host more
   engines later), name, colour tag, environment (`local`/`staging`/`production`), group/folder.
2. Fields: host, port, user, password (save in keychain or ask each time), default database, connection URL
   paste that fills the fields, extra driver options.
3. SSL/TLS: modes disable/prefer/require/verify-ca/verify-full, CA/cert/key files.
4. SSH tunnel: host, port, user, password or key file (+ passphrase) or agent; keep-alive; clear errors.
5. Test connection (shows server version and latency), connect/disconnect, auto-reconnect on dropped
   connection, read-only flag, duplicate/edit/delete, import/export connections as JSON without secrets.

### B. Schema navigator

1. Click behaviour (owner request, as DBeaver): single click selects; double click on a table, view or
   materialized view opens its data tab (structure for other objects: routines open their source); Enter
   does the same as double click; the chevron, Right/Left arrows or a double click on a folder node expand and
   collapse. A setting chooses what double click on a table opens (data, structure, or expand).
2. Lazy tree: databases → schemas → tables, views, materialized views, functions, procedures, sequences,
   types/enums, triggers, extensions (PostgreSQL); databases → tables, views, routines, triggers, events
   (MySQL). Columns and indexes under each table.
3. Filter box, refresh node, row-count and size hints, icons per object kind.
4. Context menu: open data, open structure, new query here, copy name, generate SELECT/INSERT/UPDATE/DELETE,
   show DDL, rename, truncate, drop (with SQL preview and confirmation), create table/view/schema/database.

### C. SQL editor

1. Tabs, several per connection, each with its own connection/database/schema selector; tabs and unsaved text
   restored after restart; open and save `.sql` files.
2. Highlighting, schema-aware completion (keywords, functions, schemas, tables, columns, aliases, joins on
   foreign keys), bracket matching, comment toggle, find/replace, multi-cursor if the editor provides it,
   format SQL.
3. Run statement at cursor, selection, or whole script (one result tab per statement), cancel, timing, rows
   affected, server notices/warnings panel.
4. Query parameters (`:name` and `$1`/`?`) prompted before run.
5. EXPLAIN and EXPLAIN ANALYZE with a readable plan tree (cost, rows, time per node, slowest node
   highlighted) plus raw output.
6. Transaction mode per tab: auto-commit or manual with visible "transaction open" state, Commit and Rollback.
7. Query history (searchable, per connection, with duration and status) and saved queries (named, foldered).

### D. Results grid

1. Virtualized, streamed, smooth at 1,000,000 rows × 20 columns; column resize, reorder, hide, pin; client-side
   sort; row numbers; selection of cells, rows, columns, ranges.
2. Type-aware rendering: NULL distinct from empty, numbers right-aligned, dates/times with zone, UUID, JSON,
   arrays, booleans, binary (hex preview), long text truncated with a value viewer panel (JSON pretty-print,
   text wrap, hex).
3. Copy as TSV, CSV, JSON, Markdown, SQL INSERT; copy column names; export the full result (see G).

### E. Table data view

1. Open a table or view: paginated data, server-side sort by column header, filter bar (column / operator /
   value builder and a raw `WHERE` input), row count, refresh.
2. Inline edit of cells, add row, duplicate row, delete rows, set NULL / default; pending changes highlighted;
   "Review SQL" shows the exact statements; Apply in one transaction or Discard. Editing requires a primary or
   unique key; otherwise the view is read-only and says why.
3. Foreign keys: jump to the referenced row, and list rows that reference the current one.

### F. Structure

1. Table structure tabs: columns (type, nullable, default, comment), indexes, constraints, foreign keys,
   triggers, DDL.
2. Create and alter tables through a form editor (add/drop/rename/modify columns, indexes, foreign keys,
   comments) with live DDL preview, applied after confirmation.
3. View, function and procedure source: view and edit, apply with confirmation.
4. Create/drop database and schema; manage PostgreSQL extensions and enums.

### G. Import, export, backup

1. Export a table or a query result to CSV, JSON (array and NDJSON), SQL INSERT, Markdown; options for
   delimiter, header, NULL text, encoding; streamed to disk with progress and cancel.
2. Import CSV into an existing or new table: column mapping, type guess, preview, batch inserts in a
   transaction, error report.
3. Backup and restore through the installed `pg_dump`/`pg_restore`/`psql` and `mysqldump`/`mysql` binaries:
   detect them, choose path, show the command, stream its output, clear message when missing.

### H. Administration

1. Sessions/processes (`pg_stat_activity`, `SHOW PROCESSLIST`) with refresh, cancel query, terminate session.
2. Users and roles with their grants (read view), database and table sizes, server version and settings.

### I. App

1. Command palette, settings view (theme, fonts, editor, grid page size, confirmations), keymap override file.
2. macOS app bundle and Linux build (see §5); app data in the platform dirs; logs with secrets redacted.

Everything else (ERD diagrams, schema compare, data compare, visual query builder, SQLite and the other
engines, AI assistant, cloud sync) goes to `docs/ROADMAP.md`, ordered, with how the driver trait extends to
each. Not in v1.

## 5. Quality bars (measure, write numbers to `docs/BENCH.md`)

- Release build, Apple Silicon: cold start to first interactive frame ≤ 400 ms; idle RSS with one connected
  connection and one open query tab ≤ 150 MB.
- First rows of a large `SELECT` visible ≤ 150 ms after the server starts sending (localhost); scrolling the
  1M-row result keeps frame time under 16 ms on average (instrument it: log frame times in a debug overlay or
  a bench mode).
- Memory for 1M rows × 20 mixed columns fetched in full: report it; keep a compact columnar or arena storage
  for values, not a `String` per cell.
- `cargo clippy --workspace --all-targets -- -D warnings` clean, `cargo fmt --check` clean, `cargo deny check`
  clean.
- Linux: the workspace builds in a Linux container (Docker, Debian/Ubuntu image with GPUI's system deps,
  `nice`, `-j 3`). Add `.github/workflows/ci.yml` for macOS + Linux (fmt, clippy, tests with service
  containers, build). Do not create a remote or push.
- Packaging scripts: `scripts/bundle-macos.sh` (`.app` + `.dmg`, ad-hoc signed) and
  `scripts/bundle-linux.sh` (tarball + `.desktop` file; AppImage if straightforward).

## 6. Testing

- `docker-compose.yml` with pinned images: `postgres:17` (host port 55432), `mysql:8.4` (53306),
  `mariadb:11` (53307), container names `kerno-postgres`, `kerno-mysql`, `kerno-mariadb`, plus an SSH server
  container (`kerno-ssh`, port 52222) for tunnel tests. Seed scripts in `tests/fixtures/` with a realistic
  sample schema per engine: every common type, enums, arrays, JSON, FKs, views, materialized view, functions,
  procedures, triggers, a table without a primary key, and a generated 1M-row table.
- `kerno-core` and drivers: unit tests plus integration tests against the containers, skipped with a clear
  message when the containers are down. Cover connect (plain, TLS, tunnel), streaming, cancel, every type
  round-trip, introspection, DDL generation, pending-edit SQL generation and transactional apply.
- UI: GPUI test contexts (`TestAppContext`/`VisualTestContext`) for key flows: open connection, run query,
  edit a cell and apply, cancel a running query, command palette.
- Real-app checks: run the release app, drive it (the `orca-computer-use` skill or keyboard automation), and
  capture windows with `screencapture -l <windowid>` into `docs/screenshots/`. Look at every capture, fix
  what looks wrong, capture again. Light and dark.
- Test-first for core logic (SQL generation, parsing, type conversion, statement splitting).

## 7. Working method

- Milestones, one commit (or more) per milestone, conventional messages:
  0. Environment check, pinned GPUI + gpui-component hello window, workspace, licences, docker-compose,
     fixtures, CI file.
  1. Driver trait, PostgreSQL and MySQL drivers with tests.
  2. App shell: window, sidebar, tabs, theme tokens, command palette, storage, secrets, Tokio bridge.
  3. Connections (A).
  4. Navigator (B).
  5. SQL editor, results grid, history, saved queries (C, D).
  6. Table data view and editing (E).
  7. Structure (F).
  8. Import/export/backup and administration (G, H).
  9. Performance pass and `docs/BENCH.md`.
  10. Polish: UX/UI pass on every screen with screenshots, icon, empty/error/loading states, keyboard
      coverage, settings, packaging, Linux container build, README, docs.
- Keep `docs/PLAN.md` (milestones and status), `docs/PROGRESS.md` (log), `docs/DECISIONS.md` (choices and
  why) current. Update them before each commit.
- Model budget (owner request): the main session plans, reviews and integrates; subagents implement.
  - Write an implementation plan per remaining milestone in `docs/plans/M<n>.md` first: files to touch,
    types and functions, catalog queries per engine, tests to write, acceptance checks, screenshots to take.
    Detailed enough that an agent with no other context can implement it from the plan, CLAUDE.md and the
    code.
  - Implementation goes to subagents with the Agent tool, `model: "opus"`, `effort: "high"`, one bounded
    part of a plan per agent.
  - Mechanical work (running the test suite, captures, grep and inventory, doc updates, formatting) can go
    to `model: "sonnet"` or `model: "haiku"` when that is enough.
  - At most 2 subagents at once (they share `target/` and the CPU). Review each result yourself (diff, tests,
    capture) before committing.
- Commit locally only. Never add a remote, push, tag a release or publish anything.
- Use context7 for library docs; read the pinned sources when the docs and the code disagree.

## 8. Stop and ask the owner only if

- A step would publish something, push, create an account or a remote repo, or cost money.
- A step needs `sudo` or a system-level install beyond Rust toolchains, crates and Docker images.
- A step would touch a database or container that you did not create.

Otherwise decide, write the decision in `docs/DECISIONS.md`, and keep going.
