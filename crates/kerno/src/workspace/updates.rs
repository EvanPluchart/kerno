//! The update item of the status bar, "Check for Updates…" and quitting to install
//! (CLAUDE.md §3 item 12, docs/DECISIONS.md D-045). The steps that run commands are
//! `crate::update`; Recto's `viewer/update.rs` (MIT, same owner) is the model: the same
//! timing, the same item, the same "declining keeps the checked download for the next click".

use std::path::PathBuf;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::ContextMenuExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _};
use gpui_kit::{
    AnyElement, App, BorrowAppContext as _, Context, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, Styled as _, TestSupportExt as _, Window, div,
};
use kerno_core::update::{Version, release_page};

use super::{MenuOf, Then, Workspace};
use crate::actions::CheckForUpdates;
use crate::bridge;
use crate::state::AppState;
use crate::tabs::{StatusItem, TabAction};
use crate::update::{self, Install, Phase, Updates};

/// Changes where the update stands.
fn set_phase(cx: &mut App, phase: Phase) {
    cx.update_global::<Updates, _>(|updates, _| updates.phase = phase);
}

fn phase(cx: &App) -> Option<Phase> {
    cx.try_global::<Updates>()
        .map(|updates| updates.phase.clone())
}

/// What the status bar item says, and whether it reports a failure; `None` when it has nothing
/// to say.
pub fn update_label(cx: &App) -> Option<(String, bool)> {
    Some(match phase(cx)? {
        Phase::Idle => return None,
        Phase::Available(version) | Phase::Ready(version, _) => {
            (format!("Kerno {version} is available · Update"), false)
        }
        Phase::Downloading(version) => (format!("Downloading Kerno {version}…"), false),
        Phase::Installing(version) => (format!("Installing Kerno {version}…"), false),
        Phase::Failed(version, _) => (
            format!("Kerno {version} was not installed · Release page"),
            true,
        ),
    })
}

impl Workspace {
    /// Checks 3 s after the window opens, then every 6 hours while it lives. Each check reads
    /// the setting and whether this run may check at all ([`Updates`] installed).
    pub fn start_update_checks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(update::FIRST).await;
            loop {
                let checked =
                    this.update_in(cx, |this, window, cx| this.check_updates(false, window, cx));
                // The window is gone: nothing keeps checking.
                if checked.is_err() {
                    break;
                }
                cx.background_executor().timer(update::EVERY).await;
            }
        })
        .detach();
    }

    pub(super) fn check_for_updates(
        &mut self,
        _: &CheckForUpdates,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.check_updates(true, window, cx);
    }

    /// One check. `manual` (Help ▸ Check for Updates…, the palette) asks even with the setting
    /// off and says what it found; a timed check asks only with the setting on, and is silent
    /// unless it finds a release (offline is normal: the next one tries again).
    pub(crate) fn check_updates(
        &mut self,
        manual: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(updates) = cx.try_global::<Updates>() else {
            if manual {
                window.push_notification(
                    Notification::warning(
                        "Update checks are off for this run (KERNO_NO_UPDATE_CHECK is set, or a \
                         bench run).",
                    ),
                    cx,
                );
            }
            return;
        };
        if updates.checking || !manual && !AppState::get(cx).settings.check_updates {
            return;
        }
        let (runner, current) = (updates.runner.clone(), updates.current.clone());
        cx.update_global::<Updates, _>(|updates, _| updates.checking = true);
        let task = bridge::spawn_blocking(cx, move || update::check(&*runner, &current));
        cx.spawn_in(window, async move |this, cx| {
            let found = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.update_checked(found, manual, window, cx)
            });
        })
        .detach();
    }

    fn update_checked(
        &mut self,
        found: Result<Option<Version>, String>,
        manual: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = cx.update_global::<Updates, _>(|updates, _| {
            updates.checking = false;
            updates.current.clone()
        });
        match found {
            Ok(Some(version)) => {
                let skipped = AppState::get(cx).settings.skipped_update.clone();
                let skip = !manual && skipped == Some(version.to_string());
                // A download or an install under way keeps its state.
                if !skip
                    && matches!(
                        phase(cx),
                        Some(Phase::Idle | Phase::Available(_) | Phase::Failed(..))
                    )
                {
                    set_phase(cx, Phase::Available(version));
                }
                if manual {
                    window.push_notification(
                        Notification::info(SharedString::from(format!(
                            "Kerno {version} is available: “Update” in the status bar installs it."
                        ))),
                        cx,
                    );
                }
            }
            Ok(None) if manual => window.push_notification(
                Notification::success(SharedString::from(format!(
                    "Kerno is up to date ({current})."
                ))),
                cx,
            ),
            Err(why) if manual => window.push_notification(
                Notification::warning(SharedString::from(format!(
                    "Kerno could not check for updates: {why}."
                ))),
                cx,
            ),
            _ => {}
        }
        cx.notify();
    }

    /// The item at the right of the status bar while there is an update to talk about. A click
    /// updates (or opens the release page after a failure); a right-click opens its menu.
    pub(super) fn update_item(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let (text, failed) = update_label(cx)?;
        let theme = cx.theme();
        let color = match self.active_chrome(cx) {
            Some(chrome) => chrome.bar_text,
            None if failed => theme.danger,
            None => theme.link,
        };
        Some(
            div()
                .id(StatusItem::Update.id())
                .test_support()
                .context_menu(self.menu(MenuOf::Status(StatusItem::Update), cx))
                .child(
                    Button::new("status-update-button")
                        .ghost()
                        .xsmall()
                        .child(div().text_color(color).child(text))
                        .on_click(cx.listener(|this, _, window, cx| {
                            if let Some(Phase::Failed(version, _)) = phase(cx) {
                                cx.open_url(&release_page(version));
                            } else {
                                this.update_action(TabAction::Update, window, cx);
                            }
                        })),
                )
                .into_any_element(),
        )
    }

    /// An entry of the update item's menu.
    pub(super) fn update_action(
        &mut self,
        action: TabAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(updates) = cx.try_global::<Updates>() else {
            return;
        };
        let target = updates.target.clone();
        let phase = updates.phase.clone();
        let Some(version) = phase.version() else {
            return;
        };
        match action {
            TabAction::Update => match phase {
                Phase::Downloading(_) | Phase::Installing(_) => window
                    .push_notification(Notification::info("The update is already under way."), cx),
                _ if target == Install::Page => cx.open_url(&release_page(version)),
                Phase::Ready(_, file) if file.is_file() => {
                    self.quit_to_install(version, file, window, cx)
                }
                _ => self.download_update(version, window, cx),
            },
            TabAction::OpenReleasePage => cx.open_url(&release_page(version)),
            TabAction::SkipVersion => {
                AppState::update_settings(cx, |settings| {
                    settings.skipped_update = Some(version.to_string())
                });
                set_phase(cx, Phase::Idle);
            }
            TabAction::TurnOffUpdateChecks => {
                AppState::update_settings(cx, |settings| settings.check_updates = false);
                set_phase(cx, Phase::Idle);
                window.push_notification(
                    Notification::info(
                        "Update checks are off. Settings ▸ Updates turns them on again; Help ▸ \
                         Check for Updates… still asks.",
                    ),
                    cx,
                );
            }
            _ => {}
        }
        cx.notify();
    }

    /// Downloads and checks the release in the background; the window stays usable. Once
    /// checked, the quit questions follow.
    fn download_update(&mut self, version: Version, window: &mut Window, cx: &mut Context<Self>) {
        let Some(updates) = cx.try_global::<Updates>() else {
            return;
        };
        let (runner, current, temp) = (
            updates.runner.clone(),
            updates.current.clone(),
            updates.temp.clone(),
        );
        set_phase(cx, Phase::Downloading(version));
        let task = bridge::spawn_blocking(cx, move || {
            update::download(&*runner, &current, version, &temp)
        });
        cx.spawn_in(window, async move |this, cx| {
            let downloaded = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                // Skipped or turned off meanwhile: the download is dropped.
                if phase(cx) != Some(Phase::Downloading(version)) {
                    if let Ok(file) = &downloaded {
                        remove_download(file);
                    }
                    return;
                }
                match downloaded {
                    Ok(file) => {
                        set_phase(cx, Phase::Ready(version, file.clone()));
                        this.quit_to_install(version, file, window, cx);
                    }
                    Err(why) => this.update_failed(version, why, window, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn update_failed(
        &mut self,
        version: Version,
        why: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.push_notification(
            Notification::error(SharedString::from(format!(
                "Kerno {version} was not installed: {why}. Nothing was changed; its release \
                 page has it."
            ))),
            cx,
        );
        set_phase(cx, Phase::Failed(version, why));
        cx.notify();
    }

    /// Asks what quitting would end (open transactions, pending changes), as closing sessions
    /// does; then writes the tabs and settings (the new Kerno reads them, unsaved text
    /// included), installs, starts the new Kerno and quits. Declining keeps the checked download
    /// for the next click.
    fn quit_to_install(
        &mut self,
        version: Version,
        file: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let action = format!("Quit Kerno to install {version}");
        let then: Then =
            Box::new(move |this, window, cx| this.install_and_quit(version, file, window, cx));
        self.end_then(action, self.tabs.clone(), then, window, cx);
    }

    fn install_and_quit(
        &mut self,
        version: Version,
        file: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(updates) = cx.try_global::<Updates>() else {
            return;
        };
        let (runner, target) = (updates.runner.clone(), updates.target.clone());
        set_phase(cx, Phase::Installing(version));
        cx.notify();
        let (tabs, settings) = (self.save_tabs(cx), AppState::flush_settings(cx));
        cx.spawn_in(window, async move |this, cx| {
            tabs.await;
            settings.await;
            let Ok(install) = this.update(cx, |_, cx| {
                bridge::spawn_blocking(cx, move || {
                    let installed = update::install(&*runner, &target, &file, version);
                    remove_download(&file);
                    installed
                })
            }) else {
                return;
            };
            let installed = install.await;
            let _ = this.update_in(cx, |this, window, cx| match installed {
                Ok(()) => cx.quit(),
                Err(why) => this.update_failed(version, why, window, cx),
            });
        })
        .detach();
    }
}

/// Removes the private folder a download was made in.
fn remove_download(file: &std::path::Path) {
    if let Some(folder) = file.parent() {
        let _ = std::fs::remove_dir_all(folder);
    }
}
