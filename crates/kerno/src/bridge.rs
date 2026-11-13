//! The one place where GPUI and Tokio meet.
//!
//! Database drivers, the SSH tunnel and child processes need Tokio; the UI
//! runs on GPUI's executors. A single multi-threaded Tokio runtime (three
//! workers) runs all of that work, and the functions here hand its results
//! back as GPUI tasks the UI can await without blocking.

use std::future::Future;

use gpui_kit::{App, AppContext as _, Global, Task};

/// Worker threads of the Tokio runtime.
const WORKER_THREADS: usize = 3;

struct Tokio(tokio::runtime::Runtime);

impl Global for Tokio {}

/// Starts the runtime. Call once, before anything is spawned.
pub fn init(cx: &mut App) -> anyhow::Result<()> {
    // Once per process run: starting it again would drop the first runtime
    // and cancel whatever was still running on it (tests open the app twice).
    if cx.has_global::<Tokio>() {
        return Ok(());
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(WORKER_THREADS)
        .thread_name("kerno-io")
        .enable_all()
        .build()?;
    cx.set_global(Tokio(runtime));
    Ok(())
}

/// Runs `future` on the Tokio runtime and returns a GPUI task for its
/// output. Dropping the task does not stop the work.
pub fn spawn<T: Send + 'static>(
    cx: &App,
    future: impl Future<Output = T> + Send + 'static,
) -> Task<T> {
    let handle = cx.global::<Tokio>().0.spawn(future);
    cx.background_spawn(async move {
        match handle.await {
            Ok(output) => output,
            // The work panicked: carry the panic to whoever awaits it.
            Err(error) => std::panic::resume_unwind(error.into_panic()),
        }
    })
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
