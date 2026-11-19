//! The one place where GPUI and Tokio meet.
//!
//! Database drivers, the SSH tunnel and child processes need Tokio; the UI
//! runs on GPUI's executors. A single multi-threaded Tokio runtime (three
//! workers) runs all of that work, and the functions here hand its results
//! back as GPUI tasks the UI can await without blocking.

use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::{App, AppContext as _, Global, Task};
use kerno_core::Session;

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

/// Reads on a server session that the UI can stop (C3): Cancel, a closed
/// tab, a load that took too long. Stopping drops the work where it waits,
/// so that nothing queued behind it on the session is sent, then closes
/// the sessions it ran on, which stops their statement on the server (the
/// drivers' `close` sends PostgreSQL's cancel request, MySQL's
/// `KILL QUERY`). A session given to a stopped `Stop` is closed at once.
#[derive(Clone, Default)]
pub struct Stop(Rc<RefCell<Stopping>>);

#[derive(Default)]
struct Stopping {
    stopped: bool,
    sessions: Vec<Arc<dyn Session>>,
    running: Vec<tokio::task::AbortHandle>,
}

impl Stop {
    /// Runs `work` on `session` on the Tokio runtime. The task gives
    /// `None` when [`Stop::stop`] came first.
    pub fn run<T: Send + 'static, F: Future<Output = T> + Send + 'static>(
        &self,
        cx: &App,
        session: Arc<dyn Session>,
        work: impl FnOnce(Arc<dyn Session>) -> F,
    ) -> Task<Option<T>> {
        let mut state = self.0.borrow_mut();
        if state.stopped {
            drop(state);
            close(cx, vec![session]);
            return Task::ready(None);
        }
        if !state
            .sessions
            .iter()
            .any(|known| Arc::ptr_eq(known, &session))
        {
            state.sessions.push(session.clone());
        }
        let handle = cx.global::<Tokio>().0.spawn(work(session));
        state.running.retain(|running| !running.is_finished());
        state.running.push(handle.abort_handle());
        cx.background_spawn(async move {
            match handle.await {
                Ok(output) => Some(output),
                Err(error) if error.is_cancelled() => None,
                Err(error) => std::panic::resume_unwind(error.into_panic()),
            }
        })
    }

    /// Drops the work in flight and closes its sessions; what is run after
    /// this is not run.
    pub fn stop(&self, cx: &App) {
        let mut state = self.0.borrow_mut();
        state.stopped = true;
        for running in state.running.drain(..) {
            running.abort();
        }
        let sessions = std::mem::take(&mut state.sessions);
        drop(state);
        close(cx, sessions);
    }

    /// Whether [`Stop::stop`] was called.
    pub fn is_stopped(&self) -> bool {
        self.0.borrow().stopped
    }

    /// Whether work it runs has not ended yet.
    pub fn busy(&self) -> bool {
        let state = self.0.borrow();
        !state.stopped && state.running.iter().any(|running| !running.is_finished())
    }
}

/// Closes `sessions` off the UI thread; a server that stopped answering
/// does not hold it.
fn close(cx: &App, sessions: Vec<Arc<dyn Session>>) {
    if sessions.is_empty() {
        return;
    }
    spawn(cx, async move {
        for session in sessions {
            let _ = tokio::time::timeout(Duration::from_secs(2), session.close()).await;
        }
    })
    .detach();
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
