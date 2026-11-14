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
//!   frame (with a 300-column table: `scripts/bench.sh --only wide`).
//!
//! Cases competitors fail (CLAUDE.md §4 J9), each connecting first:
//!
//! - `KERNO_BENCH=navigator`: expands `KERNO_BENCH_DATABASE` in the
//!   navigator, then `KERNO_BENCH_SCHEMA` (none on MySQL, where the database
//!   is the schema) and its Tables folder: time from each click to the
//!   frame that shows the rows below it.
//! - `KERNO_BENCH=import`: runs the SQL file `KERNO_BENCH_FILE` in
//!   `KERNO_BENCH_DATABASE` with the Execute script tool, recording the
//!   interval between frames (one asked for after every frame) and the
//!   bytes sent; with `KERNO_BENCH_CANCEL_AT` (a percentage) it cancels
//!   there and times the stop.
//! - `KERNO_BENCH=reconnect`: runs `KERNO_BENCH_SQL` in a query tab every
//!   500 ms; prints a `ready` line once one succeeded, then waits for a run
//!   held up or failed for more than 2 s (the network went away) and
//!   reports, as wall-clock milliseconds, when the next one succeeded.
//!
//! Bench modes refuse to run without `KERNO_DATA_DIR` and
//! `KERNO_SECRET_STORE=memory`: they never touch the user's data or keychain.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::OnceLock;
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
    Navigator,
    Import,
    Reconnect,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Startup => "startup",
            Mode::FirstRows => "first_rows",
            Mode::Scroll => "scroll",
            Mode::Navigator => "navigator",
            Mode::Import => "import",
            Mode::Reconnect => "reconnect",
        }
    }
}

/// The mode and the instant `main` started; set only in bench mode.
static BENCH: OnceLock<(Mode, Instant)> = OnceLock::new();
/// The bench gives up after this long.
const WATCHDOG: Duration = Duration::from_secs(180);
const VERTICAL_PASS: Duration = Duration::from_secs(10);
const HORIZONTAL_PASS: Duration = Duration::from_secs(3);
/// Why a bench waiting for a frame stops: macOS stops the display link of
/// a window that is not on screen, so no frame comes until it is shown
/// again, and the memory of a hidden window is not the same either (its
/// drawables are released).
const HIDDEN: &str = "the window is hidden (covered by another window, minimized, on another \
                      Space or on a sleeping display): no frame is drawn, nothing is measured";

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
        "navigator" => Mode::Navigator,
        "import" => Mode::Import,
        "reconnect" => Mode::Reconnect,
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

/// Whether the app runs in a bench mode.
pub(crate) fn active() -> bool {
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
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        cx.background_executor().timer(WATCHDOG).await;
        cx.update(|cx| {
            let hidden = handle
                .update(cx, |_, window, _| !window.is_visible())
                .unwrap_or(false);
            fail(mode, if hidden { HIDDEN } else { "timed out" }, cx)
        });
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
        _ => {
            let (workspace, connections) = (workspace.clone(), connections.clone());
            window
                .spawn(cx, async move |cx| {
                    let outcome = match mode {
                        Mode::Navigator => navigator(workspace, connections, cx).await,
                        Mode::Import => import(connections, cx).await,
                        Mode::Reconnect => reconnect(workspace, connections, cx).await,
                        _ => run(mode, workspace, connections, cx).await,
                    };
                    if let Err(error) = outcome {
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

/// Physical footprint of this process, in MB (macOS `footprint`): what the
/// process holds, compressed or swapped pages and GPU memory included. RSS
/// leaves out pages the system compressed, so on a busy machine it drops
/// with nothing freed.
fn footprint_mb() -> Option<f64> {
    let out = std::process::Command::new("footprint")
        .args(["-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    // "kerno [123]: 64-bit    Footprint: 199 MB (16384 bytes per page)"
    let text = String::from_utf8_lossy(&out.stdout);
    let mut words = text.split("Footprint:").nth(1)?.split_whitespace();
    let value: f64 = words.next()?.parse().ok()?;
    let mb = match words.next()? {
        "B" => value / 1048576.0,
        "KB" => value / 1024.0,
        "MB" => value,
        "GB" => value * 1024.0,
        _ => return None,
    };
    Some((mb * 10.0).round() / 10.0)
}

/// Polls `done` every few milliseconds.
async fn until(
    cx: &mut gpui_kit::AsyncWindowContext,
    mut done: impl FnMut(&mut Window, &mut App) -> anyhow::Result<bool>,
) -> anyhow::Result<()> {
    loop {
        if cx.update(|window, cx| done(window, cx))?? {
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

/// Puts the bench password in the in-memory store and connects the saved
/// connection `name`; its id once connected.
async fn connect(
    name: &str,
    connections: &Entity<Connections>,
    cx: &mut gpui_kit::AsyncWindowContext,
) -> anyhow::Result<String> {
    // Read once, put in the in-memory store, never printed or logged.
    let password = std::env::var("KERNO_BENCH_PASSWORD").ok().map(Secret::new);
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
    until(cx, |_, cx| match connections.read(cx).status(&id) {
        Status::Connected { .. } => Ok(true),
        Status::Failed { message } => anyhow::bail!("the connection failed: {message}"),
        Status::WaitingForInput => anyhow::bail!("the connection asks for input"),
        _ => Ok(false),
    })
    .await?;
    Ok(id)
}

async fn run(
    mode: Mode,
    workspace: Entity<Workspace>,
    connections: Entity<Connections>,
    cx: &mut gpui_kit::AsyncWindowContext,
) -> anyhow::Result<()> {
    let name = env("KERNO_BENCH_CONNECTION")?;
    let sql = env("KERNO_BENCH_SQL")?;
    let id = connect(&name, &connections, cx).await?;
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
    // A hidden window has released its drawables: its memory is not the
    // idle memory of the app on screen.
    anyhow::ensure!(cx.update(|window, _| window.is_visible())?, HIDDEN);
    let (idle_rss, idle_footprint) = (rss_mb(), footprint_mb());

    cx.update(|window, cx| tab.update(cx, |tab, cx| tab.run(Scope::All, window, cx)))?;
    until(cx, |window, cx| {
        anyhow::ensure!(window.is_visible(), HIDDEN);
        Ok(marks(cx).frame.is_some())
    })
    .await?;
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
            "idle_footprint_mb": idle_footprint,
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
    until(cx, |_, cx| Ok(marks(cx).complete.is_some())).await?;
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
        "footprint_after_mb": footprint_mb(),
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
            // A result edited like table data is ordered by the server:
            // there is no sort here to time. Whether it is one is found out
            // by a catalog query that may end after the last row.
            until(cx, |_, cx| Ok(tab.read(cx).edit_decided(0))).await?;
            if cx.update(|_, cx| grid.read(cx).delegate().sorts_on_server())? {
                report(json!({"bench": "sort", "rows": rows, "on_server": true}));
                cx.update(|_, cx| cx.quit())?;
                return Ok(());
            }
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
            until(cx, |_, cx| Ok(marks(cx).sorted.is_some())).await?;
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

/// Intervals between frames, one asked for after each frame, until `stop`.
#[derive(Default)]
struct Frames {
    last: Option<Instant>,
    intervals: Vec<f64>,
    stop: bool,
}

fn record_frames(frames: Rc<RefCell<Frames>>, window: &mut Window) {
    window.on_next_frame(move |window, _| {
        {
            let mut frames = frames.borrow_mut();
            if frames.stop {
                return;
            }
            let now = Instant::now();
            if let Some(last) = frames.last {
                frames.intervals.push(ms(now - last));
            }
            frames.last = Some(now);
        }
        window.refresh();
        record_frames(frames, window);
    });
}

/// Milliseconds since the Unix epoch: the bench script notes the moment
/// the network is back on the same clock.
fn epoch_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis())
}

/// `KERNO_BENCH=navigator`: see the module documentation.
async fn navigator(
    workspace: Entity<Workspace>,
    connections: Entity<Connections>,
    cx: &mut gpui_kit::AsyncWindowContext,
) -> anyhow::Result<()> {
    let name = env("KERNO_BENCH_CONNECTION")?;
    let database = env("KERNO_BENCH_DATABASE")?;
    let schema = std::env::var("KERNO_BENCH_SCHEMA").ok();
    let table = env("KERNO_BENCH_TABLE")?;
    let navigator = cx.update(|_, cx| workspace.read(cx).navigator(cx))?;
    let id = connect(&name, &connections, cx).await?;
    cx.update(|_, cx| navigator.update(cx, |navigator, cx| navigator.open_connection(&id, cx)))?;
    let shown = |path: &[&str], cx: &mut gpui_kit::AsyncWindowContext| {
        cx.update(|_, cx| navigator.read(cx).row_of(path, cx))
    };
    until(cx, |_, cx| {
        Ok(navigator.read(cx).row_of(&[&database], cx).is_some())
    })
    .await?;
    cx.background_executor().timer(Duration::from_secs(1)).await;
    anyhow::ensure!(cx.update(|window, _| window.is_visible())?, HIDDEN);

    // Opens the row at `path` and times the frame that shows `child`
    // under it.
    let mut open = async |path: Vec<&str>, child: &str| -> anyhow::Result<f64> {
        let row = shown(&path, cx)?.ok_or_else(|| anyhow::anyhow!("no row {path:?}"))?;
        let clicked = Instant::now();
        cx.update(|_, cx| {
            navigator.update(cx, |navigator, cx| {
                let id = navigator.id_at(row, cx);
                navigator.set_open(&id, true, cx)
            })
        })?;
        let below: Vec<&str> = path.iter().copied().chain([child]).collect();
        until(cx, |window, cx| {
            anyhow::ensure!(window.is_visible(), HIDDEN);
            Ok(navigator.read(cx).row_of(&below, cx).is_some())
        })
        .await?;
        let drawn = Rc::new(std::cell::Cell::new(None));
        let mark = drawn.clone();
        cx.update(|window, _| {
            window.on_next_frame(move |_, _| mark.set(Some(Instant::now())));
            window.refresh();
        })?;
        until(cx, |_, _| Ok(drawn.get().is_some())).await?;
        Ok(between(Some(clicked), drawn.get()).unwrap_or_default())
    };
    let report_line = match &schema {
        Some(schema) => {
            let database_ms = open(vec![&database], schema).await?;
            let schema_ms = open(vec![&database, schema], "Tables").await?;
            let tables_ms = open(vec![&database, schema, "Tables"], &table).await?;
            json!({
                "bench": "navigator",
                "connection": name,
                "database_ms": database_ms,
                "schema_ms": schema_ms,
                "tables_ms": tables_ms,
            })
        }
        None => {
            let database_ms = open(vec![&database], "Tables").await?;
            let tables_ms = open(vec![&database, "Tables"], &table).await?;
            json!({
                "bench": "navigator",
                "connection": name,
                "database_ms": database_ms,
                "tables_ms": tables_ms,
            })
        }
    };
    report(report_line);
    cx.update(|_, cx| cx.quit())?;
    Ok(())
}

/// `KERNO_BENCH=import`: see the module documentation.
async fn import(
    connections: Entity<Connections>,
    cx: &mut gpui_kit::AsyncWindowContext,
) -> anyhow::Result<()> {
    let name = env("KERNO_BENCH_CONNECTION")?;
    let database = env("KERNO_BENCH_DATABASE")?;
    let file = std::path::PathBuf::from(env("KERNO_BENCH_FILE")?);
    let cancel_at: Option<f64> = match std::env::var("KERNO_BENCH_CANCEL_AT") {
        Ok(text) => Some(text.parse()?),
        Err(_) => None,
    };
    let id = connect(&name, &connections, cx).await?;
    let target = Target {
        connection: id,
        database: Some(database),
        schema: None,
    };
    let dialog = cx
        .update(|window, cx| {
            crate::backup_dialog::open(
                connections.clone(),
                target,
                crate::backup_dialog::Mode::ExecuteScript,
                window,
                cx,
                |_, _, _, _| {},
            )
        })?
        .ok_or_else(|| anyhow::anyhow!("the Execute script wizard did not open"))?;
    cx.update(|window, cx| dialog.update(cx, |dialog, cx| dialog.load_input(file, window, cx)))?;
    // The client found and the file read, it starts.
    let waiting = Instant::now();
    loop {
        let started = cx.update(|window, cx| {
            dialog.update(cx, |dialog, cx| dialog.run_unconfirmed(window, cx))
        })?;
        match started {
            Ok(()) => break,
            Err(why) if waiting.elapsed() > Duration::from_secs(60) => {
                anyhow::bail!("the script did not start: {why}")
            }
            Err(_) => {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await
            }
        }
    }
    let started = Instant::now();
    let frames = Rc::new(RefCell::new(Frames::default()));
    cx.update(|window, _| record_frames(frames.clone(), window))?;
    let (mut first_byte, mut cancelled) = (None, None);
    let (success, text, sent, total) = loop {
        let (fed, ended) = cx.update(|window, cx| {
            anyhow::ensure!(window.is_visible(), HIDDEN);
            let dialog = dialog.read(cx);
            Ok((
                dialog.fed(),
                dialog.ended().map(|(ok, text)| (ok, text.to_string())),
            ))
        })??;
        let (sent, total) = fed.unwrap_or_default();
        if first_byte.is_none() && sent > 0 {
            first_byte = Some(started.elapsed());
        }
        if let Some((success, text)) = ended {
            break (success, text, sent, total);
        }
        if let Some(at) = cancel_at
            && cancelled.is_none()
            && total > 0
            && sent as f64 >= total as f64 * at / 100.0
        {
            cx.update(|_, cx| dialog.update(cx, |dialog, cx| dialog.cancel(cx)))?;
            cancelled = Some((Instant::now(), sent));
        }
        cx.background_executor()
            .timer(Duration::from_millis(10))
            .await;
    };
    let ended = Instant::now();
    frames.borrow_mut().stop = true;
    let interval = Summary::of(&mut frames.borrow_mut().intervals);
    let feeding = first_byte.map(|first| (ended - started).saturating_sub(first));
    report(json!({
        "bench": "import",
        "connection": name,
        "success": success,
        "outcome": text,
        "file_bytes": total,
        "sent_bytes": sent,
        "total_ms": ms(ended - started),
        // Reading the file for client commands, before the client starts.
        "check_ms": first_byte.map(ms),
        "mb_per_s": feeding
            .filter(|_| cancelled.is_none())
            .map(|feeding| (sent as f64 / 1e6 / feeding.as_secs_f64() * 10.0).round() / 10.0),
        "cancelled_at_bytes": cancelled.map(|(_, sent)| sent),
        "cancel_ms": cancelled.map(|(at, _)| ms(ended - at)),
        "frames": interval.count,
        "interval_ms": interval.json(),
    }));
    cx.update(|_, cx| cx.quit())?;
    Ok(())
}

/// `KERNO_BENCH=reconnect`: see the module documentation.
async fn reconnect(
    workspace: Entity<Workspace>,
    connections: Entity<Connections>,
    cx: &mut gpui_kit::AsyncWindowContext,
) -> anyhow::Result<()> {
    const EVERY: Duration = Duration::from_millis(500);
    const HELD: Duration = Duration::from_secs(2);
    let name = env("KERNO_BENCH_CONNECTION")?;
    let sql = env("KERNO_BENCH_SQL")?;
    let id = connect(&name, &connections, cx).await?;
    let target = Target {
        connection: id.clone(),
        database: None,
        schema: None,
    };
    let tab = cx.update(|window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.open_query("Bench".into(), &sql, Some(target), window, cx)
        })
    })?;
    let (mut ready, mut outage, mut failures, mut longest) = (false, None, 0, Duration::ZERO);
    let mut statuses: Vec<String> = Vec::new();
    loop {
        // One run: started, then ended with a new result (its duration
        // changes) or an error (no rows on screen).
        let before = cx.update(|_, cx| tab.read(cx).status(cx).elapsed)?;
        let started = Instant::now();
        cx.update(|window, cx| tab.update(cx, |tab, cx| tab.run(Scope::All, window, cx)))?;
        let success = loop {
            cx.background_executor()
                .timer(Duration::from_millis(10))
                .await;
            let (status, connection) =
                cx.update(|_, cx| (tab.read(cx).status(cx), connections.read(cx).status(&id)))?;
            let connection = format!("{connection:?}");
            let connection = connection.split([' ', '{', '(']).next().unwrap_or_default();
            if statuses
                .last()
                .is_none_or(|last| !last.ends_with(connection))
            {
                statuses.push(format!("{} {connection}", epoch_ms()));
            }
            if ready && outage.is_none() && started.elapsed() > HELD {
                outage = Some(epoch_ms());
            }
            if !status.running && (status.elapsed != before || status.rows.is_none()) {
                break status.rows == Some(1) && status.elapsed != before;
            }
        };
        longest = longest.max(started.elapsed());
        match (success, ready, outage) {
            (true, false, _) => {
                ready = true;
                report(json!({"bench": "reconnect", "phase": "ready"}));
            }
            (true, true, Some(outage)) => {
                report(json!({
                    "bench": "reconnect",
                    "connection": name,
                    "recovered_epoch_ms": epoch_ms(),
                    "outage_seen_epoch_ms": outage,
                    "longest_run_ms": ms(longest),
                    "failed_runs": failures,
                    "statuses": statuses,
                }));
                cx.update(|_, cx| cx.quit())?;
                return Ok(());
            }
            (false, true, _) => {
                failures += 1;
                outage.get_or_insert_with(epoch_ms);
            }
            _ => {}
        }
        cx.background_executor()
            .timer(EVERY.saturating_sub(started.elapsed()))
            .await;
    }
}

/// Frame samples of one scroll pass.
struct Pass {
    name: &'static str,
    started: Instant,
    last: Instant,
    intervals: Vec<f64>,
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
            steps: 0,
        }
    }

    /// Records the frame just drawn.
    fn frame(&mut self) {
        let now = Instant::now();
        if self.steps > 0 {
            self.intervals.push(ms(now - self.last));
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
        }));
    }
}

/// Scrolls the grid, one step per frame: from the first row to the last
/// in [`VERTICAL_PASS`] whatever the frame rate, then across the columns
/// and back.
fn scroll(grid: Entity<TableState<Grid>>, rows: usize, columns: usize, window: &mut Window) {
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
