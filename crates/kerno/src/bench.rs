//! The bench mode, behind `KERNO_BENCH` (milestone 9): measures startup,
//! the first rows of a large result, fetching it in full, sorting it and
//! scrolling it, prints one JSON line per measure on stdout, and quits.
//! Without the variable nothing here runs and nothing is measured: every
//! hook returns after one atomic load.
//!
//! - `KERNO_BENCH=seed`: saves the connection `KERNO_BENCH_CONNECTION` from
//!   `KERNO_BENCH_URL` (no password in it) into the data directory; no window.
//! - `KERNO_BENCH=startup`: from the top of `main` to the first frame drawn
//!   and to the next one (the event loop has turned: input is handled).
//! - `KERNO_BENCH=first_rows`: connects to `KERNO_BENCH_CONNECTION` with
//!   `KERNO_BENCH_PASSWORD`, runs `KERNO_BENCH_SQL` in a query tab: statement
//!   sent, columns, first rows, first frame with rows; then fetches all,
//!   reports memory, and sorts the result by its second column.
//! - `KERNO_BENCH=scroll`: the same up to fetch all, then scrolls the grid
//!   top to bottom for 10 s and across its columns for 3 s, one step per
//!   frame.
//!
//! Bench modes refuse to run without `KERNO_DATA_DIR` and
//! `KERNO_SECRET_STORE=memory`: they never touch the user's data or keychain.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::component::table::{ColumnSort, TableDelegate as _, TableState};
use gpui_kit::{App, Entity, Global, Window};
use kerno_core::config::{ConnectionConfig, Secret, Secrets, parse_connection_url};
use kerno_core::storage::{AppPaths, Storage};
use serde_json::json;

use crate::connections::manager::{Connections, Status};
use crate::query::grid::Grid;
use crate::query::tab::Scope;
use crate::state::AppState;
use crate::tabs::Target;
use crate::workspace::Workspace;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Startup,
    FirstRows,
    Scroll,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Startup => "startup",
            Mode::FirstRows => "first_rows",
            Mode::Scroll => "scroll",
        }
    }
}

/// The mode and the instant `main` started; set only in bench mode.
static BENCH: OnceLock<(Mode, Instant)> = OnceLock::new();
/// The bench gives up after this long.
const WATCHDOG: Duration = Duration::from_secs(180);
const VERTICAL_PASS: Duration = Duration::from_secs(10);
const HORIZONTAL_PASS: Duration = Duration::from_secs(3);

/// Called first thing in `main`. In seed mode, seeds and exits.
pub fn init() {
    let started = Instant::now();
    let Ok(mode) = std::env::var("KERNO_BENCH") else {
        return;
    };
    let mode = match mode.as_str() {
        "seed" => std::process::exit(match seed() {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("kerno bench seed: {error:#}");
                2
            }
        }),
        "startup" => Mode::Startup,
        "first_rows" => Mode::FirstRows,
        "scroll" => Mode::Scroll,
        other => refuse(&format!("unknown KERNO_BENCH mode {other:?}")),
    };
    if let Err(why) = scratch_only() {
        refuse(why);
    }
    let _ = BENCH.set((mode, started));
}

fn refuse(why: &str) -> ! {
    eprintln!("kerno bench: {why}");
    std::process::exit(2)
}

/// The bench runs on a scratch data directory and in-memory secrets only.
fn scratch_only() -> Result<(), &'static str> {
    if std::env::var_os("KERNO_DATA_DIR").is_none_or(|dir| dir.is_empty()) {
        return Err("KERNO_DATA_DIR must name a scratch data directory");
    }
    if std::env::var("KERNO_SECRET_STORE").as_deref() != Ok("memory") {
        return Err("KERNO_SECRET_STORE=memory is required");
    }
    Ok(())
}

fn env(name: &str) -> anyhow::Result<String> {
    std::env::var(name).map_err(|_| anyhow::anyhow!("{name} is not set"))
}

/// Saves the bench connection, unless one of that name is there.
fn seed() -> anyhow::Result<()> {
    scratch_only().map_err(anyhow::Error::msg)?;
    let name = env("KERNO_BENCH_CONNECTION")?;
    let url = parse_connection_url(&env("KERNO_BENCH_URL")?)?;
    anyhow::ensure!(
        url.password.is_none(),
        "KERNO_BENCH_URL must not hold a password"
    );
    let storage = Storage::open(&AppPaths::locate()?.database())?;
    if storage
        .connections()?
        .iter()
        .all(|saved| saved.config.name != name)
    {
        let mut config = ConnectionConfig::new(url.engine);
        config.name = name.clone();
        config.host = url.host.unwrap_or(config.host);
        config.port = url.port.unwrap_or(config.port);
        config.user = url.user.unwrap_or(config.user);
        config.database = url.database;
        config.options = url.options;
        if let Some(mode) = url.tls_mode {
            config.tls.mode = mode;
        }
        storage.save_connection(None, &config)?;
    }
    println!("{}", json!({"bench": "seed", "connection": name}));
    Ok(())
}

/// What the hooks recorded.
#[derive(Default)]
struct Marks {
    sent: Option<Instant>,
    columns: Option<Instant>,
    rows: Option<Instant>,
    frame: Option<Instant>,
    complete: Option<Instant>,
    sorted: Option<Instant>,
}

struct Bench(Marks);

impl Global for Bench {}

fn active() -> bool {
    BENCH.get().is_some()
}

/// Records the first time of a mark.
fn mark(cx: &mut App, which: fn(&mut Marks) -> &mut Option<Instant>) {
    if active() && cx.has_global::<Bench>() {
        which(&mut cx.global_mut::<Bench>().0).get_or_insert_with(Instant::now);
    }
}

/// The first statement of the bench's run was handed to the driver.
pub fn sent(cx: &mut App) {
    mark(cx, |marks| &mut marks.sent);
}

/// Its columns arrived: the server has started answering.
pub fn columns(cx: &mut App) {
    mark(cx, |marks| &mut marks.columns);
}

/// A batch of rows arrived; the first one is timed up to its frame.
pub fn rows(window: &Window, cx: &mut App) {
    if !active() || !cx.has_global::<Bench>() || cx.global::<Bench>().0.rows.is_some() {
        return;
    }
    mark(cx, |marks| &mut marks.rows);
    window.on_next_frame(|_, cx| mark(cx, |marks| &mut marks.frame));
}

/// The statement has no more rows.
pub fn complete(cx: &mut App) {
    mark(cx, |marks| &mut marks.complete);
}

/// A client-side sort is in place.
pub fn sorted(cx: &mut App) {
    mark(cx, |marks| &mut marks.sorted);
}

/// Set while scrolling is measured: cells mark when a frame starts drawing.
static SCROLLING: AtomicBool = AtomicBool::new(false);
/// Nanoseconds after `main` of the first cell drawn in the current frame;
/// 0 when none was.
static FRAME_START: AtomicU64 = AtomicU64::new(0);

/// A grid cell is being drawn.
pub fn cell() {
    if SCROLLING.load(Ordering::Relaxed)
        && FRAME_START.load(Ordering::Relaxed) == 0
        && let Some((_, main)) = BENCH.get()
    {
        FRAME_START.store(main.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
}

/// Starts the bench once the window and its workspace exist.
pub fn begin(
    workspace: &Entity<Workspace>,
    connections: &Entity<Connections>,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(&(mode, main)) = BENCH.get() else {
        return;
    };
    cx.set_global(Bench(Marks::default()));
    cx.spawn(async move |cx| {
        cx.background_executor().timer(WATCHDOG).await;
        cx.update(|cx| fail(mode, "timed out", cx));
    })
    .detach();
    match mode {
        Mode::Startup => window.on_next_frame(move |window, _| {
            let first = main.elapsed();
            window.on_next_frame(move |_, cx| {
                let interactive = main.elapsed();
                report(json!({
                    "bench": "startup",
                    "main_to_first_frame_ms": ms(first),
                    "main_to_interactive_ms": ms(interactive),
                }));
                cx.quit();
            });
        }),
        Mode::FirstRows | Mode::Scroll => {
            let (workspace, connections) = (workspace.clone(), connections.clone());
            window
                .spawn(cx, async move |cx| {
                    if let Err(error) = run(mode, workspace, connections, cx).await {
                        let _ = cx.update(|_, cx| fail(mode, &format!("{error:#}"), cx));
                    }
                })
                .detach();
        }
    }
}

fn ms(duration: Duration) -> f64 {
    (duration.as_secs_f64() * 1e6).round() / 1e3
}

fn between(from: Option<Instant>, to: Option<Instant>) -> Option<f64> {
    Some(ms(to?.checked_duration_since(from?)?))
}

fn report(line: serde_json::Value) {
    println!("{line}");
}

fn fail(mode: Mode, error: &str, cx: &mut App) {
    report(json!({"bench": mode.name(), "error": error}));
    cx.quit();
}

/// Resident memory of this process, in MB.
fn rss_mb() -> Option<f64> {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    let kb: f64 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
    Some((kb / 1024.0 * 10.0).round() / 10.0)
}

/// Polls `done` every few milliseconds.
async fn until(
    cx: &mut gpui_kit::AsyncWindowContext,
    mut done: impl FnMut(&mut App) -> anyhow::Result<bool>,
) -> anyhow::Result<()> {
    loop {
        if cx.update(|_, cx| done(cx))?? {
            return Ok(());
        }
        cx.background_executor()
            .timer(Duration::from_millis(2))
            .await;
    }
}

fn marks(cx: &App) -> &Marks {
    &cx.global::<Bench>().0
}

async fn run(
    mode: Mode,
    workspace: Entity<Workspace>,
    connections: Entity<Connections>,
    cx: &mut gpui_kit::AsyncWindowContext,
) -> anyhow::Result<()> {
    let name = env("KERNO_BENCH_CONNECTION")?;
    let sql = env("KERNO_BENCH_SQL")?;
    // Read once, put in the in-memory store, never printed or logged.
    let password = std::env::var("KERNO_BENCH_PASSWORD").ok().map(Secret::new);

    // Connect, then let the navigator settle before anything is timed.
    let id = cx.update(|_, cx| {
        let saved = connections.read(cx).saved();
        let id = saved
            .iter()
            .find(|saved| saved.config.name == name)
            .map(|saved| saved.id.clone())
            .ok_or_else(|| anyhow::anyhow!("no saved connection named {name:?}"))?;
        let secrets = Secrets {
            password,
            ..Secrets::default()
        };
        kerno_core::secrets::save_secrets(AppState::get(cx).secrets.as_ref(), &id, &secrets)?;
        connections.update(cx, |connections, cx| connections.connect(&id, cx));
        anyhow::Ok(id)
    })??;
    until(cx, |cx| match connections.read(cx).status(&id) {
        Status::Connected { .. } => Ok(true),
        Status::Failed { message } => anyhow::bail!("the connection failed: {message}"),
        Status::WaitingForInput => anyhow::bail!("the connection asks for input"),
        _ => Ok(false),
    })
    .await?;
    let target = Target {
        connection: id,
        database: None,
        schema: None,
    };
    let tab = cx.update(|window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.open_query("Bench".into(), &sql, Some(target), window, cx)
        })
    })?;
    cx.background_executor().timer(Duration::from_secs(2)).await;
    let idle_rss = rss_mb();

    cx.update(|window, cx| tab.update(cx, |tab, cx| tab.run(Scope::All, window, cx)))?;
    until(cx, |cx| Ok(marks(cx).frame.is_some())).await?;
    let first = cx.update(|_, cx| {
        let marks = marks(cx);
        json!({
            "bench": "first_rows",
            "connection": name,
            "sent_to_columns_ms": between(marks.sent, marks.columns),
            "columns_to_rows_ms": between(marks.columns, marks.rows),
            "rows_to_frame_ms": between(marks.rows, marks.frame),
            "columns_to_frame_ms": between(marks.columns, marks.frame),
            "sent_to_frame_ms": between(marks.sent, marks.frame),
            "idle_rss_mb": idle_rss,
        })
    })?;
    report(first);

    // Fetch all.
    let grid = cx
        .update(|_, cx| tab.read(cx).grid_of(0))?
        .ok_or_else(|| anyhow::anyhow!("the statement returned no rows"))?;
    let rss_before = rss_mb();
    let fetching = Instant::now();
    cx.update(|_, cx| grid.update(cx, |table, _| table.delegate_mut().fetch_all()))?;
    until(cx, |cx| Ok(marks(cx).complete.is_some())).await?;
    let fetched = cx.update(|_, cx| marks(cx).complete)?;
    // One more frame with every row, then measure.
    cx.background_executor()
        .timer(Duration::from_millis(200))
        .await;
    let (rows, columns, memory) = cx.update(|_, cx| {
        let store = grid.read(cx).delegate().store();
        (
            store.row_count(),
            store.columns().len(),
            store.memory_bytes(),
        )
    })?;
    let fetch_ms = between(Some(fetching), fetched);
    report(json!({
        "bench": "fetch_all",
        "rows": rows,
        "columns": columns,
        "fetch_all_ms": fetch_ms,
        "rows_per_s": fetch_ms.map(|ms| (rows as f64 / ms * 1000.0).round()),
        "rss_before_mb": rss_before,
        "rss_after_mb": rss_mb(),
        "memory_bytes": memory,
        "bytes_per_cell": (memory as f64 / (rows * columns).max(1) as f64 * 10.0).round() / 10.0,
    }));

    match mode {
        Mode::Scroll => {
            cx.update(|window, _| scroll(grid, rows, columns, window))?;
        }
        _ => {
            // Sort by the fourth column (`hash`, 32 characters of text, in
            // `analytics.big_table`), as a click on its header does; grid
            // column 0 numbers the rows.
            const SORTED: usize = 4;
            let sorting = Instant::now();
            let column = cx.update(|window, cx| {
                grid.update(cx, |table, cx| {
                    let grid = table.delegate_mut();
                    let column = grid
                        .source_column(SORTED)
                        .map(|source| grid.store().columns()[source].name.clone());
                    grid.perform_sort(SORTED, ColumnSort::Ascending, window, cx);
                    column
                })
            })?;
            until(cx, |cx| Ok(marks(cx).sorted.is_some())).await?;
            let sorted = cx.update(|_, cx| marks(cx).sorted)?;
            report(json!({
                "bench": "sort",
                "rows": rows,
                "column": column,
                "sort_ms": between(Some(sorting), sorted),
            }));
            cx.update(|_, cx| cx.quit())?;
        }
    }
    Ok(())
}

/// Frame samples of one scroll pass.
struct Pass {
    name: &'static str,
    started: Instant,
    last: Instant,
    intervals: Vec<f64>,
    work: Vec<f64>,
    steps: usize,
}

impl Pass {
    fn new(name: &'static str) -> Self {
        let now = Instant::now();
        Self {
            name,
            started: now,
            last: now,
            intervals: Vec::new(),
            work: Vec::new(),
            steps: 0,
        }
    }

    /// Records the frame just drawn.
    fn frame(&mut self) {
        let now = Instant::now();
        let start = FRAME_START.swap(0, Ordering::Relaxed);
        if self.steps > 0 {
            self.intervals.push(ms(now - self.last));
            if start > 0
                && let Some((_, main)) = BENCH.get()
            {
                let drawn = main.elapsed().as_nanos() as u64;
                self.work
                    .push(ms(Duration::from_nanos(drawn.saturating_sub(start))));
            }
        }
        self.last = now;
        self.steps += 1;
    }

    fn report(mut self, moved: usize) {
        let interval = Summary::of(&mut self.intervals);
        report(json!({
            "bench": "scroll",
            "pass": self.name,
            "frames": interval.count,
            "moved_per_frame": (moved as f64 / interval.count.max(1) as f64).round(),
            // The display paces frames: the median interval shows its rate.
            "refresh_hz": (1000.0 / interval.p50).round(),
            "interval_ms": interval.json(),
            "work_ms": Summary::of(&mut self.work).json(),
        }));
    }
}

/// Scrolls the grid, one step per frame: from the first row to the last
/// in [`VERTICAL_PASS`] whatever the frame rate, then across the columns
/// and back.
fn scroll(grid: Entity<TableState<Grid>>, rows: usize, columns: usize, window: &mut Window) {
    SCROLLING.store(true, Ordering::Relaxed);
    FRAME_START.store(0, Ordering::Relaxed);
    let pass = Pass::new("vertical");
    window.on_next_frame(move |window, cx| vertical(grid, rows, columns, pass, window, cx));
}

fn vertical(
    grid: Entity<TableState<Grid>>,
    rows: usize,
    columns: usize,
    mut pass: Pass,
    window: &mut Window,
    cx: &mut App,
) {
    pass.frame();
    let done = pass.started.elapsed().as_secs_f64() / VERTICAL_PASS.as_secs_f64();
    if done >= 1.0 {
        pass.report(rows);
        grid.update(cx, |table, cx| table.scroll_to_row(rows / 2, cx));
        let pass = Pass::new("horizontal");
        window.on_next_frame(move |window, cx| horizontal(grid, columns, pass, window, cx));
        return;
    }
    let row = (rows as f64 * done) as usize;
    grid.update(cx, |table, cx| table.scroll_to_row(row, cx));
    window.on_next_frame(move |window, cx| vertical(grid, rows, columns, pass, window, cx));
}

fn horizontal(
    grid: Entity<TableState<Grid>>,
    columns: usize,
    mut pass: Pass,
    window: &mut Window,
    cx: &mut App,
) {
    pass.frame();
    if pass.started.elapsed() >= HORIZONTAL_PASS {
        SCROLLING.store(false, Ordering::Relaxed);
        let steps = pass.steps;
        pass.report(steps);
        cx.quit();
        return;
    }
    // Across and back; column 0 is the row number.
    let span = columns.max(1);
    let turn = pass.steps % (2 * span);
    let column = 1 + if turn < span {
        turn
    } else {
        2 * span - 1 - turn
    };
    grid.update(cx, |table, cx| table.scroll_to_col(column, cx));
    window.on_next_frame(move |window, cx| horizontal(grid, columns, pass, window, cx));
}

/// Count, mean and nearest-rank percentiles of samples, in ms.
#[derive(Debug, Default, PartialEq)]
struct Summary {
    count: usize,
    avg: f64,
    p50: f64,
    p95: f64,
    p99: f64,
    max: f64,
}

impl Summary {
    fn of(samples: &mut [f64]) -> Self {
        if samples.is_empty() {
            return Self::default();
        }
        samples.sort_by(f64::total_cmp);
        let rank = |p: f64| {
            samples[((p * samples.len() as f64).ceil() as usize).clamp(1, samples.len()) - 1]
        };
        Self {
            count: samples.len(),
            avg: samples.iter().sum::<f64>() / samples.len() as f64,
            p50: rank(0.50),
            p95: rank(0.95),
            p99: rank(0.99),
            max: samples[samples.len() - 1],
        }
    }

    fn json(&self) -> serde_json::Value {
        let round = |value: f64| (value * 1000.0).round() / 1000.0;
        json!({
            "avg": round(self.avg),
            "p50": round(self.p50),
            "p95": round(self.p95),
            "p99": round(self.p99),
            "max": round(self.max),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Summary;

    #[test]
    fn percentiles_are_nearest_rank() {
        let mut samples: Vec<f64> = (1..=100).rev().map(f64::from).collect();
        let summary = Summary::of(&mut samples);
        assert_eq!(summary.count, 100);
        assert_eq!(summary.avg, 50.5);
        assert_eq!((summary.p50, summary.p95, summary.p99), (50.0, 95.0, 99.0));
        assert_eq!(summary.max, 100.0);

        let mut one = vec![7.0];
        let summary = Summary::of(&mut one);
        assert_eq!((summary.p50, summary.p99, summary.max), (7.0, 7.0, 7.0));
        assert_eq!(Summary::of(&mut []), Summary::default());
    }
}
