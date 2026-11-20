//! What the whole app shares: where its files are, its storage, the secret
//! store and the settings.

use std::sync::{Arc, Mutex};

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{App, BorrowAppContext as _, Global, SharedString, Task};
use kerno_core::secrets::{self, SecretStore};
use kerno_core::storage::{AppPaths, SavedConnection, Storage};

use crate::bridge;
use crate::settings::Settings;
use crate::theme::Appearance;

/// Storage key of the index of the active tab.
pub const ACTIVE_TAB_KEY: &str = "tabs.active";

/// Storage key of the groups of tabs (K19): see `workspace::panes::SavedPane`.
pub const TAB_LAYOUT_KEY: &str = "tabs.layout";

/// App-wide state, available as a GPUI global.
pub struct AppState {
    /// Where Kerno keeps its files.
    pub paths: AppPaths,
    /// Kerno's SQLite store. Synchronous: call it through
    /// [`bridge::spawn_blocking`] once the app is running.
    pub storage: Arc<Storage>,
    /// The OS secret store, or memory when there is none.
    pub secrets: Arc<dyn SecretStore>,
    /// The user's settings.
    pub settings: Settings,
    /// A theme pinned for this run only (`KERNO_APPEARANCE`), which takes
    /// precedence over the stored setting until the user picks one.
    pub appearance_override: Option<Appearance>,
}

impl Global for AppState {}

/// What is read from disk before the first frame, off the main thread,
/// while the platform starts up.
pub struct Startup {
    pub paths: AppPaths,
    pub storage: Arc<Storage>,
    pub settings: Settings,
    pub connections: Vec<SavedConnection>,
    /// Saved connections that could not be read: hidden, kept in the store.
    pub unreadable_connections: usize,
    pub tabs: Vec<crate::tabs::TabSnapshot>,
    /// Index of the tab that was active.
    pub active_tab: usize,
    /// The groups of the tabs, when stored and readable (K19).
    pub tab_layout: Option<crate::workspace::panes::SavedPane>,
}

impl Startup {
    /// Opens the store under `paths` and reads what the first frame needs.
    pub fn load(paths: &AppPaths) -> anyhow::Result<Self> {
        let storage = Arc::new(Storage::open(&paths.database())?);
        let settings = Settings::load(&storage);
        let (connections, unreadable_connections) = storage.connections_and_unreadable()?;
        let tabs = storage.tabs()?;
        let active_tab = storage.setting(ACTIVE_TAB_KEY)?.unwrap_or(0);
        // A layout that cannot be read is no reason not to start: one group.
        let tab_layout = storage.setting(TAB_LAYOUT_KEY).unwrap_or(None);
        Ok(Self {
            paths: paths.clone(),
            storage,
            settings,
            connections,
            unreadable_connections,
            tabs,
            active_tab,
            tab_layout,
        })
    }
}

/// What the main window says once when saved connections could not be
/// read (K8, C16), or nothing.
pub fn unreadable_notice(count: usize) -> Option<String> {
    let (what, is) = match count {
        0 => return None,
        1 => ("1 saved connection".to_string(), "is"),
        _ => (format!("{count} saved connections"), "are"),
    };
    Some(format!(
        "{what} could not be read and {is} hidden. Nothing was deleted: the data file \
         still holds them, and the Kerno that wrote them can read them."
    ))
}

impl AppState {
    /// Installs the global state.
    pub fn init(startup: &Startup, appearance_override: Option<Appearance>, cx: &mut App) {
        cx.set_global(AppState {
            paths: startup.paths.clone(),
            storage: startup.storage.clone(),
            secrets: secrets::default_store(),
            settings: startup.settings.clone(),
            appearance_override,
        });
    }

    /// The theme choice in effect.
    pub fn appearance(&self) -> Appearance {
        self.appearance_override.unwrap_or(self.settings.appearance)
    }

    /// The global state.
    pub fn get(cx: &App) -> &AppState {
        cx.global::<AppState>()
    }

    /// Changes the settings and stores them in the background. Changes
    /// reach storage in the order they were made: the last one always wins.
    pub fn update_settings(cx: &mut App, change: impl FnOnce(&mut Settings)) {
        let (storage, settings) = cx.update_global::<AppState, _>(|state, _| {
            change(&mut state.settings);
            (state.storage.clone(), state.settings.clone())
        });
        let saver = cx.default_global::<SettingsSaver>().saver.clone();
        let start = {
            let mut pending = saver.pending.lock().unwrap_or_else(|e| e.into_inner());
            pending.latest = Some(settings);
            !std::mem::replace(&mut pending.writing, true)
        };
        // One writer at a time; it writes the newest value until none is
        // left, so a change made while it writes is written after. How the
        // last write ended is shown (K8).
        if start {
            let writing = bridge::spawn_blocking(cx, move || {
                let mut last = None;
                while let Some(written) = saver.write_next(&storage, true) {
                    last = Some(written);
                }
                last
            });
            cx.spawn(async move |cx| {
                if let Some(written) = writing.await {
                    cx.update(|cx| show_save_outcome(written, cx));
                }
            })
            .detach();
        }
    }

    /// Writes the settings not stored yet; for quitting.
    pub fn flush_settings(cx: &mut App) -> Task<()> {
        let saver = cx.default_global::<SettingsSaver>().saver.clone();
        let storage = Self::get(cx).storage.clone();
        bridge::spawn_blocking(cx, move || {
            saver.write_next(&storage, false);
        })
    }
}

/// No settings write is pending or running, for tests.
#[cfg(test)]
pub fn settings_idle(cx: &mut App) -> bool {
    cx.default_global::<SettingsSaver>().saver.idle()
}

/// The notification of a settings save that failed.
struct SettingsNotSaved;

/// A failed save shows one notification, however many saves fail after it
/// (a slider dragged); the next save that succeeds takes it away (K8, C16).
fn show_save_outcome(written: Result<(), String>, cx: &mut App) {
    let failing = written.is_err();
    if cx.default_global::<SettingsSaver>().failure_shown == failing {
        return;
    }
    let Some(window) = cx.windows().into_iter().next() else {
        return;
    };
    let shown = window.update(cx, |_, window, cx| match written {
        Err(error) => window.push_notification(
            Notification::error(SharedString::from(format!(
                "Settings could not be saved: {error}. They apply until Kerno quits."
            )))
            .id::<SettingsNotSaved>()
            .autohide(false),
            cx,
        ),
        Ok(()) => window.remove_notification::<SettingsNotSaved>(cx),
    });
    if shown.is_ok() {
        cx.default_global::<SettingsSaver>().failure_shown = failing;
    }
}

/// The settings waiting to be stored.
#[derive(Default)]
struct Pending {
    /// The newest value not taken by a writer yet.
    latest: Option<Settings>,
    /// A background writer is running.
    writing: bool,
}

#[derive(Default)]
struct Saver {
    pending: Mutex<Pending>,
    /// Held while a value is taken and written, so values are written in
    /// the order they were taken.
    write: Mutex<()>,
}

impl Saver {
    /// Writes the newest pending value, and says how that went; `None` when
    /// none was pending (then, with `writer` set, the writer stops).
    fn write_next(&self, storage: &Storage, writer: bool) -> Option<Result<(), String>> {
        let _writing = self.write.lock().unwrap_or_else(|e| e.into_inner());
        let next = {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            let next = pending.latest.take();
            if next.is_none() && writer {
                pending.writing = false;
            }
            next
        };
        let settings = next?;
        Some(settings.save(storage).map_err(|error| {
            tracing::error!(%error, "could not store the settings");
            error.to_string()
        }))
    }

    #[cfg(test)]
    fn idle(&self) -> bool {
        let pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        !pending.writing && pending.latest.is_none()
    }
}

/// The one [`Saver`] of the app, and whether a failed save is on screen.
#[derive(Default)]
struct SettingsSaver {
    saver: Arc<Saver>,
    failure_shown: bool,
}

impl Global for SettingsSaver {}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use gpui_kit::TestAppContext;
    use kerno_core::secrets::MemorySecretStore;

    use super::*;

    /// C16: hidden connections are named, in the singular or the plural.
    #[test]
    fn unreadable_connections_are_named() {
        assert_eq!(unreadable_notice(0), None);
        let one = unreadable_notice(1).unwrap();
        assert!(
            one.starts_with("1 saved connection could not be read and is hidden."),
            "{one}"
        );
        assert!(one.contains("Nothing was deleted"), "{one}");
        let two = unreadable_notice(2).unwrap();
        assert!(
            two.starts_with("2 saved connections could not be read and are hidden."),
            "{two}"
        );
    }

    #[gpui_kit::test]
    fn quick_changes_reach_storage_in_order(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(Storage::in_memory().unwrap());
        cx.update(|cx| {
            bridge::init(cx).unwrap();
            cx.set_global(AppState {
                paths: AppPaths::under(dir.path().to_path_buf()),
                storage: storage.clone(),
                secrets: Arc::new(MemorySecretStore::default()),
                settings: Settings::default(),
                appearance_override: None,
            });
        });
        for round in 0..50u32 {
            let last = round * 100 + 19;
            for step in 0..20 {
                cx.update(|cx| {
                    AppState::update_settings(cx, |settings| {
                        settings.page_size = round * 100 + step;
                        // A folder made, then removed: it must not come back.
                        if step % 2 == 0 {
                            settings.connection_folders.insert("Gone".into());
                        } else {
                            settings.connection_folders.remove("Gone");
                        }
                    })
                });
            }
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                cx.run_until_parked();
                let idle = cx.update(|cx| cx.global::<SettingsSaver>().saver.idle());
                if idle {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "round {round}: the saves never ended"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            let stored = Settings::load(&storage);
            assert_eq!(stored.page_size, last, "round {round}");
            assert!(!stored.connection_folders.contains("Gone"), "round {round}");
        }

        // Quitting writes what is pending.
        cx.update(|cx| AppState::update_settings(cx, |settings| settings.page_size = 7));
        let done = Arc::new(Mutex::new(false));
        let flagged = done.clone();
        cx.update(|cx| {
            let flushing = AppState::flush_settings(cx);
            cx.spawn(async move |_| {
                flushing.await;
                *flagged.lock().unwrap() = true;
            })
            .detach();
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        while !*done.lock().unwrap() {
            assert!(Instant::now() < deadline, "the flush never ended");
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(Settings::load(&storage).page_size, 7);
    }
}
