# Kerno — measured performance

The targets are those of CLAUDE.md §5. Every number below was produced by `scripts/bench.sh` (or, where said,
by one of its steps run alone) on the release build of commit `b34bd15`, the final code of milestone 11 (v1.1), on 2026-11-14.

## Verdicts

| Target (CLAUDE.md §5) | Measured (median, min–max) | Verdict |
|---|---|---|
| Cold start to first interactive frame ≤ 400 ms | 264 ms (229–648), 30 launches | met |
| Idle RSS, one connection and one query tab, ≤ 150 MB | 149.9 MB (149.5–150.4, nine runs) in `Kerno.app`; 152.1 MB (152.0–152.7) for the bare binary | met at the median for the app, with no margin: four runs of nine read 150.0 to 150.4; the bare binary is 2.1 MB over (see below) |
| First rows visible ≤ 150 ms after the server starts sending | 6.2 ms PostgreSQL (2.2–7.5), 3.1 ms MySQL (2.2–4.6) | met |
| Scrolling 1,000,000 rows: frame time under 16 ms on average | 8.40 ms vertical, 8.33 ms horizontal (120 Hz display) | met |
| Memory for 1,000,000 rows × 20 mixed columns fetched in full: report it | 340 MB for the values, 17 bytes per cell; process RSS 494 MB | reported |

## Machine and build

- Apple M3 Pro (11 cores), 18 GB, macOS 27.0.1, built-in 120 Hz display.
- `rustc 1.99.1`, `cargo build --release`, GPUI with runtime-compiled Metal shaders (D-002: the Metal
  toolchain is not installed on this machine; build-time shaders would remove their compilation from the
  start-up path).
- Servers: the `kerno-postgres` (PostgreSQL 17) and `kerno-mysql` (MySQL 8.4) containers on localhost, with
  the fixture table `analytics.big_table` / `kerno_analytics.big_table`: 1,000,000 rows × 20 columns
  (integers, numerics, text, a 32-character hash, timestamps, booleans, JSON, UUID).
- 1-minute load average during the runs: 1.9 to 3.95 (the script refuses above 4 and waits). Nothing else of
  Kerno was compiling or running.

## Method

`scripts/bench.sh [runs] [startup-runs]` builds the release binary and runs it in its bench mode
(`KERNO_BENCH`, `crates/kerno/src/bench.rs`), each run on a fresh copy of a scratch data directory with the
in-memory secret store, so the user's own Kerno data and keychain are never touched. One untimed launch comes
first (a freshly built binary pays the system's one-time checks). The update check is off in bench mode.
Five runs per measure, thirty for the start-up, which is short and varies the most; the table gives the
median and the range.

The table marks each median against its target, `met` or `MISSED`, and the script exits 1 after printing it
when one is missed (K10, C1): a regression fails the run. On the numbers of this page a full run exits 1, for
the bare binary's idle memory, 152.1 MB for 150 (D-082; the bundled app, 149.9 MB, is measured by hand). A
measure whose every run failed ("error in …") has no row and does not fail the run. The raw lines are kept in
`target/bench-raw.tsv`; `scripts/bench.sh --report FILE` prints their table again and builds and measures
nothing.

- **Start-up** (`KERNO_BENCH=startup`): from spawning the process to the second frame drawn. The first
  frame is on screen after 161 ms (133–447) from `main`; the app is counted interactive at the next frame,
  after macOS has activated it: 254 ms from `main`, 264 ms from the spawn. Kerno's own work before the first
  frame is about 20 ms; the rest is the platform (process start, AppKit, the font list, the window and the
  Metal renderer).
- **Idle memory and first rows** (`KERNO_BENCH=first_rows`): the app connects, opens one query tab, waits
  until it is idle, and samples its RSS and its footprint (`idle_rss_mb`, `idle_footprint_mb`: 203 MB of
  footprint, most of it the window's surfaces, which RSS does not count). It then runs
  `SELECT * FROM analytics.big_table` and times, from the arrival of the column descriptions (the server has
  started sending), to the frame that shows the first page of rows.
- **Full fetch**: "Fetch all" on that result, to the last row: 1.44 s on PostgreSQL (1.32–1.70), 1.29 s on
  MySQL (1.25–1.71), about 700,000 rows per second. The values are held in compact batches, not a `String`
  per cell: 340,098,183 bytes for 20,000,000 cells on PostgreSQL, 335,721,274 on MySQL.
- **Scroll** (`KERNO_BENCH=scroll`): with the million rows in the grid, the bench scrolls 10 seconds
  vertically (about 835 rows per frame) and 3 seconds horizontally and records the interval between frames:
  average 8.40 ms, median 8.33 ms, 95th percentile 8.39 ms, 99th percentile 8.42 ms; the longest single
  interval of a run was between 9.4 and 74 ms (one frame of a run, when it happens).
- **Sort** (no target): a click on the header of a text column of the million rows, on a result the server
  does not order (a `UNION ALL`, so that the sort is Kerno's): 225 ms (221–240), off the UI thread.
- The bench refuses to sample a window that is hidden, minimised or on another Space: macOS then draws no
  frame and the figures mean nothing.

## Idle memory: two figures, and why

`scripts/bench.sh` runs the bare binary, `target/release/kerno`: **152.1 MB** (152.0–152.7 over the steps of
the run), 2.1 MB over the target. The same binary started from `Kerno.app`
(`dist/Kerno.app/Contents/MacOS/kerno`, the `first_rows` step alone, nine runs): **149.9 MB** (149.5, 149.7,
149.8, 149.9, 149.9, 150.0, 150.2, 150.3, 150.4). The median is under the line and four runs of nine are on
it or just over: the target is met at the median and has no margin left.

- **Bare against bundled** (about 2 MB): the Dock icon. Outside a bundle Kerno sets its icon itself so the
  Dock and the app switcher show it (an owner's requirement of 2026-11-13); AppKit then keeps the icon
  rendered and its decoding touches framework code. A bundled app does not pay it: the system reads the
  bundle's icon (D-053).
- **v1.1 against v1** (about 1.6 MB more: 148.4 MB bundled and 150.5 MB bare at the end of milestone 10):
  the binary grew with the nineteen items (80.0 MB against 73.0 MB), and with it the data pages that are
  resident once the loader has fixed them up (`__DATA`, `__DATA_CONST` and `__DATA_DIRTY` of the process in
  `vmmap`: 32.0 MB resident against 29.9 MB). Nothing new runs at idle: the AI assistant, vim mode and the
  diagram code are not touched until used.
- **What could be recovered, and was not**: a 16.2 MB block the system allocator keeps, still resident, after
  the editor's syntax highlighter compiled its query (`Malloc Large (empty)` in `vmmap`; tree-sitter's scratch
  memory, freed by the library, not returned to the system). `malloc_zone_pressure_relief` returns it and
  would put idle RSS near 134 MB; it is a second `unsafe` call, which the workspace does not allow (D-042,
  D-053): the owner's decision. A safe way was tried on 2026-11-14 (freeing untouched large blocks to push
  that block out of the allocator's keep-list): the block stayed, the change was removed.

## History of the figures

| When | Commit | Start-up | Idle RSS | First rows | Scroll | 1M × 20 |
|---|---|---|---|---|---|---|
| Morning, busy machine (indicative) | `2b0f3bf` | 452–676 ms | 122–162 MB | 3–4 ms | 8.4 ms | 340 MB |
| Afternoon, 5 start-up runs | `4777368` | 439 ms (389–990) | 138 MB | 2–5 ms | 8.4 ms | 340 MB |
| Afternoon, 30 start-up runs | `4777368` | 341 ms (293–713) | — | — | — | — |
| Evening, before the icon fix | `949d5a3` | 270 ms (251–415) | 170 MB | 1–4 ms | 8.4 ms | 340 MB |
| Final of milestone 10 | `97a05b8` | 279 ms (235–698) | 150.5 MB bare, 148.4 MB bundled | 2–7 ms | 8.3 ms | 340 MB |
| Final of milestone 11 (v1.1) | `b34bd15` | 264 ms (229–648) | 152.1 MB bare, 149.9 MB bundled | 3–6 ms | 8.4 ms | 340 MB |

The start-up needs many runs to be read: with five, one slow launch moves the median by a hundred
milliseconds. The slow launches (the maximum of each row) are the first ones after other work on the
machine.

## Cases competitors fail (CLAUDE.md §4 J9)

What users of other clients report as slow or stuck, measured the same way (`KERNO_BENCH` modes, a scratch
data directory, the in-memory secret store, the update check off). Each step runs alone with
`scripts/bench.sh --only wide|navigator|import|reconnect`, or with every other measure when `--only` is left
out. The steps create their scratch objects, `kerno_bench_*` databases on `kerno-postgres` and `kerno-mysql`,
and drop them, pass or fail (an exit trap, after Kerno quit); the reconnect step runs a server of its own.

Final numbers: five runs of each step (median, then the range), release build of commit `b34bd15`, on a quiet
machine (1-minute load 1.9 to 3.95), 2026-11-14.

| Case (demand) | Target | Measured | Verdict |
|---|---|---|---|
| Scroll a 300-column table (Beekeeper #752) | average frame < 16 ms | PostgreSQL 8.34 ms down, 8.33 ms across; MySQL 8.33 ms down, 8.33 ms across | met |
| Open a database of 200 schemas (Beekeeper #135) | < 300 ms to the frame showing them | PostgreSQL 22 ms (20–58); MySQL (a database of 1,200 tables, to its folders) 37 ms (34–60) | met |
| Open a schema of 1,200 tables (Beekeeper #135) | < 500 ms to the frame showing them | PostgreSQL 103 ms (102–220) for the schema, its objects read, then 4 ms (2–5) for Tables; MySQL 4 ms (3–5) for Tables | met |
| Run a 200 MB `.sql` file (Beekeeper #702, #2325) | UI responsive, progress, cancel < 2 s | 17.7 s (16.4–18.2), 11.5 MB/s; frames 8.36 ms on average, 99th percentile 8.50 ms, longest 31 ms (11–63); cancelled at 30 %: stopped in 82 ms (34–93) | met |
| Server killed and started again (DBeaver #6903) | usable < 5 s after it accepts connections again | 975 ms (948–1,004); the loss seen at once, two statements reported failed, none run again | met |
| Server frozen 15 s (`docker pause`) | usable < 5 s after it is unfrozen | 171 ms (69–267); the loss found by the keep-alive 13.5 s into the freeze | met |
| Server's network detached 10 s (informative) | — | −38 ms: the connection survives on OrbStack, nothing to reconnect | — |

- **wide** (`KERNO_BENCH=scroll` on another table): `kerno_bench_wide.wide`, 300 columns plus a key ×
  20,000 rows: PostgreSQL cycles int, bigint, numeric, text, timestamp and boolean; MySQL int, bigint,
  decimal, varchar(20), date and TEXT (300 VARCHAR(255) would pass the 65,535-byte row limit). The scroll
  mode fetches everything (20,000 × 301 cells: 286 ms and 74.5 MB on PostgreSQL, 230 ms and 84.0 MB on MySQL)
  and scrolls 10 s down (17 rows a frame) and 3 s across, one column a frame, both ways; the longest single
  interval of the five runs was 69 ms (PostgreSQL, down) and 70 ms (MySQL, across), one frame each.
- **navigator** (`KERNO_BENCH=navigator`): `kerno_bench_many` holds 200 schemas `s001`–`s200`, `s001` with
  1,200 tables of five columns (MySQL: the database itself holds the 1,200 tables), created statement by
  statement so the shared server's lock table stays small. The connection opens on `kerno`; the bench opens
  `kerno_bench_many` under it (a session in that database first), then `s001`, then its Tables folder, and
  times each from the call that opens the row to the frame that draws the first row below it. A schema's
  objects are read when the schema opens (one catalog query per kind), so its Tables folder then opens
  from memory.
- **import** (`KERNO_BENCH=import`): `scripts/bench.sh` writes a 200,057,575-byte file of `INSERT`s, 1,000
  rows each, for `kerno_bench_import.items`; the Execute script wizard runs it with `psql` (Homebrew's,
  found as the user's would be), the bench calling the step its confirmation would. `psql` now reads the
  file from its standard input, which Kerno feeds and counts: the wizard shows a bar and "x of y sent
  (n %)". The mode asks for a frame after every frame and records the intervals: what a frozen UI would
  show as long ones. The first 0.20 s is the scan of the file for client commands, before `psql` starts.
  A second run cancels at 30 % of the file and times the stop (the client killed, the feeding stopped).
  The MySQL side of this step is not measured (same code path, `mysql` reads its standard input).
- **reconnect** (`KERNO_BENCH=reconnect`): a dedicated server, `kerno-bench-pg` (postgres:17) on its own
  network `kerno-bench-net`, host port 55440, created and removed by the step; the shared containers are
  never touched. The app runs `SELECT 1` in a query tab every 500 ms and prints `ready` once one succeeded;
  the script then breaks the server one of three ways, notes the wall clock when it is back, and the app
  reports the wall clock of the first statement that succeeds after one was held up (more than 2 s) or
  failed; the step prints the difference. *Killed*: `docker kill`, `docker start`, back when `pg_isready`
  answers inside the container (crash recovery included). *Frozen*: `docker pause` 15 s, back at
  `docker unpause` (nothing closes: the case of a dead route or a hung server). *Detached*, informative:
  `docker network disconnect` 10 s; with OrbStack (Docker 29.8) the published port and the client's TCP
  connection survive it, the statement in flight finishes as the network returns, before the `docker`
  command does (a negative difference), and no reconnect is needed. What Kerno does (D-062): a keep-alive
  check every 5 s with a 5 s timeout, tried twice; a statement failing on a closed connection; then
  attempts of at most 10 s, again after 0.5 s, 1 s and every 2 s. The statuses each run went through are
  printed with the numbers (Connected, Connecting, Connected).

## A table's data with 100,000 rows loaded (K36, offscreen)

A table's data tab now reads its rows a load at a time (D-109). Measured on 2026-11-23 on `analytics.big_table`
(PostgreSQL), fetch size 10,000, with a release build of a temporary test (not committed) that drives the app
offscreen (`HeadlessAppContext`, Metal's headless renderer, 1200 × 800): no window could be opened for this
item, so these figures are the time to lay out, paint and render one frame, not the interval between frames on
a display as above. The machine was busy (1-minute load 9 to 50, another build beside it).

| Measure | Result |
|---|---|
| A load of 10,000 rows, offsets 10,000 to 90,000, from the call to the rows on screen | 50 to 68 ms each |
| Process memory, 10,000 then 100,000 rows of 20 columns | 76 MiB, then 106 MiB (+29 MiB) |
| Scrolling the 100,000 rows down and up, 150 rows a frame, 1,332 frames | 7.72 ms on average, median 7.32, 99th percentile 13.75, longest 36.05 |

## A faster backup: pg_dump on several connections (K48)

Measured on 2026-11-23 through the simple backup view itself, driven by a temporary UI test (not committed): the
time from Back up to the end of the run, the size of what was written, three runs each, with the client Kerno
picks for the PostgreSQL 17 test server (Homebrew libpq 19.0's pg_dump), 11 cores, so "Faster" is `--jobs=8`.
The machine was shared with another build (1-minute load 5 to 7). Sizes in MB of 10⁶ bytes: the app's own end
message counts in units of 2²⁰ (92.8 MB here is its "88 MB").

| Database | Without "Faster" (custom archive) | With "Faster" (directory, 8 jobs) |
|---|---|---|
| `kerno` (317 MB; `analytics.big_table` alone is 301 MB) | 5.78, 5.66, 5.92 s; 92.8 MB | 5.70, 5.63, 5.33 s; 92.8 MB |
| `kerno_test_k48_bench` (299 MB in 8 tables of 125,000 rows, made and dropped by the test) | 3.46, 3.51, 3.59 s; 61.2 MB | 1.01, 1.38, 0.95 s; 61.2 MB |

On the fixture database "Faster" gains almost nothing: pg_dump gives each table to one connection, and one table
holds nearly all the data. On eight tables of similar size it is about 3.3 times faster, for the same size.

Compression (one run each, pg_dump by hand on `kerno`, custom format): pg_dump 17.12's default (gzip) 7.3 s and
92.9 MB, `--compress=zstd` 2.9 s and 90.1 MB. libpq 19.0's pg_dump, the one Kerno picks here, refuses `zstd` and
`lz4` ("this build does not support compression with ZSTD"): Kerno does not use them (D-117).

## Foreign keys in the grid (K49, offscreen)

The header marks of foreign keys and the arrow of a hovered or selected cell (D-118), measured on 2026-11-23 the
same way as K36 (release, `HeadlessAppContext`, 1200 × 800, a temporary test not committed) on scratch PostgreSQL
tables: each table with foreign keys against the same table without them, one cell selected and one under the
pointer in a key column, two runs (the machine busy, 1-minute load 6 to 15). Average frame, then 99th percentile.

| Measure | With foreign keys | Without |
|---|---|---|
| 100,000 rows of 20 columns (5 keys), down and up, 150 rows a frame, 1,332 frames | 7.47 / 8.82 ms; 10.34 / 19.19 | 9.01 / 8.41 ms; 20.67 / 17.47 |
| 300 columns (30 keys), 1,000 rows, across and back, one column a frame, 600 frames | 8.02 / 7.01 ms; 11.08 / 11.12 | 8.27 / 7.43 ms; 19.56 / 10.47 |
| The same, down and up, 17 rows a frame, 116 frames | 8.47 / 8.02 ms; 9.35 / 15.99 | 8.28 / 7.90 ms; 9.47 / 12.45 |

## Data compare (K60)

The rows of `analytics.big_table` (1,000,000 rows of 20 columns: integers, numerics, floats, text, dates, times,
timestamps with and without zone, a uuid, `jsonb`, an array, `inet`, an enum) against a copy in the scratch
schema `kerno_test_dc_big`, made and dropped by the run, with 1,000 rows deleted, 1,000 added and 2,000 changed:
the core's compare (`data_compare::run`, the same code the tab runs) on two sessions of the PostgreSQL 17 test
server, both read at once, merged as they arrive. Release build, measured on 2026-11-24 with
`/usr/bin/time -l` around the test process alone (the copy, the compare and the script), the machine shared
with other builds (1-minute load 8 to 27).

| Measure | Result |
|---|---|
| Compare, 1,000,000 rows against 1,000,000, four runs | 2.9, 2.1, 1.7, 2.0 s (the first before progress was told at most every 100 ms) |
| Found | 1,000 only in the source, 1,000 only in the target, 2,000 different, 997,000 the same: the planted ones |
| Differences kept (all 4,000) | 1.98 MB in their arenas; the script of their statements 774 kB |
| Peak memory of the whole process (maximum resident set size) | 18.3 to 19.1 MiB |

The memory does not grow with the tables: each side holds the batch being merged (the execution channel lets
four wait), the differences kept are copied out of their batches, and at most 10,000 of them (or 64 MiB of
their values) are kept; beyond, they are counted. The key is an integer, read in its index's order; a text key
is ordered by the bytes of its text (D-126), which the server sorts first.

`cargo test -p kerno-postgres --release --test data_compare -- --ignored` runs it.

## Foreign keys of any result (K59)

Between a result's rows on screen and its marks lie, on PostgreSQL, one query naming every column of the result
(its tables' numbers in the row description) and the match of their keys against the completion catalog the tab
already holds; MySQL, MariaDB and SQLite name their columns themselves and skip the query. Measured on
2026-11-24 (debug build, the machine busy) by `kerno-postgres` `edits::three_hundred_columns_over_twenty_tables_are_named_in_one_query`,
300 columns over 20 joined scratch tables, 19 keys: names 1.57 ms, names and keys 1.68 ms. In the app, a
temporary probe in a UI test (debug, `ui_tests::k59`, not committed) gave 39 to 74 ms from the result's columns
arriving to the marks set, three results: that includes the test executor's turns between frames of the grid.
The marks show on the frame after. The names are kept in the tab: a result read again asks nothing. The grid's frame cost is that of K49 (a key looked up per header and per hovered or selected cell,
the keys worked out once per result); the frame times above were not measured again.

## A faster backup and restore for MySQL and MariaDB (K63)

Measured on 2026-11-24 with a temporary driver test (not committed) that calls what the simple backup and
restore call: the plain backup is one `mysqldump` of the database into one file (`tools::backup_command`) and
its restore one `mysql` client (`tools::restore_command`); "Faster" is `mysql_folder::back_up` and
`mysql_folder::restore` with 8 processes (the machine's 11 cores, 8 at most). Clients: Homebrew's
`mysql-client` 26.10 on MySQL 8.4 (`kerno-mysql`), Homebrew's `mysql@5.7` 5.7.44 for MySQL 5.1
(`kerno-mysql51`); restores into a scratch database `kerno_test_k63_bench`, never a fixture database. Each run
waits for a 1-minute load below 8 before it starts. Sizes in MB of 10⁶ bytes.

| Database | Plain backup | Plain restore | Faster backup | Faster restore |
|---|---|---|---|---|
| `kerno_analytics` (8.4; `big_table`, 1M rows, 238 MB of data, cut in 8 ranges, and `events`; 307.1 MB written, 10 files) | 1.88, 1.81, 1.79 s | 16.49, 14.26, 17.12 s | 0.81, 0.81, 0.83 s | 8.36, 6.49, 7.24 s |
| `kerno_test_k63_eight` (8.4; eight tables of 125,000 rows of `big_table`, 30 MB of data each, not cut; 303.1 MB, 9 files) | 1.94, 1.80, 2.85 s | 15.52, 14.80, 17.22 s | 1.23, 0.98, 1.14 s | 6.49, 7.48, 6.81 s |
| `kerno_test_k63_eight` on MySQL 5.1.73 (eight tables of 125,000 rows, 16 MB each, not cut; 129.8 MB, 9 files), one run | 1.18 s | 7.20 s | 0.68 s | 3.69 s |

The 1-minute load at the start of each run was between 7.0 and 7.9 (the test servers are shared with other
agents' runs). "Faster" backs up about 2.2 times faster and restores about 2.2 times faster on 8.4, on one large
table cut into ranges as on eight tables; about 1.7 and 2 times on 5.1. The files are the same size.

**The threshold** (`RANGE_THRESHOLD`, 32 MiB): one table of `big_table`'s rows, backed up and restored whole by
one process, then cut into 8 ranges, three runs each (load 3 to 6):

| Data (`DATA_LENGTH`) | Whole: backup, restore | Cut in 8: backup, restore |
|---|---|---|
| 238 MB (1M rows) | 1.70–1.77 s, 14.2–14.7 s | 0.70–0.74 s, 6.0–8.5 s |
| 59 MB | 0.46–0.53 s, 3.2–3.6 s | 0.19–0.27 s, 1.4–1.9 s |
| 30 MB | 0.29–0.32 s, 1.9–2.0 s | 0.15–0.19 s, 0.8–1.2 s |
| 14 MB | 0.14–0.15 s, 1.0–1.1 s | 0.08–0.09 s, 0.4–0.5 s |
| 8 MB | 0.08–0.11 s, 0.5 s | 0.07 s, 0.3–0.5 s |

One mysqldump of an empty table takes 18 to 21 ms (ten runs). A cut helps at every size alone, but below about
32 MB it saves under a second, and in a database of many tables the other tables keep the processes busy:
smaller tables are left whole.

**`--skip-add-locks` on the ranges**: the 1M-row table cut in 8, restored on 8 clients, 6.65 to 9.30 s; the same
ranges written with mysqldump's `LOCK TABLES … WRITE` around their rows, 16.28 to 29.63 s (the ranges then wait
for each other). Taken at a load of 9 to 10, above the bar of the other numbers: the gap is far larger than the
noise.

## Reproducing

```sh
scripts/test-env.sh up          # the kerno-* containers
scripts/bench.sh 5 30           # on a quiet machine, the Kerno window left visible
scripts/bench.sh 5 --only wide  # one step alone: core, wide, navigator, import, reconnect
scripts/bench.sh --report target/bench-raw.tsv  # the last run's table again (exit 1 on a miss)
scripts/bundle-macos.sh         # then, for the bundled idle figure, the first_rows step on
                                # dist/Kerno.app/Contents/MacOS/kerno (KERNO_BENCH=first_rows, see bench.sh)
```
