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

/// Keeps the log writer alive for as long as the app runs; dropping it
/// with the app's globals flushes the last lines.
struct LogWriter(#[allow(dead_code)] WorkerGuard);

impl Global for LogWriter {}

/// Starts logging into `directory`.
pub fn init(directory: &Path, cx: &mut App) -> anyhow::Result<()> {
    // KERNO_LOG overrides the level, e.g. KERNO_LOG=kerno=debug.
    let filter =
        EnvFilter::try_from_env("KERNO_LOG").unwrap_or_else(|_| EnvFilter::new("warn,kerno=info"));
    init_with(directory, filter, cx)
}

/// [`init`] with this filter.
pub fn init_with(directory: &Path, filter: EnvFilter, cx: &mut App) -> anyhow::Result<()> {
    std::fs::create_dir_all(directory)?;
    // Logs name hosts, databases and users: the directory is the user's alone.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    }
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("kerno")
        .filename_suffix("log")
        .max_log_files(KEPT_FILES)
        .build(directory)?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(writer)
                .with_ansi(false),
        )
        .try_init()?;
    cx.set_global(LogWriter(guard));
    Ok(())
}

/// Writes the lines not written yet and stops logging, as quitting does.
#[cfg(test)]
pub fn flush(cx: &mut App) {
    cx.remove_global::<LogWriter>();
}
