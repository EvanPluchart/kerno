//! The update item of the status bar, "Check for Updates…" and quitting to install
//! (CLAUDE.md §3 item 12, docs/DECISIONS.md D-045). The steps that run commands are
//! `crate::update`; Recto's `viewer/update.rs` (MIT, same owner) is the model: the same
//! timing, the same item, the same "declining keeps the checked download for the next click".
//! A timed check that finds a new version also opens a dialog, once per run (D-097).

use std::path::PathBuf;

use gpui_kit::base::TestTextExt as _;
use gpui_kit::component::Disableable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::ContextMenuExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, BorrowAppContext as _, Context, Global, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, Styled as _, TestSupportExt as _, WeakEntity, Window, div,
    px,
};
use kerno_core::update::{Version, release_page};

use super::{MenuOf, Then, Workspace};
use crate::actions::CheckForUpdates;
use crate::bridge;
use crate::state::AppState;
use crate::tabs::{StatusItem, TabAction};
use crate::update::{self, Install, Phase, Updates};

/// The tooltip of the status bar item when its version was skipped: it stays, without the dialog.
pub fn update_tooltip(cx: &App) -> Option<&'static str> {
    let (Phase::Available(version) | Phase::Ready(version, _)) = phase(cx)? else {
        return None;
    };
    let skipped = AppState::get(cx).settings.skipped_update.as_deref();
    (skipped == Some(version.to_string().as_str())).then_some(
        "You skipped this version: Kerno no longer asks about it at launch. Click to update anyway.",
    )
}

/// What "Update" does for this copy of Kerno, said in the dialog before anyone clicks it.
pub fn what_update_does(target: &Install, version: Version) -> String {
    let checked = format!(
        "Update downloads Kerno {version} from GitHub and checks it against the release's \
         SHA-256 checksums"
    );
    let asks = "If a tab holds an open transaction or pending changes, Kerno asks before it quits.";
    match target {
        Install::App(bundle) => format!(
            "{checked}, replaces {} and starts it again. {asks}",
            bundle.display()
        ),
        Install::Prefix(dir) => format!(
            "{checked}, replaces Kerno's files in {} and starts it again. {asks}",
            dir.display()
        ),
        Install::Page => "This copy of Kerno cannot replace itself (a package, an AppImage, a \
             build, or a folder Kerno may not write): Update opens the release page in your \
             browser, to download it from there."
            .to_string(),
    }
}

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

    /// The About window's "Check for updates" (K42): the same check as Help ▸ Check for
    /// Updates…, its answer said in the About window. Checks turned off in Settings or by
    /// `KERNO_NO_UPDATE_CHECK` send nothing and say so.
    pub fn check_from_about(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let off = if cx.try_global::<Updates>().is_none() {
            Some("Update checks are off for this run: KERNO_NO_UPDATE_CHECK is set.")
        } else if !AppState::get(cx).settings.check_updates {
            Some("Update checks are turned off in Settings ▸ Updates.")
        } else {
            None
        };
        cx.set_global(match off {
            Some(why) => AboutCheck::Failed(why.into()),
            None => AboutCheck::Checking,
        });
        if off.is_none() {
            self.check_updates(true, window, cx);
        }
        cx.notify();
    }

    /// Reads what `version` brings from its notes (K43, D-115), once per version per run, only
    /// while checks are on; the dialog and the About window show it when it arrives. A failure
    /// leaves them as they are, silently.
    fn ask_notes(&mut self, version: Version, cx: &mut Context<Self>) {
        let Some(updates) = cx.try_global::<Updates>() else {
            return;
        };
        if !AppState::get(cx).settings.check_updates || updates.notes_asked == Some(version) {
            return;
        }
        let (runner, current) = (updates.runner.clone(), updates.current.clone());
        cx.update_global::<Updates, _>(|updates, _| updates.notes_asked = Some(version));
        let task =
            bridge::spawn_blocking(cx, move || update::read_notes(&*runner, &current, version));
        cx.spawn(async move |this, cx| {
            if let Some(notes) = task.await {
                let _ = this.update(cx, |_, cx| {
                    cx.update_global::<Updates, _>(|updates, _| {
                        updates.notes = Some((version, notes))
                    });
                    cx.notify();
                });
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
        // Asked from the About window (K42): the answer is said there.
        if cx.try_global::<AboutCheck>() == Some(&AboutCheck::Checking) {
            if let Ok(Some(version)) = &found
                && matches!(
                    phase(cx),
                    Some(Phase::Idle | Phase::Available(_) | Phase::Failed(..))
                )
            {
                set_phase(cx, Phase::Available(*version));
            }
            if let Ok(Some(version)) = &found {
                self.ask_notes(*version, cx);
            }
            cx.set_global(match found {
                Ok(Some(version)) => AboutCheck::Newer(version),
                Ok(None) => AboutCheck::Latest(current),
                Err(why) => AboutCheck::Failed(format!(
                    "Kerno could not reach GitHub to check (offline?): {why}."
                )),
            });
            cx.notify();
            return;
        }
        match found {
            Ok(Some(version)) => {
                // A download or an install under way keeps its state. A skipped version keeps
                // its item too (D-097): only the dialog is left out.
                if matches!(
                    phase(cx),
                    Some(Phase::Idle | Phase::Available(_) | Phase::Failed(..))
                ) {
                    set_phase(cx, Phase::Available(version));
                }
                if !manual {
                    self.offer_update(version, window, cx);
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

    /// Opens the update dialog for `version`, unless it was shown in this run already (for any
    /// version), the version was skipped, or the item is no longer about it. Over another
    /// dialog it waits: `keep_focus` calls this again once none is open.
    pub(super) fn offer_update(
        &mut self,
        version: Version,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(updates) = cx.try_global::<Updates>() else {
            return;
        };
        let skipped = AppState::get(cx).settings.skipped_update.as_deref();
        if updates.offered.is_some()
            || updates.phase != Phase::Available(version)
            || skipped == Some(version.to_string().as_str())
        {
            return;
        }
        let have = SharedString::from(format!("You have {}.", updates.current));
        let does = SharedString::from(what_update_does(&updates.target, version));
        if window.has_active_dialog(cx) {
            self.pending_update_offer = Some(version);
            return;
        }
        let title = SharedString::from(format!("Kerno {version} is available"));
        cx.update_global::<Updates, _>(|updates, _| updates.offered = Some(version));
        self.ask_notes(version, cx);
        let workspace = cx.entity().downgrade();
        let text = |id: &'static str, text: SharedString| {
            div()
                .id(id)
                .test_support()
                .child(text.clone())
                .test_text(text)
        };
        window.open_dialog(cx, move |dialog, _, _| {
            let (workspace, have, does) = (workspace.clone(), have.clone(), does.clone());
            dialog
                .title(text("update-dialog-title", title.clone()))
                .w(px(500.))
                // Enter answers nothing: a key typed as the dialog arrives installs nothing.
                // Escape, the close button and the backdrop are "Later".
                .on_ok(|_, _, _| false)
                .content(move |content, _, cx| {
                    let answer =
                        |id: &'static str, label: &'static str, action: Option<TabAction>| {
                            let workspace = workspace.clone();
                            // Tab reaches Later first, Update last: an indented line typed as the
                            // dialog arrives (Tab, a word, Space) can close it, never skip or
                            // install, and the keyboard still reaches every answer (D-097).
                            Button::new(id)
                                .label(label)
                                .tab_index(match action {
                                    None => 1,
                                    Some(TabAction::Update) => 4,
                                    Some(_) => 3,
                                })
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    if let Some(action) = action.clone() {
                                        let _ = workspace.update(cx, |this, cx| {
                                            this.update_action(action, window, cx)
                                        });
                                    }
                                })
                        };
                    content.child(
                        v_flex()
                            .gap_3()
                            .text_sm()
                            .child(text("update-dialog-current", have.clone()))
                            .children(whats_new_block(version, cx))
                            .child(
                                text("update-dialog-does", does.clone())
                                    .text_color(cx.theme().muted_foreground),
                            )
                            .child(
                                h_flex()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        // The browser fetches the page; the button sends nothing itself.
                                        Button::new("update-dialog-notes")
                                            .ghost()
                                            .tab_index(2)
                                            .label("What's new")
                                            .on_click(move |_, _, cx| {
                                                cx.open_url(&release_page(version))
                                            }),
                                    )
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .child(
                                                answer(
                                                    "update-dialog-skip",
                                                    "Skip this version",
                                                    Some(TabAction::SkipVersion),
                                                )
                                                .ghost(),
                                            )
                                            .child(
                                                answer("update-dialog-later", "Later", None)
                                                    .outline(),
                                            )
                                            .child(
                                                answer(
                                                    "update-dialog-update",
                                                    "Update",
                                                    Some(TabAction::Update),
                                                )
                                                .primary(),
                                            ),
                                    ),
                            ),
                    )
                })
        });
    }

    /// The item at the right of the status bar while there is an update to talk about. A click
    /// updates (or opens the release page after a failure); a right-click opens its menu.
    pub(super) fn update_item(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let (text, failed) = update_label(cx)?;
        let tooltip = update_tooltip(cx);
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
                        .when_some(tooltip, |button, tooltip| button.tooltip(tooltip))
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
                // The item stays, so the version can still be installed (D-097).
                AppState::update_settings(cx, |settings| {
                    settings.skipped_update = Some(version.to_string())
                });
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

/// Where the About window's "Check for updates" stands (K42); absent until it is clicked.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum AboutCheck {
    #[default]
    Idle,
    Checking,
    /// Up to date: this version.
    Latest(String),
    Newer(Version),
    /// Why it did not or could not check, in plain words.
    Failed(String),
}

impl Global for AboutCheck {}

/// The About window's "Check for updates" button, to sit beside the version, and the line that
/// says its answer (with Update when a newer version is found), to sit under it.
pub fn about_check(workspace: WeakEntity<Workspace>, cx: &App) -> (AnyElement, AnyElement) {
    let state = cx.try_global::<AboutCheck>().cloned().unwrap_or_default();
    let checking = state == AboutCheck::Checking;
    let button = Button::new("about-check-updates")
        .small()
        .outline()
        .label("Check for updates")
        .disabled(checking)
        .on_click({
            let workspace = workspace.clone();
            move |_, window, cx| {
                let _ = workspace.update(cx, |this, cx| this.check_from_about(window, cx));
            }
        })
        .into_any_element();
    let text = match &state {
        AboutCheck::Idle => None,
        AboutCheck::Checking => Some("Checking…".to_string()),
        AboutCheck::Latest(current) => Some(format!("Kerno {current} is the latest version.")),
        AboutCheck::Newer(version) => Some(format!("Kerno {version} is available.")),
        AboutCheck::Failed(why) => Some(why.clone()),
    };
    let newer = match state {
        AboutCheck::Newer(version) => Some(version),
        _ => None,
    };
    let line = v_flex()
        .items_center()
        .gap_1()
        .text_sm()
        .when_some(text, |line, text| {
            let text = SharedString::from(text);
            line.child(
                div()
                    .id("about-update-state")
                    .test_support()
                    .text_color(cx.theme().muted_foreground)
                    .child(text.clone())
                    .test_text(text),
            )
        })
        .when_some(
            newer.and_then(|version| whats_new_block(version, cx)),
            |line, block| line.child(block),
        )
        .when_some(newer, |line, _| {
            line.child(
                Button::new("about-update")
                    .small()
                    .primary()
                    .label("Update")
                    .on_click(move |_, window, cx| {
                        window.close_dialog(cx);
                        let _ = workspace.update(cx, |this, cx| {
                            this.update_action(TabAction::Update, window, cx)
                        });
                    }),
            )
        })
        .into_any_element();
    (button, line)
}

/// The "What's new" block of a newer version (K43), once its notes were read: the summary,
/// then each lead, as plain text.
pub fn whats_new_block(version: Version, cx: &App) -> Option<AnyElement> {
    let notes = cx
        .try_global::<Updates>()?
        .notes
        .as_ref()
        .filter(|(of, _)| *of == version)
        .map(|(_, notes)| notes.clone())?;
    let mut lines = vec![notes.summary.clone()];
    lines.extend(notes.leads.iter().map(|lead| format!("• {lead}")));
    if notes.more {
        lines.push("…and more".into());
    }
    let text = SharedString::from(lines.join("\n"));
    Some(
        v_flex()
            .id("update-whats-new")
            .test_support()
            .gap_1()
            .child(
                div()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child("What's new"),
            )
            .children(lines.into_iter().map(|line| div().child(line)))
            .test_text(text)
            .into_any_element(),
    )
}
