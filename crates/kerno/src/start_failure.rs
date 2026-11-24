//! What Kerno shows when it cannot start (K8, C16): a small window that says
//! what happened in plain words, what to do, the raw error below, and the
//! buttons that apply. Before, the error went to stderr and the app quit, so
//! from the Dock the icon bounced and vanished.
//!
//! The window needs no workspace, no app state and no Tokio bridge: only the
//! kit and, when it loads, Kerno's theme. "Start fresh" renames the store
//! aside ([`kerno_core::storage::move_aside`]), never deletes it, and starts
//! Kerno in the same run. A failure before the platform runs at all (no
//! display) cannot show a window: stderr and the log are all there is.

use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, ClickEvent, Context, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, TestSupportExt as _, TitlebarOptions, Window, WindowBounds, WindowOptions, div,
    px, size,
};
use kerno_core::config::Secret;
use kerno_core::instance;
use kerno_core::storage::{self, AppPaths, OpenError, OpenProblem};

use crate::theme::{self, Appearance};

// The start that follows "Start fresh". Tests record it instead of opening
// the main window (with its keychain, socket thread and Dock icon).
#[cfg(not(test))]
use crate::start as start_kerno;
#[cfg(test)]
use tests::start_kerno;

/// What the window says about one failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartFailure {
    /// What happened, in plain words.
    pub headline: String,
    /// What the user can do.
    pub next_step: String,
    /// The error as Kerno got it.
    pub raw: String,
    /// Moving the data aside would let Kerno start (the data is the
    /// problem, not the disk or another program).
    pub can_start_fresh: bool,
}

/// Kerno could not tell where its data goes (no home directory).
#[derive(Debug, Clone)]
pub struct NoDataDir(pub String);

impl std::fmt::Display for NoDataDir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NoDataDir {}

/// How "Start fresh" is explained wherever it is offered.
const FRESH: &str = "start fresh: the data is moved aside in its folder, renamed with the \
                     time, and nothing is deleted";

/// What to say about `error`, a failure of Kerno's start.
pub fn start_failure(error: &anyhow::Error) -> StartFailure {
    let version = env!("CARGO_PKG_VERSION");
    let (headline, next_step, can_start_fresh): (String, String, bool) = match error
        .downcast_ref::<OpenError>()
        .map(|error| error.problem)
    {
        Some(OpenProblem::Newer { schema, known }) => (
            format!(
                "This data was written by a newer Kerno (data version {schema}); this is \
                     Kerno {version}, which reads up to version {known}."
            ),
            format!(
                "Install the newer Kerno again to keep using it, or {FRESH}. To use the moved \
                 data again, quit Kerno and rename its files back to kerno.db, kerno.db-wal and \
                 kerno.db-shm."
            ),
            true,
        ),
        Some(OpenProblem::Damaged) => (
            "Kerno's data file is damaged, or is not a Kerno data file.".into(),
            format!(
                "Put back a copy from the backups folder if there is one (quit Kerno, move \
                 kerno.db, kerno.db-wal and kerno.db-shm aside together, copy the backup in as \
                 kerno.db), or {FRESH}."
            ),
            true,
        ),
        Some(OpenProblem::InUse) => (
            "Another program is using Kerno's data file.".into(),
            "Quit the other Kerno, or the program that opened kerno.db, then open Kerno \
                 again."
                .into(),
            false,
        ),
        Some(OpenProblem::NotWritable) => not_writable(),
        Some(OpenProblem::CopyFailed) => (
            "Kerno did not update its data: the copy it takes first could not be written.".into(),
            "Your data is unchanged. Free some disk space, or make the backups folder \
                 writable, then open Kerno again."
                .into(),
            false,
        ),
        Some(OpenProblem::MigrationFailed) => (
            format!("Kerno {version} could not update its data; nothing was changed."),
            format!(
                "A copy taken just before is in the backups folder. Open the Kerno you used \
                     before to keep using this data, or {FRESH}."
            ),
            true,
        ),
        Some(OpenProblem::Other) => (
            "Kerno could not open its data file.".into(),
            format!("Open Kerno again; if this keeps happening, {FRESH}."),
            true,
        ),
        None if error.downcast_ref::<NoDataDir>().is_some() => (
            "Kerno could not find your home folder to keep its data in.".into(),
            "Set HOME, or open Kerno with KERNO_DATA_DIR naming a folder you can write to.".into(),
            false,
        ),
        None => match error.downcast_ref::<kerno_core::Error>() {
            Some(kerno_core::Error::Io { .. }) => not_writable(),
            Some(kerno_core::Error::Storage(_)) => (
                "Kerno could not read its saved data.".into(),
                format!("Open Kerno again; if this keeps happening, {FRESH}."),
                true,
            ),
            _ => (
                "Kerno could not start.".into(),
                "Open Kerno again. If this keeps happening, report the details below: they \
                     are also in Kerno's log, in the logs folder of its data folder."
                    .into(),
                false,
            ),
        },
    };
    StartFailure {
        headline,
        next_step,
        raw: format!("{error:#}"),
        can_start_fresh,
    }
}

fn not_writable() -> (String, String, bool) {
    (
        "Kerno cannot read or write its data folder.".into(),
        "Check that the folder named below belongs to you and that the disk is not full, then \
         open Kerno again."
            .into(),
        false,
    )
}

/// The window of a failed start.
pub struct FailureWindow {
    failure: StartFailure,
    /// Where the data is; `None` when even that is unknown.
    paths: Option<AppPaths>,
    /// The URL Kerno was opened with, for the start after "Start fresh".
    url: Option<Secret>,
    /// Another Kerno has this data open: moving it would pull it from under
    /// that one, so "Start fresh" is not offered.
    in_use: bool,
    /// "Start fresh" is running.
    moving: bool,
    /// What "Start fresh" did, or why it did not.
    note: Option<String>,
}

/// Logs `error` and opens its window; quits when even that fails.
pub fn open(error: &anyhow::Error, paths: Option<AppPaths>, url: Option<Secret>, cx: &mut App) {
    tracing::error!("Kerno could not start: {error:#}");
    eprintln!("Kerno could not start: {error:#}");
    // The kit's own theme is there already; Kerno's when it loads.
    if let Err(error) = theme::init(Appearance::System, cx) {
        tracing::warn!(%error, "the theme did not load for the start-failure window");
    }
    let in_use = paths.as_ref().is_some_and(|paths| in_use(&paths.data_dir));
    let view = FailureWindow {
        failure: start_failure(error),
        paths,
        url,
        in_use,
        moving: false,
        note: None,
    };
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(600.), px(420.)),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some("Kerno".into()),
            ..Default::default()
        }),
        app_id: Some("kerno".into()),
        ..Default::default()
    };
    match gpui_kit::open_window(options, cx, |_, cx| cx.new(|_| view)) {
        Ok(_) => cx.activate(true),
        Err(error) => {
            eprintln!("Kerno could not open its window either: {error:#}");
            cx.quit();
        }
    }
}

/// Whether a running Kerno holds the instance socket of `data_dir`.
fn in_use(data_dir: &std::path::Path) -> bool {
    instance::socket_path(data_dir).is_ok_and(|socket| UnixStream::connect(socket).is_ok())
}

impl FailureWindow {
    /// "Start fresh" is shown.
    fn offers_start_fresh(&self) -> bool {
        self.failure.can_start_fresh && !self.in_use && self.paths.is_some()
    }

    fn open_folder(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(paths) = &self.paths {
            let database = paths.database();
            // The store selected in its folder; the folder when it is not there.
            match database.exists() {
                true => cx.reveal_path(&database),
                false => cx.reveal_path(&paths.data_dir),
            }
        }
    }

    fn start_fresh(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(paths) = self.paths.clone() else {
            return;
        };
        if self.moving || !self.offers_start_fresh() {
            return;
        }
        // Another Kerno may have opened this data since the window did.
        self.in_use = in_use(&paths.data_dir);
        if self.in_use {
            cx.notify();
            return;
        }
        self.moving = true;
        self.note = Some("Moving the data aside…".into());
        cx.notify();
        let work = cx.background_executor().spawn(async move {
            let moved = storage::move_aside(&paths.database())?;
            anyhow::Ok((moved, crate::load(&paths)))
        });
        cx.spawn_in(window, async move |this, cx| {
            let outcome = work.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.started_fresh(outcome, window, cx)
            });
        })
        .detach();
    }

    fn started_fresh(
        &mut self,
        outcome: anyhow::Result<(PathBuf, anyhow::Result<crate::Loaded>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.moving = false;
        let (moved, loaded) = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                tracing::error!("the data could not be moved aside: {error:#}");
                self.note = Some(format!("The data could not be moved aside: {error:#}"));
                cx.notify();
                return;
            }
        };
        tracing::warn!(to = %moved.display(), "the data was moved aside (Start fresh)");
        let started = loaded
            .and_then(|(startup, listener)| start_kerno(startup, listener, self.url.take(), cx));
        match started {
            Ok(()) => {
                let this = window.window_handle();
                cx.defer(move |cx| told_where(this, &moved, cx));
                window.remove_window();
            }
            Err(error) => {
                tracing::error!("Kerno could not start after moving the data aside: {error:#}");
                self.failure = start_failure(&error);
                self.note = Some(format!(
                    "The data was moved to {}, but Kerno still could not start.",
                    moved.display()
                ));
                cx.notify();
            }
        }
    }
}

/// The main window says where the old data went.
fn told_where(failure_window: AnyWindowHandle, moved: &std::path::Path, cx: &mut App) {
    let text = format!(
        "Kerno started fresh. The earlier data was moved to {}; nothing was deleted.",
        moved.display()
    );
    if let Some(main) = cx
        .windows()
        .into_iter()
        .find(|window| window.window_id() != failure_window.window_id())
    {
        let _ = main.update(cx, |_, window, cx| {
            window.push_notification(
                Notification::warning(SharedString::from(text)).autohide(false),
                cx,
            )
        });
    }
}

impl Render for FailureWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, mono) = (theme.muted_foreground, theme.mono_font_family.clone());
        let failure = &self.failure;
        v_flex()
            .size_full()
            .p_6()
            .gap_3()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                div()
                    .id("start-failure-headline")
                    .test_support()
                    .aria_label(failure.headline.clone())
                    .text_lg()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(failure.headline.clone()),
            )
            .child(
                div()
                    .id("start-failure-next")
                    .test_support()
                    .aria_label(failure.next_step.clone())
                    .child(failure.next_step.clone()),
            )
            .when(failure.can_start_fresh && self.in_use, |this| {
                const IN_USE: &str =
                    "Another Kerno is using this data: quit it before starting fresh.";
                this.child(
                    div()
                        .id("start-failure-in-use")
                        .test_support()
                        .aria_label(IN_USE)
                        .text_color(muted)
                        .child(IN_USE),
                )
            })
            .child(
                div()
                    .id("start-failure-raw")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_2()
                    .rounded_md()
                    .bg(theme.muted)
                    .text_color(muted)
                    .text_sm()
                    .font_family(mono)
                    .child(failure.raw.clone())
                    .test_support()
                    .aria_label(failure.raw.clone()),
            )
            .when_some(self.note.clone(), |this, note| {
                this.child(
                    div()
                        .id("start-failure-note")
                        .test_support()
                        .aria_label(note.clone())
                        .child(note),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .when(self.paths.is_some(), |this| {
                        this.child(
                            Button::new("start-failure-folder")
                                .ghost()
                                .label("Open data folder")
                                .on_click(cx.listener(Self::open_folder)),
                        )
                    })
                    .when(self.offers_start_fresh(), |this| {
                        this.child(
                            Button::new("start-failure-fresh")
                                .warning()
                                .label("Start fresh (move data aside)")
                                .disabled(self.moving)
                                .on_click(cx.listener(Self::start_fresh)),
                        )
                    })
                    .child(
                        Button::new("start-failure-quit")
                            .primary()
                            .label("Quit")
                            .on_click(|_, _, cx| cx.quit()),
                    ),
            )
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use gpui_kit::Global;

    use super::*;

    /// What the start after "Start fresh" was given, in tests.
    pub struct Started {
        pub connections: usize,
        pub database: PathBuf,
    }

    impl Global for Started {}

    pub fn start_kerno(
        startup: crate::state::Startup,
        _: crate::Listener,
        _: Option<Secret>,
        cx: &mut App,
    ) -> anyhow::Result<()> {
        cx.set_global(Started {
            connections: startup.connections.len(),
            database: startup.paths.database(),
        });
        Ok(())
    }

    fn open_error(problem: OpenProblem) -> anyhow::Error {
        anyhow::Error::new(OpenError {
            problem,
            message: "the store's own words".into(),
        })
    }

    /// C16: each start failure says what happened and offers "Start fresh"
    /// only where moving the data aside lets Kerno start.
    #[test]
    fn each_start_failure_says_what_happened_and_what_to_do() {
        let newer = start_failure(&open_error(OpenProblem::Newer {
            schema: 14,
            known: 4,
        }));
        assert!(newer.headline.contains("newer Kerno"), "{newer:?}");
        assert!(newer.headline.contains("version 14"), "{newer:?}");
        assert!(newer.headline.contains("up to version 4"), "{newer:?}");
        assert!(newer.next_step.contains("nothing is deleted"), "{newer:?}");
        assert!(newer.can_start_fresh);
        assert_eq!(newer.raw, "the store's own words");

        for (problem, fresh) in [
            (OpenProblem::Damaged, true),
            (OpenProblem::MigrationFailed, true),
            (OpenProblem::Other, true),
            (OpenProblem::InUse, false),
            (OpenProblem::NotWritable, false),
            (OpenProblem::CopyFailed, false),
        ] {
            let failure = start_failure(&open_error(problem));
            assert_eq!(failure.can_start_fresh, fresh, "{problem:?}");
            assert!(!failure.headline.is_empty() && !failure.next_step.is_empty());
        }
        let copy = start_failure(&open_error(OpenProblem::CopyFailed));
        assert!(copy.next_step.contains("unchanged"), "{copy:?}");

        // A data folder that cannot be created.
        let unreadable = start_failure(&anyhow::Error::new(kerno_core::Error::io(
            "creating /nowhere/Kerno",
            std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        )));
        assert!(
            unreadable.headline.contains("data folder"),
            "{unreadable:?}"
        );
        assert!(!unreadable.can_start_fresh);
        assert!(unreadable.raw.contains("/nowhere/Kerno"), "{unreadable:?}");

        let home = start_failure(&anyhow::Error::new(NoDataDir("no home".into())));
        assert!(home.headline.contains("home folder"), "{home:?}");
        assert!(!home.can_start_fresh);

        let rows = start_failure(&anyhow::Error::new(kerno_core::Error::Storage(
            "no such table: tabs".into(),
        )));
        assert!(rows.can_start_fresh, "{rows:?}");

        let other = start_failure(&anyhow::anyhow!("the GPU said no"));
        assert_eq!(other.headline, "Kerno could not start.");
        assert!(!other.can_start_fresh);
        assert_eq!(other.raw, "the GPU said no");
    }
}
