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

/// A store read at most once per key for the life of the process: the
/// first `get` of a key asks `inner` and the answer is kept, "no such
/// secret" included; `set` and `delete` write through and update what is
/// kept. Over the OS store this keeps the keychain from asking again on
/// every connect. Values stay in memory, in [`Secret`]s that never print;
/// a failed read is not kept, so it can be tried again.
pub struct CachedSecretStore<S> {
    inner: S,
    known: Mutex<HashMap<String, Option<Secret>>>,
}

impl<S> CachedSecretStore<S> {
    /// Wraps `inner`.
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            known: Mutex::default(),
        }
    }

    fn known(&self) -> Result<std::sync::MutexGuard<'_, HashMap<String, Option<Secret>>>> {
        self.known
            .lock()
            .map_err(|_| Error::Secrets("the secret cache lock was poisoned by a panic".into()))
    }
}

impl<S> std::fmt::Debug for CachedSecretStore<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CachedSecretStore")
    }
}

impl<S: SecretStore> SecretStore for CachedSecretStore<S> {
    fn get(&self, key: &str) -> Result<Option<Secret>> {
        // ponytail: one lock for every key, held across the inner read, so
        // two connects starting together never prompt twice for one key.
        let mut known = self.known()?;
        if let Some(secret) = known.get(key) {
            return Ok(secret.clone());
        }
        let secret = self.inner.get(key)?;
        known.insert(key.to_string(), secret.clone());
        Ok(secret)
    }

    fn set(&self, key: &str, secret: &Secret) -> Result<()> {
        let mut known = self.known()?;
        known.remove(key);
        self.inner.set(key, secret)?;
        known.insert(key.to_string(), Some(secret.clone()));
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<()> {
        let mut known = self.known()?;
        known.remove(key);
        self.inner.delete(key)?;
        known.insert(key.to_string(), None);
        Ok(())
    }

    fn is_persistent(&self) -> bool {
        self.inner.is_persistent()
    }
}

/// The store the app uses: the OS store when it works, memory otherwise.
/// `KERNO_SECRET_STORE=memory` forces memory (used by tests and by checks
/// of the real app, so they never write to the user's keychain). The OS
/// store is read once per key per run ([`CachedSecretStore`]).
pub fn default_store() -> Arc<dyn SecretStore> {
    let forced_memory = std::env::var("KERNO_SECRET_STORE").is_ok_and(|value| value == "memory");
    if !forced_memory {
        match OsSecretStore::open() {
            Ok(store) => return Arc::new(CachedSecretStore::new(store)),
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

/// Whether the store holds any secret of the connection (its password, SSH
/// password or key passphrase): each key read at most once by a cached
/// store, and only the answer leaves this function.
pub fn has_secrets(store: &dyn SecretStore, connection_id: &str) -> Result<bool> {
    for kind in KINDS {
        if store.get(&key(connection_id, kind))?.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Stores the database password of a connection, leaving its SSH secrets
/// as they are.
pub fn save_password(
    store: &dyn SecretStore,
    connection_id: &str,
    password: &Secret,
) -> Result<()> {
    store.set(&key(connection_id, KINDS[0]), password)
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
    fn secrets_are_looked_up_and_a_password_stored_alone() {
        let store = MemorySecretStore::default();
        let ssh = Secrets {
            ssh_key_passphrase: Some(Secret::new("pp")),
            ..Secrets::default()
        };
        assert!(!has_secrets(&store, "c").unwrap());
        // A passphrase alone counts: it would follow the connection's id.
        save_secrets(&store, "c", &ssh).unwrap();
        assert!(has_secrets(&store, "c").unwrap());
        save_password(&store, "c", &Secret::new("pw")).unwrap();
        let loaded = load_secrets(&store, "c").unwrap();
        assert_eq!(loaded.password, Some(Secret::new("pw")));
        assert_eq!(loaded.ssh_key_passphrase, Some(Secret::new("pp")));
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
        let cached = CachedSecretStore::new(store);
        assert_eq!(cached.get("k").unwrap(), Some(Secret::new("hunter2")));
        assert!(!format!("{cached:?}").contains("hunter2"));
    }

    /// Counts the reads that reach the store beneath a cache.
    #[derive(Default)]
    struct Counting {
        store: MemorySecretStore,
        reads: Mutex<HashMap<String, usize>>,
    }

    impl Counting {
        fn reads(&self, key: &str) -> usize {
            self.reads.lock().unwrap().get(key).copied().unwrap_or(0)
        }
    }

    impl SecretStore for Counting {
        fn get(&self, key: &str) -> Result<Option<Secret>> {
            *self.reads.lock().unwrap().entry(key.into()).or_default() += 1;
            self.store.get(key)
        }
        fn set(&self, key: &str, secret: &Secret) -> Result<()> {
            self.store.set(key, secret)
        }
        fn delete(&self, key: &str) -> Result<()> {
            self.store.delete(key)
        }
        fn is_persistent(&self) -> bool {
            true
        }
    }

    #[test]
    fn a_cached_store_reads_each_key_once() {
        let inner = Counting::default();
        inner.set("present", &Secret::new("pw")).unwrap();
        let store = CachedSecretStore::new(inner);
        assert!(store.is_persistent());
        for _ in 0..3 {
            assert_eq!(store.get("present").unwrap(), Some(Secret::new("pw")));
            // "No such secret" is kept too.
            assert_eq!(store.get("absent").unwrap(), None);
        }
        assert_eq!(store.inner.reads("present"), 1);
        assert_eq!(store.inner.reads("absent"), 1);

        // Writes go through and are what later reads see, without a read.
        store.set("absent", &Secret::new("new")).unwrap();
        assert_eq!(
            store.inner.store.get("absent").unwrap(),
            Some(Secret::new("new"))
        );
        assert_eq!(store.get("absent").unwrap(), Some(Secret::new("new")));
        store.delete("present").unwrap();
        assert_eq!(store.inner.store.get("present").unwrap(), None);
        assert_eq!(store.get("present").unwrap(), None);
        assert_eq!(store.inner.reads("present"), 1);
        assert_eq!(store.inner.reads("absent"), 1);

        // The whole contract of a store holds through the cache.
        exercise(
            &CachedSecretStore::new(MemorySecretStore::default()),
            "cached",
        );
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
