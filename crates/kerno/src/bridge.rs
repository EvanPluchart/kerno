//! The one place where GPUI and Tokio meet.
//!
//! Database drivers, the SSH tunnel and child processes need Tokio; the UI
//! runs on GPUI's executors. A single multi-threaded Tokio runtime (three
//! workers) runs all of that work, and the functions here hand its results
//! back as GPUI tasks the UI can await without blocking.

use gpui_kit::{App, AppContext as _, Global, Task};

/// Worker threads of the Tokio runtime.
const WORKER_THREADS: usize = 3;

struct Tokio(tokio::runtime::Runtime);

impl Global for Tokio {}

/// Starts the runtime. Call once, before anything is spawned.
pub fn init(cx: &mut App) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(WORKER_THREADS)
        .thread_name("kerno-io")
        .enable_all()
        .build()?;
    cx.set_global(Tokio(runtime));
    Ok(())
}

/// Runs blocking work (Kerno's SQLite store, the OS secret store) on
/// Tokio's blocking pool.
pub fn spawn_blocking<T: Send + 'static>(
    cx: &App,
    work: impl FnOnce() -> T + Send + 'static,
) -> Task<T> {
    let handle = cx.global::<Tokio>().0.spawn_blocking(work);
    cx.background_spawn(async move {
        match handle.await {
            Ok(output) => output,
            Err(error) => std::panic::resume_unwind(error.into_panic()),
        }
    })
}
