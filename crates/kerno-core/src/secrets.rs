//! Where passwords and passphrases live: the operating system's secret
//! store (macOS Keychain, Linux Secret Service), never Kerno's own files.
//!
//! When no secret store is available, [`MemorySecretStore`] keeps secrets
//! for the life of the process only, and the app asks for them at connect.
//! Store calls can block (the OS may ask the user to unlock the keychain):
//! callers run them off the UI thread.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::config::{Secret, Secrets};
use crate::error::{Error, Result};

/// The service name Kerno's entries are filed under in the OS store.
const SERVICE: &str = "Kerno";

/// A place to keep secrets, addressed by key.
pub trait SecretStore: Send + Sync {
    /// The secret stored under `key`, or `None`.
    fn get(&self, key: &str) -> Result<Option<Secret>>;

    /// Stores `secret` under `key`, replacing what was there.
    fn set(&self, key: &str, secret: &Secret) -> Result<()>;

    /// Removes the secret under `key`. Removing nothing is not an error.
    fn delete(&self, key: &str) -> Result<()>;

    /// Whether secrets outlive the process. `false` means memory only: the
    /// app must say so and ask for passwords at connect.
    fn is_persistent(&self) -> bool;
}

/// The operating system's secret store.
#[derive(Debug, Clone, Copy, Default)]
pub struct OsSecretStore;

impl OsSecretStore {
    /// The OS store, or the reason it cannot be used on this machine (no
    /// Secret Service running, for example).
    pub fn open() -> Result<Self> {
        match keyring::Entry::store_status() {
            Ok(()) => Ok(Self),
            Err(error) => Err(secret_error(
                "the system secret store is not available",
                error,
            )),
        }
    }

    fn entry(key: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(SERVICE, key)
            .map_err(|error| secret_error("could not address the system secret store", &error))
    }
}

/// Describes a store failure without ever including a secret: keyring's
/// errors carry platform messages and, for one variant, the raw bytes of a
/// secret that is not valid UTF-8, which must not be printed.
fn secret_error(context: &str, error: &keyring::Error) -> Error {
    let detail = match error {
        keyring::Error::BadEncoding(_) => "the stored secret is not valid text".to_string(),
        other => other.to_string(),
    };
    Error::Secrets(format!("{context}: {detail}"))
}

impl SecretStore for OsSecretStore {
    fn get(&self, key: &str) -> Result<Option<Secret>> {
        match Self::entry(key)?.get_password() {
            Ok(password) => Ok(Some(Secret::new(password))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(secret_error(
                "could not read from the system secret store",
                &error,
            )),
        }
    }

    fn set(&self, key: &str, secret: &Secret) -> Result<()> {
        Self::entry(key)?
            .set_password(secret.expose())
            .map_err(|error| secret_error("could not write to the system secret store", &error))
    }

    fn delete(&self, key: &str) -> Result<()> {
        match Self::entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(secret_error(
                "could not delete from the system secret store",
                &error,
            )),
        }
    }

    fn is_persistent(&self) -> bool {
        true
    }
}

/// Secrets kept in memory for the life of the process.
#[derive(Default)]
pub struct MemorySecretStore {
    secrets: Mutex<HashMap<String, Secret>>,
}

impl std::fmt::Debug for MemorySecretStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MemorySecretStore")
    }
}

impl MemorySecretStore {
    fn map(&self) -> Result<std::sync::MutexGuard<'_, HashMap<String, Secret>>> {
        self.secrets
            .lock()
            .map_err(|_| Error::Secrets("the secret store lock was poisoned by a panic".into()))
    }
}

impl SecretStore for MemorySecretStore {
    fn get(&self, key: &str) -> Result<Option<Secret>> {
        Ok(self.map()?.get(key).cloned())
    }

    fn set(&self, key: &str, secret: &Secret) -> Result<()> {
        self.map()?.insert(key.to_string(), secret.clone());
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<()> {
        self.map()?.remove(key);
        Ok(())
    }

    fn is_persistent(&self) -> bool {
        false
    }
}

/// The store the app uses: the OS store when it works, memory otherwise.
/// `KERNO_SECRET_STORE=memory` forces memory (used by tests and by checks
/// of the real app, so they never write to the user's keychain).
pub fn default_store() -> Arc<dyn SecretStore> {
    let forced_memory = std::env::var("KERNO_SECRET_STORE").is_ok_and(|value| value == "memory");
    if !forced_memory {
        match OsSecretStore::open() {
            Ok(store) => return Arc::new(store),
            Err(error) => {
                tracing::warn!(%error, "secrets will be kept in memory only");
            }
        }
    }
    Arc::new(MemorySecretStore::default())
}

/// The three secrets a connection can have.
const KINDS: [&str; 3] = ["password", "ssh-password", "ssh-key-passphrase"];

fn key(connection_id: &str, kind: &str) -> String {
    format!("connection/{connection_id}/{kind}")
}

/// Reads the secrets of a connection.
pub fn load_secrets(store: &dyn SecretStore, connection_id: &str) -> Result<Secrets> {
    Ok(Secrets {
        password: store.get(&key(connection_id, KINDS[0]))?,
        ssh_password: store.get(&key(connection_id, KINDS[1]))?,
        ssh_key_passphrase: store.get(&key(connection_id, KINDS[2]))?,
    })
}

/// Stores the secrets of a connection. A secret that is `None` is removed,
/// so clearing a password field really forgets the password.
pub fn save_secrets(store: &dyn SecretStore, connection_id: &str, secrets: &Secrets) -> Result<()> {
    let values = [
        &secrets.password,
        &secrets.ssh_password,
        &secrets.ssh_key_passphrase,
    ];
    for (kind, value) in KINDS.iter().zip(values) {
        let key = key(connection_id, kind);
        match value {
            Some(secret) => store.set(&key, secret)?,
            None => store.delete(&key)?,
        }
    }
    Ok(())
}

/// Removes every secret of a connection.
pub fn delete_secrets(store: &dyn SecretStore, connection_id: &str) -> Result<()> {
    save_secrets(store, connection_id, &Secrets::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exercise(store: &dyn SecretStore, id: &str) {
        assert_eq!(load_secrets(store, id).unwrap(), Secrets::default());

        let secrets = Secrets {
            password: Some(Secret::new("db-pw \u{e9}\u{1F600}")),
            ssh_password: None,
            ssh_key_passphrase: Some(Secret::new("")),
        };
        save_secrets(store, id, &secrets).unwrap();
        assert_eq!(load_secrets(store, id).unwrap(), secrets);

        // Another connection sees nothing of it.
        assert_eq!(
            load_secrets(store, &format!("{id}-other")).unwrap(),
            Secrets::default()
        );

        // Replacing, and clearing one field forgets that secret.
        let changed = Secrets {
            password: Some(Secret::new("new")),
            ssh_password: Some(Secret::new("ssh")),
            ssh_key_passphrase: None,
        };
        save_secrets(store, id, &changed).unwrap();
        assert_eq!(load_secrets(store, id).unwrap(), changed);

        delete_secrets(store, id).unwrap();
        assert_eq!(load_secrets(store, id).unwrap(), Secrets::default());
        // Deleting what is not there is fine.
        delete_secrets(store, id).unwrap();
    }

    #[test]
    fn memory_store_keeps_and_forgets() {
        let store = MemorySecretStore::default();
        assert!(!store.is_persistent());
        exercise(&store, "test-connection");
    }

    #[test]
    fn stores_never_print_their_secrets() {
        let store = MemorySecretStore::default();
        store.set("k", &Secret::new("hunter2")).unwrap();
        assert!(!format!("{store:?}").contains("hunter2"));
        assert!(!format!("{:?}", store.get("k").unwrap()).contains("hunter2"));
    }

    /// Writes to, reads from and cleans up the real OS secret store. Opt-in
    /// (`KERNO_TEST_OS_SECRET_STORE=1`) because it touches the user's
    /// keychain and the OS may show an unlock prompt.
    #[test]
    fn os_store_keeps_and_forgets() {
        if std::env::var_os("KERNO_TEST_OS_SECRET_STORE").is_none() {
            eprintln!("SKIPPED: set KERNO_TEST_OS_SECRET_STORE=1 to test the system secret store");
            return;
        }
        let store = OsSecretStore::open().expect("the system secret store");
        assert!(store.is_persistent());
        exercise(&store, &format!("kerno-self-test-{}", std::process::id()));
    }
}
