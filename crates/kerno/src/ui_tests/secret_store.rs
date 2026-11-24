//! K33 (D-106): Settings ▸ Passwords chooses where saved passwords are kept, says honestly
//! what each place means, is stored, and changes the store for the next read without a
//! restart. The stores built here are Kerno's file in the run's scratch data directory and
//! memory standing for the keychain: nothing reaches the real keychain.

use kerno_core::Secret;
use kerno_core::secrets::{FileSecretStore, MemorySecretStore, SecretStore, StoreChoice};

use super::tool_wizards::paints;
use super::*;

/// Kerno's file in the data directory; the "keychain" is memory, so that the switch shows in
/// the status bar and nothing touches the real one.
fn stores(choice: StoreChoice, dir: &Path) -> Arc<dyn SecretStore> {
    match choice {
        StoreChoice::File => Arc::new(FileSecretStore::new(dir)),
        StoreChoice::Keychain => Arc::new(MemorySecretStore::default()),
    }
}

fn store_name(cx: &mut TestAppContext) -> &'static str {
    cx.update(|cx| AppState::get(cx).secrets.name())
}

fn stored_choice(app: &Fixture, cx: &mut TestAppContext) -> StoreChoice {
    app.wait_until(cx, "the settings saved", |_, cx| {
        cx.update(crate::state::settings_idle)
    });
    Settings::load(&app.storage).secret_store
}

/// Mutations seen: the chip's click not calling `set_secret_store` (the store stays memory,
/// the status bar keeps its warning), and `set_secret_store` not rebuilding the store.
#[gpui_kit::test]
fn the_passwords_setting_shows_changes_and_is_stored(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    cx.update(|cx| cx.global_mut::<AppState>().secret_stores = stores);
    // Kerno's file by default on macOS, the Secret Service elsewhere (unchanged).
    let default = Settings::default().secret_store;
    assert_eq!(
        default,
        if cfg!(target_os = "macos") {
            StoreChoice::File
        } else {
            StoreChoice::Keychain
        }
    );

    app.dispatch(cx, actions::OpenSettings);
    let index = crate::settings_view::SECTIONS
        .iter()
        .position(|(_, label)| *label == "Passwords")
        .unwrap();
    app.click(cx, ("settings-section", index));
    // Its row, one chip per place, and the line saying what wins over it.
    assert!(app.has(cx, "row-settings-secret-store"));
    assert!(app.has(cx, "secret-store-0") && app.has(cx, "secret-store-1"));
    assert_eq!(StoreChoice::File.label(), "Kerno's file");
    assert!(paints(&app, cx, "KERNO_SECRET_STORE wins"));
    // The honest sentence under it (the row's description, drawn as declared): what the file
    // protects and what it does not, and the keychain's prompt after an update on macOS.
    let row = crate::settings_view::ROWS
        .iter()
        .find(|row| row.key == "settings-secret-store")
        .unwrap();
    assert_eq!(row.label, "Keep saved passwords in");
    let said = row
        .description
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut parts = vec![
        "one encrypted file in Kerno's data folder, its key in a second file beside it",
        "a program running under your account could read both files",
        "A password saved in the other place moves here the first time it is used",
    ];
    if cfg!(target_os = "macos") {
        parts.extend([
            "macOS Keychain: guarded by your login",
            "asks for your password again after every update",
        ]);
    } else if cfg!(windows) {
        parts.push("Windows Credential Manager: guarded by your Windows login");
    } else {
        parts.push("Secret Service: GNOME Keyring or KWallet");
    }
    for part in parts {
        assert!(said.contains(part), "{part}: {said}");
    }

    // The keychain chosen: stored, and the next read uses it (here memory, which the status
    // bar says) without a restart.
    app.click(cx, "secret-store-1");
    assert_eq!(settings_now(cx).secret_store, StoreChoice::Keychain);
    assert_eq!(stored_choice(&app, cx), StoreChoice::Keychain);
    assert_eq!(store_name(cx), "memory");
    assert!(app.has(cx, "status-memory-only"));

    // Back to Kerno's file: a password saved now is in the file of the data directory.
    app.click(cx, "secret-store-0");
    assert_eq!(settings_now(cx).secret_store, StoreChoice::File);
    assert_eq!(stored_choice(&app, cx), StoreChoice::File);
    assert_eq!(store_name(cx), "Kerno's file");
    assert!(!app.has(cx, "status-memory-only"));
    let (secrets, dir) = cx.update(|cx| {
        let state = AppState::get(cx);
        (state.secrets.clone(), state.paths.data_dir.clone())
    });
    secrets.set("k33-ui", &Secret::new("pw")).unwrap();
    assert_eq!(
        FileSecretStore::new(&dir).get("k33-ui").unwrap(),
        Some(Secret::new("pw"))
    );
}
