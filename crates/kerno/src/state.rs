//! What the whole app shares: where its files are, its storage, the secret
//! store and the settings.

use std::sync::{Arc, Mutex};

use gpui_kit::{App, BorrowAppContext as _, Global, Task};
use kerno_core::secrets::{self, SecretStore};
use kerno_core::storage::{AppPaths, SavedConnection, Storage};

use crate::bridge;
use crate::settings::Settings;
use crate::theme::Appearance;

/// Storage key of the index of the active tab.
pub const ACTIVE_TAB_KEY: &str = "tabs.active";

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
    pub tabs: Vec<crate::tabs::TabSnapshot>,
    /// Index of the tab that was active.
    pub active_tab: usize,
}

impl Startup {
    /// Opens the store and reads what the first frame needs.
    pub fn load() -> anyhow::Result<Self> {
        let paths = AppPaths::locate()?;
        let storage = Arc::new(Storage::open(&paths.database())?);
        let settings = Settings::load(&storage);
        let connections = storage.connections()?;
        let tabs = storage.tabs()?;
        let active_tab = storage.setting(ACTIVE_TAB_KEY)?.unwrap_or(0);
        Ok(Self {
            paths,
            storage,
            settings,
            connections,
            tabs,
            active_tab,
        })
    }
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
        let saver = cx.default_global::<SettingsSaver>().0.clone();
        let start = {
            let mut pending = saver.pending.lock().unwrap_or_else(|e| e.into_inner());
            pending.latest = Some(settings);
            !std::mem::replace(&mut pending.writing, true)
        };
        // One writer at a time; it writes the newest value until none is
        // left, so a change made while it writes is written after.
        if start {
            bridge::spawn_blocking(cx, move || while saver.write_next(&storage, true) {}).detach();
        }
    }

    /// Writes the settings not stored yet; for quitting.
    pub fn flush_settings(cx: &mut App) -> Task<()> {
        let saver = cx.default_global::<SettingsSaver>().0.clone();
        let storage = Self::get(cx).storage.clone();
        bridge::spawn_blocking(cx, move || {
            saver.write_next(&storage, false);
        })
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
    /// Writes the newest pending value. Returns whether one was written;
    /// when none is left and `writer` is set, the writer stops.
    fn write_next(&self, storage: &Storage, writer: bool) -> bool {
        let _writing = self.write.lock().unwrap_or_else(|e| e.into_inner());
        let next = {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            let next = pending.latest.take();
            if next.is_none() && writer {
                pending.writing = false;
            }
            next
        };
        let Some(settings) = next else {
            return false;
        };
        if let Err(error) = settings.save(storage) {
            tracing::error!(%error, "could not store the settings");
        }
        true
    }

    #[cfg(test)]
    fn idle(&self) -> bool {
        let pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        !pending.writing && pending.latest.is_none()
    }
}

/// The one [`Saver`] of the app.
#[derive(Default)]
struct SettingsSaver(Arc<Saver>);

impl Global for SettingsSaver {}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use gpui_kit::TestAppContext;
    use kerno_core::secrets::MemorySecretStore;

    use super::*;

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
                let idle = cx.update(|cx| cx.global::<SettingsSaver>().0.idle());
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
