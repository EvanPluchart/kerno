//! Logs go to a daily-rotated file in the data directory, never anywhere
//! else. Kerno does not log SQL text, values or connection secrets: log
//! calls carry what happened and how long it took, not what was said.

use std::path::Path;

use gpui_kit::{App, Global};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

/// Days of log files kept.
const KEPT_FILES: usize = 7;
/// Lines waiting for the writer thread. The library's default (128,000)
/// allocates 4 MB up front; Kerno logs a few lines per action. When the
/// queue is full, new lines are dropped rather than blocking the UI.
const QUEUED_LINES: usize = 1024;

/// Keeps the log writer alive for as long as the app runs; dropping it
/// with the app's globals flushes the last lines.
struct LogWriter(#[allow(dead_code)] WorkerGuard);

impl Global for LogWriter {}

/// Starts logging into `directory`, before anything else runs (K8: a start
/// failure is logged); [`keep`] the result once the app runs.
pub fn start(directory: &Path) -> anyhow::Result<WorkerGuard> {
    // KERNO_LOG overrides the level, e.g. KERNO_LOG=kerno=debug.
    let filter =
        EnvFilter::try_from_env("KERNO_LOG").unwrap_or_else(|_| EnvFilter::new("warn,kerno=info"));
    start_with(directory, filter)
}

/// Keeps the writer [`start`] made until the app quits.
pub fn keep(guard: WorkerGuard, cx: &mut App) {
    cx.set_global(LogWriter(guard));
}

/// [`start`] with this filter, kept in `cx`.
#[cfg(test)]
pub fn init_with(directory: &Path, filter: EnvFilter, cx: &mut App) -> anyhow::Result<()> {
    keep(start_with(directory, filter)?, cx);
    Ok(())
}

/// Creates the log directory, the user's alone: logs name hosts, databases
/// and users. The data folder above it too when this is what creates it:
/// the log starts before the store (K8), which would have created that
/// folder for the user alone and leaves an existing one as it is.
fn make_private(directory: &Path) -> std::io::Result<()> {
    let new_parent = directory.parent().filter(|parent| !parent.exists());
    std::fs::create_dir_all(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        for directory in new_parent.into_iter().chain([directory]) {
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    #[cfg(not(unix))]
    let _ = new_parent;
    Ok(())
}

fn start_with(directory: &Path, filter: EnvFilter) -> anyhow::Result<WorkerGuard> {
    make_private(directory)?;
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("kerno")
        .filename_suffix("log")
        .max_log_files(KEPT_FILES)
        .build(directory)?;
    let (writer, guard) = tracing_appender::non_blocking::NonBlockingBuilder::default()
        .buffered_lines_limit(QUEUED_LINES)
        .finish(appender);
    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .with_ansi(false),
        )
        .try_init()?;
    Ok(guard)
}

/// Writes the lines not written yet and stops logging, as quitting does.
#[cfg(test)]
pub fn flush(cx: &mut App) {
    cx.remove_global::<LogWriter>();
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    /// The log starts before the store: when it is what creates the data
    /// folder, that folder is the user's alone, as the store makes it; one
    /// that exists keeps its mode.
    #[test]
    fn the_data_folder_the_log_creates_is_the_user_s_alone() {
        let mode =
            |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let scratch = tempfile::tempdir().unwrap();
        let data = scratch.path().join("Kerno");
        super::make_private(&data.join("logs")).unwrap();
        assert_eq!(mode(&data), 0o700);
        assert_eq!(mode(&data.join("logs")), 0o700);

        let existing = scratch.path().join("mine");
        std::fs::create_dir(&existing).unwrap();
        std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o755)).unwrap();
        super::make_private(&existing.join("logs")).unwrap();
        assert_eq!(mode(&existing), 0o755);
        assert_eq!(mode(&existing.join("logs")), 0o700);
    }
}
