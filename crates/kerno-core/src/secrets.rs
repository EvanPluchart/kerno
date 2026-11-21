//! Where passwords and passphrases live (K33, D-106): Kerno's own encrypted
//! file ([`FileSecretStore`], the default on macOS) or the operating
//! system's store ([`OsSecretStore`]: macOS Keychain, Linux Secret Service,
//! the default on Linux), as Settings ▸ Passwords says. Never in plain text.
//!
//! Kerno's file keeps the passwords out of plain sight (a backup or a copy
//! of the file, a file search, a shared screen, another user of the
//! machine), not from a program running as the same user, which can read
//! its key file too: the level of DBeaver, `~/.pgpass` and an SSH key
//! without a passphrase. The system store is guarded by the user's login,
//! but macOS asks for the login password again, once per item, after every
//! update of a build that is not signed with a Developer ID.
//!
//! [`ChosenStore`] uses one and empties the other into it: a secret found
//! only in the other store is moved the first time it is read, once
//! ([`app_store`]). When no store can be used, [`MemorySecretStore`] keeps
//! secrets for the life of the process only, and the app asks for them at
//! connect. Store calls can block (the OS may ask the user to unlock the
//! keychain): callers run them off the UI thread.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};

use serde::{Deserialize, Serialize};

use crate::config::{Secret, Secrets};
use crate::error::{Error, Result};

mod file;

pub use file::{FILE_PROBLEM, FileSecretStore, KEY_FILE, Lookup, SECRETS_FILE};

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

    /// The secret under `key` in the store in use, without moving one from
    /// another store: for a question that is not a connect (whether an
    /// imported connection has saved secrets), which must not make the
    /// keychain ask.
    fn peek(&self, key: &str) -> Result<Option<Secret>> {
        self.get(key)
    }

    /// Where its secrets are, in the app's words ("Kerno's file").
    fn name(&self) -> &'static str {
        SYSTEM_STORE_NAME
    }
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

    fn name(&self) -> &'static str {
        "memory"
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

    fn name(&self) -> &'static str {
        self.inner.name()
    }
}

/// The name of the system store, as Settings shows it.
pub const SYSTEM_STORE_NAME: &str = if cfg!(target_os = "macos") {
    "macOS Keychain"
} else {
    "Secret Service"
};

/// Where the saved secrets are kept (Settings ▸ Passwords).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StoreChoice {
    /// Kerno's own encrypted file.
    File,
    /// The system store ([`SYSTEM_STORE_NAME`]).
    Keychain,
}

impl Default for StoreChoice {
    /// Kerno's file on macOS, where an unsigned build makes the keychain
    /// ask after every update; the Secret Service elsewhere, as before.
    fn default() -> Self {
        if cfg!(target_os = "macos") {
            Self::File
        } else {
            Self::Keychain
        }
    }
}

impl StoreChoice {
    pub const ALL: [StoreChoice; 2] = [Self::File, Self::Keychain];

    pub fn label(self) -> &'static str {
        match self {
            Self::File => "Kerno's file",
            Self::Keychain => SYSTEM_STORE_NAME,
        }
    }
}

/// One store in use, the other emptied into it; each move happens once.
///
/// With Kerno's file in use, a key the file has never seen is read from the
/// system store once and its answer written in the file: the secret, or a
/// mark that there is none. From then on the system store is never read for
/// it, and never written or deleted (a delete there could make the keychain
/// ask): a removed secret is a mark in the file, which keeps an older copy
/// in the system store from coming back. A system store that refuses the
/// read counts as having nothing: the app then asks for the password and
/// keeps the one typed in the file.
///
/// With the system store in use, the file is drained: a secret found there
/// (newer: it was written while the file was in use) moves to the system
/// store, a mark deletes the system store's copy, and every write removes
/// the key from the file. Switching back to the file then reads the system
/// store once more, and gets its latest value.
pub struct ChosenStore {
    choice: StoreChoice,
    file: FileSecretStore,
    system: Arc<dyn SecretStore>,
}

impl ChosenStore {
    pub fn new(choice: StoreChoice, file: FileSecretStore, system: Arc<dyn SecretStore>) -> Self {
        Self {
            choice,
            file,
            system,
        }
    }
}

impl std::fmt::Debug for ChosenStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ChosenStore({:?})", self.choice)
    }
}

impl SecretStore for ChosenStore {
    fn get(&self, key: &str) -> Result<Option<Secret>> {
        match (self.choice, self.file.lookup(key)?) {
            (StoreChoice::File, Lookup::Value(secret)) => Ok(Some(secret)),
            (StoreChoice::File, Lookup::Gone) => Ok(None),
            (StoreChoice::File, Lookup::Unknown) => {
                let found = self.system.get(key).unwrap_or_else(|error| {
                    tracing::warn!(%error, "the system secret store could not be read once");
                    None
                });
                match &found {
                    Some(secret) => self.file.put(key, secret)?,
                    None => self.file.mark_gone(key)?,
                }
                Ok(found)
            }
            (StoreChoice::Keychain, Lookup::Value(secret)) => {
                self.system.set(key, &secret)?;
                self.file.forget(key)?;
                Ok(Some(secret))
            }
            (StoreChoice::Keychain, Lookup::Gone) => {
                self.system.delete(key)?;
                self.file.forget(key)?;
                Ok(None)
            }
            (StoreChoice::Keychain, Lookup::Unknown) => self.system.get(key),
        }
    }

    fn set(&self, key: &str, secret: &Secret) -> Result<()> {
        match self.choice {
            StoreChoice::File => self.file.put(key, secret),
            StoreChoice::Keychain => {
                self.system.set(key, secret)?;
                self.file.forget(key)
            }
        }
    }

    fn delete(&self, key: &str) -> Result<()> {
        match self.choice {
            StoreChoice::File => self.file.mark_gone(key),
            StoreChoice::Keychain => {
                self.system.delete(key)?;
                self.file.forget(key)
            }
        }
    }

    fn is_persistent(&self) -> bool {
        match self.choice {
            StoreChoice::File => true,
            StoreChoice::Keychain => self.system.is_persistent(),
        }
    }

    fn peek(&self, key: &str) -> Result<Option<Secret>> {
        match self.choice {
            StoreChoice::File => self.file.get(key),
            StoreChoice::Keychain => self.system.get(key),
        }
    }

    fn name(&self) -> &'static str {
        match self.choice {
            StoreChoice::File => self.file.name(),
            StoreChoice::Keychain => self.system.name(),
        }
    }
}

/// The system store, read once per key per run and shared by every store
/// the app builds in a run: switching the setting back and forth does not
/// read a key again. Creating it touches nothing; the first read does.
static SYSTEM: LazyLock<Arc<dyn SecretStore>> =
    LazyLock::new(|| Arc::new(CachedSecretStore::new(OsSecretStore)));

/// The store the app uses for `choice`, Kerno's file in `dir` (the data
/// directory). `KERNO_SECRET_STORE` wins over the setting and gives one
/// store alone, nothing moved from the other: `memory` (tests and checks of
/// the real app), `file` (Kerno's file, the keychain never touched) or
/// `keychain` (the system store; memory when there is none). The system
/// store chosen but not available (no Secret Service running) is memory, as
/// before K33.
pub fn app_store(choice: StoreChoice, dir: &Path) -> Arc<dyn SecretStore> {
    match std::env::var("KERNO_SECRET_STORE").as_deref() {
        Ok("memory") => return memory(),
        Ok("file") => return Arc::new(FileSecretStore::new(dir)),
        Ok("keychain") => return system().unwrap_or_else(memory),
        Ok(other) => tracing::warn!(value = other, "KERNO_SECRET_STORE ignored"),
        Err(_) => {}
    }
    let file = FileSecretStore::new(dir);
    match choice {
        // Not opened: only a read of a key the file has never seen reaches
        // the system store.
        StoreChoice::File => Arc::new(ChosenStore::new(choice, file, SYSTEM.clone())),
        StoreChoice::Keychain => match system() {
            Some(system) => Arc::new(ChosenStore::new(choice, file, system)),
            None => memory(),
        },
    }
}

/// The shared system store, when this machine has one.
fn system() -> Option<Arc<dyn SecretStore>> {
    match OsSecretStore::open() {
        Ok(_) => Some(SYSTEM.clone()),
        Err(error) => {
            tracing::warn!(%error, "secrets will be kept in memory only");
            None
        }
    }
}

fn memory() -> Arc<dyn SecretStore> {
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

/// Whether the store in use holds any secret of the connection (its
/// password, SSH password or key passphrase), nothing moved from another
/// store ([`SecretStore::peek`]): each key read at most once by a cached
/// store, and only the answer leaves this function.
pub fn has_secrets(store: &dyn SecretStore, connection_id: &str) -> Result<bool> {
    for kind in KINDS {
        if store.peek(&key(connection_id, kind))?.is_some() {
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

    /// Counts the reads that reach the store beneath a cache, and the
    /// writes and deletes: the stand-in for the keychain.
    #[derive(Default)]
    struct Counting {
        store: MemorySecretStore,
        reads: Mutex<HashMap<String, usize>>,
        changes: Mutex<usize>,
    }

    impl Counting {
        fn reads(&self, key: &str) -> usize {
            self.reads.lock().unwrap().get(key).copied().unwrap_or(0)
        }
        fn changes(&self) -> usize {
            *self.changes.lock().unwrap()
        }
    }

    impl SecretStore for Counting {
        fn get(&self, key: &str) -> Result<Option<Secret>> {
            *self.reads.lock().unwrap().entry(key.into()).or_default() += 1;
            self.store.get(key)
        }
        fn set(&self, key: &str, secret: &Secret) -> Result<()> {
            *self.changes.lock().unwrap() += 1;
            self.store.set(key, secret)
        }
        fn delete(&self, key: &str) -> Result<()> {
            *self.changes.lock().unwrap() += 1;
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

    // Kerno's file (K33, D-106) ------------------------------------------------

    use std::os::unix::fs::PermissionsExt as _;

    const SECRET_A: &str = "kerno-k33-alpha-\u{e9}t\u{e9}";
    const SECRET_B: &str = "kerno-k33-bravo-pass";

    fn raw(dir: &Path) -> Vec<Vec<u8>> {
        file::paths(dir)
            .iter()
            .map(|path| std::fs::read(path).unwrap_or_default())
            .collect()
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    #[test]
    fn the_file_keeps_and_forgets_across_runs() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(dir.path());
        assert!(store.is_persistent());
        exercise(&store, "file-connection");
        // Several secrets, read back by another instance (the next run).
        store.set("a", &Secret::new(SECRET_A)).unwrap();
        store.set("b", &Secret::new(SECRET_B)).unwrap();
        let next_run = FileSecretStore::new(dir.path());
        assert_eq!(next_run.get("a").unwrap(), Some(Secret::new(SECRET_A)));
        assert_eq!(next_run.get("b").unwrap(), Some(Secret::new(SECRET_B)));
        assert_eq!(next_run.get("c").unwrap(), None);
    }

    /// Mutation seen: `write` storing `plain` instead of `sealed`.
    #[test]
    fn no_file_holds_a_secret_in_clear() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(dir.path());
        store
            .set("connection/x/password", &Secret::new(SECRET_A))
            .unwrap();
        store
            .set("connection/x/ssh-password", &Secret::new(SECRET_B))
            .unwrap();
        store.delete("connection/x/ssh-password").unwrap();
        for bytes in raw(dir.path()) {
            for needle in [SECRET_A, SECRET_B, "connection/x"] {
                assert!(!contains(&bytes, needle.as_bytes()), "{needle} in clear");
            }
        }
        assert_eq!(
            store.get("connection/x/password").unwrap(),
            Some(Secret::new(SECRET_A))
        );
    }

    /// Mutation seen: `.mode(0o600)` dropped from `replace` and `lock`.
    #[test]
    fn every_file_is_readable_by_its_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        FileSecretStore::new(dir.path())
            .set("a", &Secret::new(SECRET_A))
            .unwrap();
        for path in file::paths(dir.path()) {
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{}", path.display());
        }
    }

    /// A file that does not decrypt is set aside (never overwritten), said
    /// in plain words without a secret, once; the next call starts afresh.
    /// Mutation seen: `set_aside` returning its error without renaming.
    #[test]
    fn a_damaged_file_is_set_aside_and_said_in_plain_words() {
        type Damage = fn(&Path);
        let damages: [(&str, Damage); 5] = [
            ("one byte changed", |dir| {
                let path = dir.join(SECRETS_FILE);
                let mut bytes = std::fs::read(&path).unwrap();
                let last = bytes.len() - 1;
                bytes[last] ^= 1;
                std::fs::write(&path, bytes).unwrap();
            }),
            ("cut short", |dir| {
                let path = dir.join(SECRETS_FILE);
                let bytes = std::fs::read(&path).unwrap();
                std::fs::write(&path, &bytes[..10]).unwrap();
            }),
            ("another key", |dir| {
                std::fs::write(dir.join(KEY_FILE), [7u8; 32]).unwrap();
            }),
            ("a key of the wrong size", |dir| {
                std::fs::write(dir.join(KEY_FILE), [7u8; 5]).unwrap();
            }),
            ("no key file", |dir| {
                std::fs::remove_file(dir.join(KEY_FILE)).unwrap();
            }),
        ];
        for (what, damage) in damages {
            let dir = tempfile::tempdir().unwrap();
            let store = FileSecretStore::new(dir.path());
            store.set("a", &Secret::new(SECRET_A)).unwrap();
            damage(dir.path());
            let damaged = std::fs::read(dir.path().join(SECRETS_FILE)).unwrap();
            let message = store.get("a").unwrap_err().to_string();
            assert!(
                message.starts_with(&format!("secrets: {FILE_PROBLEM} could not be read: ")),
                "{what}: {message}"
            );
            assert!(
                message.contains("enter the password again"),
                "{what}: {message}"
            );
            assert!(!message.contains(SECRET_A), "{what}: {message}");
            // Kept aside, as it was; the next read finds nothing, and a new
            // secret is kept in a new file.
            let aside = dir.path().join(format!("{SECRETS_FILE}.damaged"));
            assert_eq!(std::fs::read(&aside).unwrap(), damaged, "{what}");
            assert!(
                message.contains(&format!("{SECRETS_FILE}.damaged")),
                "{what}"
            );
            assert_eq!(store.get("a").unwrap(), None, "{what}");
            store.set("b", &Secret::new(SECRET_B)).unwrap();
            assert_eq!(
                store.get("b").unwrap(),
                Some(Secret::new(SECRET_B)),
                "{what}"
            );
            // A second damaged file does not replace the first one.
            damage(dir.path());
            assert!(store.set("c", &Secret::new("c")).is_err(), "{what}");
            assert_eq!(std::fs::read(&aside).unwrap(), damaged, "{what}");
            assert!(
                dir.path()
                    .join(format!("{SECRETS_FILE}-2.damaged"))
                    .exists(),
                "{what}"
            );
        }
    }

    /// The previous content survives a write that fails, and a temporary
    /// file left by a crash changes nothing. Mutation seen: `replace`
    /// writing the file in place (`std::fs::write`).
    #[test]
    fn a_failed_write_keeps_the_previous_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(dir.path());
        store.set("a", &Secret::new(SECRET_A)).unwrap();
        store.set("b", &Secret::new(SECRET_B)).unwrap();
        let read_only = std::fs::Permissions::from_mode(0o500);
        std::fs::set_permissions(dir.path(), read_only).unwrap();
        let failed = store.set("a", &Secret::new("changed"));
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let message = failed.unwrap_err().to_string();
        assert!(
            message.contains("could not be written") && message.contains("saved before are kept"),
            "{message}"
        );
        assert_eq!(store.get("a").unwrap(), Some(Secret::new(SECRET_A)));
        assert_eq!(store.get("b").unwrap(), Some(Secret::new(SECRET_B)));

        std::fs::write(dir.path().join("secrets.kerno.tmp"), b"half a write").unwrap();
        assert_eq!(store.get("a").unwrap(), Some(Secret::new(SECRET_A)));
        store.set("c", &Secret::new("c")).unwrap();
        assert_eq!(store.get("b").unwrap(), Some(Secret::new(SECRET_B)));
        assert!(!dir.path().join("secrets.kerno.tmp").exists());
    }

    /// Two stores on one directory (two Kerno processes) keep each other's
    /// secrets, also when they write at the same time. Mutation seen: the
    /// `file.lock()` of `lock` removed (the threads then lose secrets).
    #[test]
    fn two_stores_on_one_directory_keep_each_others_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let (one, two) = (
            FileSecretStore::new(dir.path()),
            FileSecretStore::new(dir.path()),
        );
        one.set("one", &Secret::new("1")).unwrap();
        two.set("two", &Secret::new("2")).unwrap();
        assert_eq!(one.get("two").unwrap(), Some(Secret::new("2")));
        assert_eq!(two.get("one").unwrap(), Some(Secret::new("1")));
        std::thread::scope(|scope| {
            for (name, store) in [("x", &one), ("y", &two)] {
                scope.spawn(move || {
                    for index in 0..25 {
                        store
                            .set(&format!("{name}{index}"), &Secret::new("v"))
                            .unwrap();
                    }
                });
            }
        });
        let reader = FileSecretStore::new(dir.path());
        for name in ["x", "y"] {
            for index in 0..25 {
                let key = format!("{name}{index}");
                assert_eq!(reader.get(&key).unwrap(), Some(Secret::new("v")), "{key}");
            }
        }
    }

    /// A `Secret` is a `String`: a secret that is not UTF-8 cannot reach a
    /// store. Every other character, and a long one, round-trips.
    #[test]
    fn unusual_and_long_secrets_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(dir.path());
        let long = "x\u{1F600}".repeat(200_000);
        let odd = "nul \0 quote \" back \\ line\nend \u{202e}";
        store.set("long", &Secret::new(long.clone())).unwrap();
        store.set("odd", &Secret::new(odd)).unwrap();
        store.set("empty", &Secret::new("")).unwrap();
        let next_run = FileSecretStore::new(dir.path());
        assert_eq!(next_run.get("long").unwrap(), Some(Secret::new(long)));
        assert_eq!(next_run.get("odd").unwrap(), Some(Secret::new(odd)));
        assert_eq!(next_run.get("empty").unwrap(), Some(Secret::new("")));
    }

    /// Reading or forgetting creates nothing; the first write creates the
    /// directory and the files.
    #[test]
    fn a_missing_directory_gets_what_it_needs_at_the_first_write() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("data").join("kerno");
        let store = FileSecretStore::new(&dir);
        assert_eq!(store.lookup("a").unwrap(), Lookup::Unknown);
        store.forget("a").unwrap();
        store.delete("a").unwrap();
        assert!(!dir.exists());
        store.set("a", &Secret::new(SECRET_A)).unwrap();
        for path in file::paths(&dir) {
            assert!(path.exists(), "{}", path.display());
        }
        assert_eq!(store.get("a").unwrap(), Some(Secret::new(SECRET_A)));
    }

    /// The store of the app with Kerno's file in use, a fake keychain
    /// beneath.
    fn chosen(choice: StoreChoice, dir: &Path, keychain: &Arc<Counting>) -> ChosenStore {
        ChosenStore::new(choice, FileSecretStore::new(dir), keychain.clone())
    }

    /// A secret saved in the keychain before K33 is read there once, then
    /// served by the file, in this run and the next ones; a key the keychain
    /// does not have is not asked for again either. Mutation seen: the
    /// answer not written back to the file (the reads then count 2).
    #[test]
    fn a_keychain_secret_moves_to_the_file_once() {
        let dir = tempfile::tempdir().unwrap();
        let keychain = Arc::new(Counting::default());
        keychain.store.set("pw", &Secret::new(SECRET_A)).unwrap();
        for _run in 0..3 {
            let store = chosen(StoreChoice::File, dir.path(), &keychain);
            assert_eq!(store.get("pw").unwrap(), Some(Secret::new(SECRET_A)));
            assert_eq!(store.get("absent").unwrap(), None);
        }
        assert_eq!(keychain.reads("pw"), 1);
        assert_eq!(keychain.reads("absent"), 1);
        assert_eq!(keychain.changes(), 0);
        assert_eq!(
            FileSecretStore::new(dir.path()).lookup("pw").unwrap(),
            Lookup::Value(Secret::new(SECRET_A))
        );
    }

    /// With the file in use, writes and deletes never reach the keychain,
    /// and its older copy of a removed secret never comes back. Mutation
    /// seen: `delete` forgetting the key instead of marking it.
    #[test]
    fn with_the_file_in_use_the_keychain_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let keychain = Arc::new(Counting::default());
        keychain.store.set("pw", &Secret::new("old")).unwrap();
        let store = chosen(StoreChoice::File, dir.path(), &keychain);
        store.set("pw", &Secret::new("new")).unwrap();
        assert_eq!(store.get("pw").unwrap(), Some(Secret::new("new")));
        store.delete("pw").unwrap();
        assert_eq!(store.get("pw").unwrap(), None);
        // A connection saved with "ask each time" forgets all three secrets.
        delete_secrets(&store, "c").unwrap();
        assert_eq!(load_secrets(&store, "c").unwrap(), Secrets::default());
        assert_eq!(keychain.reads("pw"), 0);
        assert_eq!(keychain.reads("connection/c/password"), 0);
        assert_eq!(keychain.changes(), 0);
        // The keychain's copy is still there, unused.
        assert_eq!(keychain.store.get("pw").unwrap(), Some(Secret::new("old")));
    }

    /// A keychain that refuses the read (Deny) counts as having nothing,
    /// once: the app asks for the password and the file keeps it.
    #[test]
    fn a_refused_keychain_read_counts_as_nothing_once() {
        struct Refusing(Mutex<usize>);
        impl SecretStore for Refusing {
            fn get(&self, _: &str) -> Result<Option<Secret>> {
                *self.0.lock().unwrap() += 1;
                Err(Error::Secrets("User canceled the operation".into()))
            }
            fn set(&self, _: &str, _: &Secret) -> Result<()> {
                unreachable!("the file is in use")
            }
            fn delete(&self, _: &str) -> Result<()> {
                unreachable!("the file is in use")
            }
            fn is_persistent(&self) -> bool {
                true
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let refusing = Arc::new(Refusing(Mutex::new(0)));
        let store = ChosenStore::new(
            StoreChoice::File,
            FileSecretStore::new(dir.path()),
            refusing.clone(),
        );
        assert_eq!(store.get("pw").unwrap(), None);
        assert_eq!(store.get("pw").unwrap(), None);
        assert_eq!(*refusing.0.lock().unwrap(), 1);
        store.set("pw", &Secret::new("typed")).unwrap();
        assert_eq!(store.get("pw").unwrap(), Some(Secret::new("typed")));
    }

    /// With the keychain in use the file drains into it: a newer secret
    /// moves, a removal deletes the keychain's copy, a write clears the
    /// file's entry; back on the file, the keychain's latest value is read
    /// once. Mutation seen: the `Lookup::Gone` arm reading the keychain.
    #[test]
    fn with_the_keychain_in_use_the_file_drains_into_it() {
        let dir = tempfile::tempdir().unwrap();
        let keychain = Arc::new(Counting::default());
        keychain.store.set("moved", &Secret::new("old")).unwrap();
        keychain.store.set("removed", &Secret::new("old")).unwrap();
        let file = chosen(StoreChoice::File, dir.path(), &keychain);
        file.set("moved", &Secret::new("new")).unwrap();
        file.delete("removed").unwrap();

        let keychain_store = chosen(StoreChoice::Keychain, dir.path(), &keychain);
        assert_eq!(
            keychain_store.get("moved").unwrap(),
            Some(Secret::new("new"))
        );
        assert_eq!(
            keychain.store.get("moved").unwrap(),
            Some(Secret::new("new"))
        );
        assert_eq!(keychain_store.get("removed").unwrap(), None);
        assert_eq!(keychain.store.get("removed").unwrap(), None);
        let in_file = FileSecretStore::new(dir.path());
        assert_eq!(in_file.lookup("moved").unwrap(), Lookup::Unknown);
        assert_eq!(in_file.lookup("removed").unwrap(), Lookup::Unknown);
        // Now read from the keychain itself.
        assert_eq!(
            keychain_store.get("moved").unwrap(),
            Some(Secret::new("new"))
        );
        assert_eq!(keychain.reads("moved"), 1);

        keychain_store.set("moved", &Secret::new("latest")).unwrap();
        assert_eq!(in_file.lookup("moved").unwrap(), Lookup::Unknown);
        let back = chosen(StoreChoice::File, dir.path(), &keychain);
        assert_eq!(back.get("moved").unwrap(), Some(Secret::new("latest")));
        assert_eq!(keychain.reads("moved"), 2);
    }

    /// An import's question (`has_secrets`) asks the store in use only:
    /// nothing is read from the keychain, nothing moved. Mutation seen:
    /// `has_secrets` calling `get`.
    #[test]
    fn a_question_about_saved_secrets_never_reads_the_other_store() {
        let dir = tempfile::tempdir().unwrap();
        let keychain = Arc::new(Counting::default());
        save_password(&keychain.store, "c", &Secret::new("pw")).unwrap();
        let store = chosen(StoreChoice::File, dir.path(), &keychain);
        assert!(!has_secrets(&store, "c").unwrap());
        assert_eq!(keychain.reads("connection/c/password"), 0);
        assert!(!dir.path().join(SECRETS_FILE).exists());
        save_password(&store, "d", &Secret::new("pw")).unwrap();
        assert!(has_secrets(&store, "d").unwrap());
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
