# Implementation plans

One file per remaining milestone: `M5.md` (what is left), `M6.md` (what is left), `M7.md`, `M8.md`, `M9.md`,
`M10.md`; plus `CTX.md` (context menus everywhere, connection folders, navigator clicks) and `PARITY.md`
(the DBeaver inventory and its gaps), both from the owner's feedback of 2026-11-13: DBeaver Community is
the primary reference for features and interactions (CLAUDE.md §3). Each is written so that an agent with no other context can implement it from the plan, `CLAUDE.md`
(the master spec, it wins over everything here) and the code. Read this page first: it holds what every
plan shares.

State when these plans were written (2026-11-12, commit `356bf42`): milestones 0 to 4 done; milestone 5 and 6
implemented except the parts listed in `M5.md` and `M6.md`; 91 app tests, 212 core tests, driver suites green.

## How the work is split

The main session plans, reviews and commits. Each **part** of a plan (`Part 1`, `Part 2`, …) is one bounded
task for one subagent (`model: opus`, `effort: high`). Parts of one plan that touch different files can run
two at a time, never more. Mechanical tasks (test runs, captures, doc tables) go to `sonnet` or `haiku`.

A subagent given a part must:

1. Read `CLAUDE.md`, this page, the plan of its milestone, and the files the part names.
2. Stay inside the files the part lists. If another file must change, say so in the report instead of
   changing it, unless the part says otherwise.
3. Write the tests the part lists, run them, and run the lint commands below.
4. Not commit, not start or stop containers, not launch the app unless the part says so.
5. Report: files changed, public signatures added, test names and results (the exact final command lines and
   their `test result:` lines), what is not done or not verified. No claim without a command that shows it.

## Rules that are easy to break (all from CLAUDE.md)

- Databases: only the `kerno-*` containers (`kerno-postgres` 55432, `kerno-mysql` 53306, `kerno-mariadb` 53307,
  `kerno-ssh` 52222). Other containers exist on this machine (`studio-*`, `pim-*`): never touch them. Tests
  create their own objects under names starting with `kerno_ui_` or `kerno_test_` and remove them, pass or
  fail (see `Scratch` in `crates/kerno/src/ui_tests.rs`). Never change the fixture data (`shop.*`,
  `analytics.*`, `public.all_types`): other tests assert on it.
- Every long command through `nice -n 10`. `cargo` jobs are capped at 3 in `.cargo/config.toml`. Tests with
  `--test-threads=3`. Run `uptime` before a heavy step; if the load average is above 11 (the core count), wait.
- Disk (owner's rule of 2026-11-13, after the disk filled up): run `df -h /` before each heavy build
  (a release build, a full test build after a dependency change, a Docker build). With less than 10 GB
  free, delete `target/debug/incremental` first — only when no `cargo` or `rustc` is running
  (`pgrep -x cargo; pgrep -x rustc`); a subagent that cannot (the other agent is building) stops and
  reports instead. `target/` must stay under about 20 GB: report its size (`du -sh target`) at the end of a
  part. Docker images built for a check are removed when the check is done.
- **At most 2 subagents at once, of any kind** (documentation-only agents count too).
- One Kerno instance at a time; quit it (`pkill -x kerno`) when done. No process left running.
- No secret in a file, a log or a panic message. No telemetry, no network call other than to the database.
- Every statement that writes or destroys goes through `crates/kerno/src/sql_confirm.rs` (shows the SQL, asks;
  production asks twice; Enter never confirms). A `read_only` connection refuses writes in the client.
- The UI thread never waits on I/O: database, file and process work goes through `crate::bridge::spawn`
  (Tokio) or `bridge::spawn_blocking`, and the result comes back with `cx.spawn` / `cx.spawn_in`.
- Clean IP: DBeaver (Apache-2.0) logic may be ported with a comment naming the source file; Beekeeper Studio
  (GPL-3.0) may be read for behaviour only, never translated; never open its `src-commercial/`. References
  are cloned in `~/VM/refs/` (outside the repository). Catalog queries are written from the engine docs.
- English for code, comments, UI text, docs and commits. No `#[allow(...)]` to silence a lint: fix the code
  (type alias for `type_complexity`, a struct for `too_many_arguments`).
- Commit locally only (the main session does it). Never push, never add a remote.
- Ponytail style: the smallest design that is complete. No trait with one implementation, no option for a
  value that never changes, no file "for later". Reuse what exists (list below) before writing new code.

## Commands

```bash
scripts/test-env.sh up          # starts the four kerno-* containers and waits for the seed (main session only)
scripts/test-env.sh down        # stops them (main session, end of session)

nice -n 10 cargo fmt --all
nice -n 10 cargo clippy --workspace --all-targets -- -D warnings
KERNO_REQUIRE_CONTAINERS=1 nice -n 10 cargo test -p kerno -- --test-threads=3            # app (UI) tests
KERNO_REQUIRE_CONTAINERS=1 nice -n 10 cargo test -p kerno-core -- --test-threads=3
KERNO_REQUIRE_CONTAINERS=1 nice -n 10 cargo test -p kerno-postgres -- --test-threads=3
KERNO_REQUIRE_CONTAINERS=1 nice -n 10 cargo test -p kerno-mysql -- --test-threads=3
KERNO_REQUIRE_CONTAINERS=1 nice -n 10 cargo test --workspace -- --test-threads=3         # everything
nice -n 10 cargo build --release -p kerno
```

`KERNO_REQUIRE_CONTAINERS=1` turns "container down, test skipped" into a failure. Two subagents share
`target/`: cargo serialises builds, so expect waits, not errors.

## Map of the code

- `crates/kerno-core` (no GPUI): `driver.rs` (`Driver`, `Session`, `AdminView`, `TxState`), `dialect.rs`
  (`Dialect`: quoting, `literal`, `limit_offset`, `explain`, `parse_plan`, `rename_sql`, `drop_sql`,
  `truncate_sql`, `create_schema_sql`, `create_database_sql`, `cast_to_text`), `schema.rs` (catalog types:
  `TableRef`, `ObjectRef`, `RelationKind`, `TableStructure`, `ColumnInfo`, `IndexInfo`, `ConstraintInfo`,
  `ForeignKeyInfo`, `TriggerInfo`, `RoutineInfo`, `TypeInfo`, `ExtensionInfo`…), `exec.rs` (`run` streaming
  with back-pressure, `collect`, `RowDemand`, `RowLimit`), `result.rs` (`ResultStore`, `RowBatch`, `Column`,
  `TypeKind`: cells are the server's text, `None` is NULL), `format.rs` (cell display, `pretty_json`,
  `hex_dump`, `byte_size`), `sort.rs`, `export.rs` (`Exporter`, `ExportFormat`, `export_to_string`),
  `browse.rs` (`Filter`, `Condition`, `Operator`, `page_sql`, `count_sql`), `pending.rs` (`Pending` edits of
  a page), `plan.rs`, `storage.rs` (SQLite app storage, `AppPaths`), `secrets.rs`, `config.rs`, `ssh.rs`,
  `connector.rs`, `sql/` (`lex`, `split`, `params`, `classify`, `generate`, `edits` with `edit_statements`
  and `apply_edits`, `complete`, `format`), `testing.rs` (feature `test-support`: `available(engine)`,
  `config(engine)`, `params(engine)`, `secrets()`).
- `crates/kerno-postgres`, `crates/kerno-mysql` (MySQL and MariaDB): `connect.rs`, `session.rs`,
  `introspect.rs`, `dialect.rs`, `plan.rs`, `ddl.rs`; integration tests in `tests/` with `tests/common`.
- `crates/kerno` (the app): `main.rs` (menus, startup), `actions.rs` (every command: action type, palette
  entry, default key; the `command!` macro), `workspace.rs` (window, tab bar, `OpenTab` enum, status bar,
  command handlers), `tabs.rs` (`Target`, `TabSnapshot` stored between runs), `sidebar.rs`,
  `connections/` (`manager.rs`: `Connections` entity, `open_session(id, database, cx)`, `meta_session(id)`,
  `status(id)`, `get(id)`; `dialog.rs`, `form.rs`), `navigator/` (`model.rs` pure tree, `actions.rs` menu
  entries, `view.rs`), `query/` (`tab.rs` `QueryTab`, `grid.rs` `Grid` table delegate shared by results and
  table data, `params.rs`, `viewer.rs`, `plan_view.rs`, `comment.rs`), `data/tab.rs` (`DataTab`),
  `library.rs` (history and saved queries), `sql_confirm.rs`, `settings.rs`, `state.rs` (`AppState` global:
  storage, secrets, settings, paths), `theme.rs` (tokens), `engine_logo.rs` (logos and embedded icons),
  `bridge.rs`, `logging.rs`, `ui_tests.rs` (all UI tests and their `Fixture`).

## UI kit notes (gpui-kit 0.9.1; its sources are in `~/.cargo/registry/src/*/gpui-kit-0.9.1`,
`gpui-component-0.9.1`, `gpui-base-0.9.1`: read them, do not trust memory)

- An entity cannot be read while it is being updated: a table delegate cannot read its own `TableState`
  inside `render_td`. Mirror what you need in the delegate.
- `window.open_dialog(cx, |dialog, _, _| …)`; `.on_ok(|_, _, _| false)` keeps Enter from closing it. After a
  dialog closes, give the focus back with `window.defer`.
- A dropdown: `Button::new(id).dropdown_caret(true).dropdown_menu(|menu, _, _| menu.item(PopupMenuItem::new(…)
  .on_click(…)))` (see `query/tab.rs` toolbar, `data/tab.rs` filter bar).
- A context menu: `.id(…)` then `.context_menu(|menu, window, cx| …)` on the element itself. The kit's table
  does not open a row menu from a cell in cell-selection mode: `Grid::render_td` attaches the menu per cell.
- An element is found by tests only if it has `.id("…")` followed by `.test_support()` (trait
  `TestSupportExt`), and ids must be unique in the window (`("node", index)` not a bare integer).
- `InputEvent::Change` for text changes (observing an input also fires on cursor moves);
  `InputEvent::PressEnter`, `InputEvent::Blur`.
- Icons: only glyphs listed in `engine_logo.rs` (`icon_assets!`) are embedded; add the name there to use one.
- Notifications: `window.push_notification(Notification::error(…) / ::warning / ::success, cx)`.

## UI test notes (`crates/kerno/src/ui_tests.rs`)

- `connected(cx, engine)` opens the app on a test server; `app.step(cx, |window, cx| …)` runs one
  interaction; `app.click(id)`, `app.press(&["secondary-t"])`, `app.type_text("…")`, `app.has(id)`.
- Network and storage run on real threads: wait with `app.wait_until(cx, "what", |app, cx| condition)`,
  never with a fixed sleep. Timers (debounce) use the simulated clock: `wait_for_stored_tabs(…).await`.
- `on_server(engine, sql)` runs SQL outside the app; `Scratch::new(engine, cleanup, setup)` creates objects
  and drops them when the test ends. Tests run three at a time: every test uses its own schema name.
- Loop over `[Engine::Postgres, Engine::MySql, Engine::MariaDb]` when a feature exists on all three; start
  each with `if !testing::available(engine) { return; }`.
- Menus are tested through the function that fills them (see `Navigator::perform`, `DataTab::related`), plus
  one real right-click test (`a_right_click_on_a_cell_opens_the_menu_of_its_row`).

## Reliability of the suite

`KERNO_REQUIRE_CONTAINERS=1 nice -n 10 cargo test --workspace -- --test-threads=3` must pass every time, on a
busy machine too. Three tests of one binary run at once against the same four servers, and one test process at a
time holds the servers (`testing::exclusive_run`). A new test follows these rules:

- **Own objects only.** Create what you assert on under a name no other test uses (`kerno_ui_<test>` /
  `kerno_test_<test>`, through `Scratch` or `common::scratch_schema`); grep the name before you pick it. Never
  assert on a server-wide fact another test can change while you run: the first row of a sizes list, the list
  or count of sessions, roles, databases, schemas or locks, the processlist. Filter it to your own objects first
  (your schema, role, user, a marker in the statement text), or to the fixture with the scratch prefixes left
  out (see `fixture_rows` in the drivers' `tests/introspect.rs`). Count sessions under a login role of the test's
  own (`every_owner_of_a_session_closes_it_when_it_goes_away`), never all of `kerno`'s.
- **Wait for the state, not for time.** Anything that goes through the network, storage or the runtime is
  awaited with `app.wait_until(cx, "what", |app, cx| condition)` (it gives up after 30 s and prints what the
  window shows). The condition must name the end state itself (the outcome text, the row count, the status),
  not something that is also true before the action took effect, such as `!running` alone. No `sleep` instead
  of a condition, no retry around a whole test, no longer deadline without knowing what takes that long.
- **Cancel what is on the server.** `running` is true as soon as the tab starts a run, before the statement
  reaches the server; a cancel request that arrives before it is ignored there. A test that cancels waits for
  `running_on_server(engine, marker)` first (a unique alias or comment in the statement).
- **A 30 s timeout is a state that never came, not slowness.** Read the message (`situation()`: tabs, results,
  activity, status bar) and look for a refused action (a run that silently did not start because another was
  running) or a real race in the app before touching the test.
- **The server to yourself.** No test changes a server-wide setting. A test that terminates sessions, kills
  queries or holds locks does it only on sessions and objects it opened itself (a `Sleeper`, its own role, its
  own table). No test needs the whole server today, so there is no lock for it. If one ever does (a global
  setting, a restart), add one `static` mutex in `ui_tests.rs` (and one in the driver's `tests/common`) that
  every such test takes, so they never overlap each other while the rest of the suite keeps running beside
  them. Do not lower `--test-threads`.
- **Cleanup that cannot be skipped.** Declare the `Scratch` before the app (it is dropped after it), drop at
  the start what an earlier run may have left, and end every transaction the test opens. `Scratch` does not
  panic a second time when the test already failed (that would abort the whole run): it prints the cleanup
  that did not run, which happens when the failed test's app still holds a transaction on those objects (the
  drop gives up after the 15 s lock timeout); the next run of that test drops them first.
- **Measure flakiness, do not guess.** Before calling the suite reliable, run the `kerno` binary 20 times in a
  row, 5 of them beside a CPU-heavy job (`nice -n 10 cargo build --release -p kerno`), and check that no
  `kerno_ui_*` / `kerno_test_*` schema, database, role or user is left on the servers afterwards.

## Real-app checks and captures (main session or a `sonnet` agent)

- **Keyboard layout.** The owner's Mac uses the French (AZERTY) layout and `orca computer hotkey` sends
  physical key positions: `cmd+a` arrives as Cmd+Q and quits the app, `cmd+w` and `cmd+z` are swapped,
  `cmd+m` and punctuation land elsewhere. Drive letter shortcuts through the command palette (Cmd+K is at
  the same place) or the menus; an app that "quit by itself" with exit status 0 was told to.

`scripts/capture.sh` and `scripts/window-id.swift` capture the window (`screencapture -l <id>`). The app runs
on a scratch data directory: `KERNO_DATA_DIR=<dir> KERNO_SECRET_STORE=memory KERNO_APPEARANCE=<light|dark>
nice -n 10 target/release/kerno`. It is driven with the `orca-computer-use` skill (`orca computer click
--app pid:<pid> --x … --y …`, `type-text`, `press-key`; coordinates are window points; `hotkey` is not
available, use buttons and menus). Look at every capture; fix what looks wrong; capture again. Captures go
to `docs/screenshots/` as `m<n>-<what>-<light|dark>.png`. For demo data, create a `kerno_ui_demo` schema in
`kerno-postgres` and drop it afterwards (the PostgreSQL introspection test fails while it exists).

## When a part is done (main session)

1. Read the diff. Re-run the tests named in the report and the lint commands. Look at the capture if any.
2. Add the rows to `docs/FEATURES.md` (item, status, covering tests, screenshot), an entry in
   `docs/PROGRESS.md`, the decisions in `docs/DECISIONS.md` (next number after the last `D-0xx`), the status
   in `docs/PLAN.md`.
3. Commit with a conventional message ending with the `Claude-Session:` line of the session.
