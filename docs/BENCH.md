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

## Reproducing

```sh
scripts/test-env.sh up          # the kerno-* containers
scripts/bench.sh 5 30           # on a quiet machine, the Kerno window left visible
scripts/bundle-macos.sh         # then, for the bundled idle figure, the first_rows step on
                                # dist/Kerno.app/Contents/MacOS/kerno (KERNO_BENCH=first_rows, see bench.sh)
```
