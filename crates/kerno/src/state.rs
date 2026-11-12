//! What the whole app shares: where its files are, its storage, the secret
//! store and the settings.

use std::sync::Arc;

use gpui_kit::{App, BorrowAppContext as _, Global};
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

    /// Changes the settings and stores them in the background.
    pub fn update_settings(cx: &mut App, change: impl FnOnce(&mut Settings)) {
        let (storage, settings) = cx.update_global::<AppState, _>(|state, _| {
            change(&mut state.settings);
            (state.storage.clone(), state.settings.clone())
        });
        bridge::spawn_blocking(cx, move || {
            if let Err(error) = settings.save(&storage) {
                tracing::error!(%error, "could not store the settings");
            }
        })
        .detach();
    }
}
