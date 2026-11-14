# Kerno — measured performance

The targets are those of CLAUDE.md §5. Every number below was produced by `scripts/bench.sh` (or, where said,
by one of its steps run alone) on the release build of commit `97a05b8`, the final code of milestone 10.

## Verdicts

| Target (CLAUDE.md §5) | Measured (median, min–max) | Verdict |
|---|---|---|
| Cold start to first interactive frame ≤ 400 ms | 279 ms (235–698), 30 launches | met |
| Idle RSS, one connection and one query tab, ≤ 150 MB | 148.4 MB (148.1–148.6) in `Kerno.app`; 150.5 MB (149.3–151.6) for the bare binary | met for the app; the bare binary is 0.5 MB over (see below) |
| First rows visible ≤ 150 ms after the server starts sending | 3.1 ms PostgreSQL (0.6–6.1), 2.4 ms MySQL (0.2–2.9) | met |
| Scrolling 1,000,000 rows: frame time under 16 ms on average | 8.35 ms vertical, 8.33 ms horizontal (120 Hz display) | met |
| Memory for 1,000,000 rows × 20 mixed columns fetched in full: report it | 340 MB for the values, 17 bytes per cell; process RSS 490 MB | reported |

## Machine and build

- Apple M3 Pro (11 cores), 18 GB, macOS 27.0.1, built-in 120 Hz display.
- `rustc 1.99.1`, `cargo build --release`, GPUI with runtime-compiled Metal shaders (D-002: the Metal
  toolchain is not installed on this machine; build-time shaders would remove their compilation from the
  start-up path).
- Servers: the `kerno-postgres` (PostgreSQL 17) and `kerno-mysql` (MySQL 8.4) containers on localhost, with
  the fixture table `analytics.big_table` / `kerno_analytics.big_table`: 1,000,000 rows × 20 columns
  (integers, numerics, text, a 32-character hash, timestamps, booleans, JSON, UUID).
- 1-minute load average during the runs: 2.6 to 3.9 (the script refuses above 4). Nothing else of Kerno was
  compiling or running.

## Method

`scripts/bench.sh [runs] [startup-runs]` builds the release binary and runs it in its bench mode
(`KERNO_BENCH`, `crates/kerno/src/bench.rs`), each run on a fresh copy of a scratch data directory with the
in-memory secret store, so the user's own Kerno data and keychain are never touched. One untimed launch comes
first (a freshly built binary pays the system's one-time checks). The update check is off in bench mode.
Five runs per measure, thirty for the start-up, which is short and varies the most; the table gives the
median and the range.

- **Start-up** (`KERNO_BENCH=startup`): from spawning the process to the second frame drawn. The first
  frame is on screen after 160 ms (141–529) from `main`; the app is counted interactive at the next frame,
  after macOS has activated it: 269 ms from `main`, 279 ms from the spawn. Kerno's own work before the first
  frame is about 20 ms; the rest is the platform (process start, AppKit, the font list, the window and the
  Metal renderer).
- **Idle memory and first rows** (`KERNO_BENCH=first_rows`): the app connects, opens one query tab, waits
  until it is idle, and samples its RSS and its footprint (`idle_rss_mb`, `idle_footprint_mb`: 202 MB of
  footprint, most of it the window's surfaces, which RSS does not count). It then runs
  `SELECT * FROM analytics.big_table` and times, from the arrival of the column descriptions (the server has
  started sending), to the frame that shows the first page of rows.
- **Full fetch**: "Fetch all" on that result, to the last row: 1.50 s on PostgreSQL (1.42–2.44), 1.43 s on
  MySQL (1.31–1.81), about 670,000 rows per second. The values are held in compact batches, not a `String`
  per cell: 340,098,183 bytes for 20,000,000 cells on PostgreSQL, 335,721,274 on MySQL.
- **Scroll** (`KERNO_BENCH=scroll`): with the million rows in the grid, the bench scrolls 10 seconds
  vertically (about 835 rows per frame) and 3 seconds horizontally and records the interval between frames:
  average 8.35 ms, median 8.33 ms, 95th percentile 8.39 ms, 99th percentile 8.42 ms; the longest single
  interval of a run was between 8.4 and 78 ms (one frame of a run, when it happens).
- **Sort** (no target): a click on the header of a text column of the million rows, on a result the server
  does not order (a `UNION ALL`, so that the sort is Kerno's): 279 ms (277–283), off the UI thread.
- The bench refuses to sample a window that is hidden, minimised or on another Space: macOS then draws no
  frame and the figures mean nothing.

## Idle memory: two figures, and why

`scripts/bench.sh` runs the bare binary, `target/release/kerno`: **150.5 MB**, half a megabyte over the
target. The same binary started from `Kerno.app` (`dist/Kerno.app/Contents/MacOS/kerno`, the `first_rows`
step alone, five runs): **148.4 MB**.

The difference is the Dock icon. Outside a bundle Kerno sets its icon itself so the Dock and the app switcher
show it (an owner's requirement of 2026-11-13); AppKit then keeps the icon rendered and its decoding touches
framework code, about 2 MB that a bundled app does not pay because the system reads the bundle's icon. That
same call cost 24 MB when it used the full-size icon: idle RSS had gone from 138 MB in the morning to
170 MB, and came back to these figures once the icon was a 256 px drawing shown at 128 points (D-053).

What a user runs is the bundle, so the verdict above is "met" for the app and says plainly that the bare
binary is not under the line. What is left that could be recovered: a 16 MB region the system allocator
keeps after the editor's syntax highlighter compiled its query (tree-sitter's scratch memory, freed by the
library but not returned to the system). Returning it needs a second `unsafe` call, which the workspace does
not allow (D-042, D-053).

## History of the figures

| When | Commit | Start-up | Idle RSS | First rows | Scroll | 1M × 20 |
|---|---|---|---|---|---|---|
| Morning, busy machine (indicative) | `2b0f3bf` | 452–676 ms | 122–162 MB | 3–4 ms | 8.4 ms | 340 MB |
| Afternoon, 5 start-up runs | `4777368` | 439 ms (389–990) | 138 MB | 2–5 ms | 8.4 ms | 340 MB |
| Afternoon, 30 start-up runs | `4777368` | 341 ms (293–713) | — | — | — | — |
| Evening, before the icon fix | `949d5a3` | 270 ms (251–415) | 170 MB | 1–4 ms | 8.4 ms | 340 MB |
| Final | `97a05b8` | 279 ms (235–698) | 150.5 MB bare, 148.4 MB bundled | 2–7 ms | 8.3 ms | 340 MB |

The start-up needs many runs to be read: with five, one slow launch moves the median by a hundred
milliseconds. The slow launches (the maximum of each row) are the first ones after other work on the
machine.

## Cases competitors fail (CLAUDE.md §4 J9)

What users of other clients report as slow or stuck, measured the same way (`KERNO_BENCH` modes, a scratch
data directory, the in-memory secret store, the update check off). Each step runs alone with
`scripts/bench.sh --only wide|navigator|import|reconnect`, or with every other measure when `--only` is left
out. The steps create their scratch objects, `kerno_bench_*` databases on `kerno-postgres` and `kerno-mysql`,
and drop them, pass or fail (an exit trap, after Kerno quit); the reconnect step runs a server of its own.

**The numbers below are indicative**: one run of each step with `--indicative`, on a machine where three
other agents were compiling and testing (1-minute load 5 to 21, the script's limit is 4), release build of
commit `1f5161b` plus the uncommitted M11 work. The final numbers are taken on a quiet machine (M11 Part 16)
and replace this table.

| Case (demand) | Target | Indicative | Verdict |
|---|---|---|---|
| Scroll a 300-column table (Beekeeper #752) | average frame < 16 ms | PostgreSQL 8.36 ms down, 8.36 ms across; MySQL 8.34 ms down, 8.57 ms across | met |
| Open a database of 200 schemas (Beekeeper #135) | < 300 ms to the frame showing them | PostgreSQL 23 ms; MySQL (a database of 1,200 tables, to its folders) 38 ms | met |
| Open a schema of 1,200 tables (Beekeeper #135) | < 500 ms to the frame showing them | PostgreSQL 247 ms (schema, its objects read) + 4 ms (Tables); MySQL 6 ms (Tables) | met |
| Run a 200 MB `.sql` file (Beekeeper #702, #2325) | UI responsive, progress, cancel | 15.3 s, 13.3 MB/s; frames 8.33 ms on average, longest 10.9 ms; cancelled at 30 %: stopped in 114 ms | met |
| Network drops 10 s and returns (DBeaver #6903) | usable < 5 s after the network is back | the statement in flight finished 112 ms *before* `docker network connect` returned; no failed run | met |

- **wide** (`KERNO_BENCH=scroll` on another table): `kerno_bench_wide.wide`, 300 columns plus a key ×
  20,000 rows: PostgreSQL cycles int, bigint, numeric, text, timestamp and boolean; MySQL int, bigint,
  decimal, varchar(20), date and TEXT (300 VARCHAR(255) would pass the 65,535-byte row limit). The scroll
  mode fetches everything (20,000 × 301 cells: 274 ms and 74.5 MB on PostgreSQL, 246 ms and 84.0 MB on MySQL)
  and scrolls 10 s down (17 rows a frame) and 3 s across, one column a frame, both ways; the longest single
  interval was 25 ms (PostgreSQL, down) and 72 ms (MySQL, across), one frame each.
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
  show as long ones. The first 0.24 s is the scan of the file for client commands, before `psql` starts.
  A second run cancels at 30 % of the file and times the stop (the client killed, the feeding stopped).
  The MySQL side of this step is not measured (same code path, `mysql` reads its standard input).
- **reconnect** (`KERNO_BENCH=reconnect`): a dedicated server, `kerno-bench-pg` (postgres:17) on its own
  network `kerno-bench-net`, host port 55440, created and removed by the step; the shared containers are
  never touched. The app runs `SELECT 1` in a query tab every 500 ms; once one succeeded the script runs
  `docker network disconnect`, waits 10 s, `docker network connect` and notes the wall clock; the app reports
  the wall clock of the first statement that succeeds after one was held up (more than 2 s) or failed, and
  the step prints the difference. With OrbStack (Docker 29.8) the published port comes back with the
  network and the client's TCP connection survives: the statement sent during the outage waits and
  completes as the network returns, before the `docker` command itself does, so the difference is
  negative. Kerno's own reconnect (a keep-alive every 30 s, then one new connection) never had to act; the
  connection's status stayed "Connected". `docker pause` was not needed.

## Reproducing

```sh
scripts/test-env.sh up          # the kerno-* containers
scripts/bench.sh 5 30           # on a quiet machine, the Kerno window left visible
scripts/bench.sh 5 --only wide  # one step alone: core, wide, navigator, import, reconnect
scripts/bundle-macos.sh         # then, for the bundled idle figure, the first_rows step on
                                # dist/Kerno.app/Contents/MacOS/kerno (KERNO_BENCH=first_rows, see bench.sh)
```
