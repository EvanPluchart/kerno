//! Kerno's own password file (K33, D-106): every secret in one file of the
//! data directory, encrypted with AES-256-GCM under a random key kept in a
//! second file beside it.
//!
//! What this protects, exactly: the passwords are out of plain sight. A copy
//! of the secrets file alone (a backup, a synced folder, a file search, a
//! shared screen) shows nothing, and another user of the machine cannot read
//! either file (mode 0600). It does not protect them from a program running
//! as the same user, which can read the key file as well as the secrets
//! file: the same level as DBeaver's saved passwords, `~/.pgpass` or an SSH
//! key without a passphrase. Nothing is ever written in plain text.
//!
//! Every change re-reads the file under an exclusive lock (`secrets.lock`,
//! an empty file), changes the one key, and replaces the file atomically (a
//! temporary file in the same directory, `fsync`ed, renamed over it, the
//! directory then `fsync`ed where the file system takes it), so two Kerno
//! processes on one data directory never erase each other's secrets and a
//! crash leaves the previous file whole.
//! Nothing is kept in memory between calls.
//!
//! A file that cannot be decrypted (wrong key, damaged, cut short, its key
//! file missing) is never overwritten: it is renamed aside to a `.damaged`
//! name, with its key file, and the call fails once with plain words; the
//! next call starts a new file.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::PathBuf;

use aes_gcm::aead::{Aead as _, KeyInit as _, Nonce, Payload};
use aes_gcm::{Aes256Gcm, Key};
use serde::{Deserialize, Serialize};

use super::SecretStore;
use crate::config::Secret;
use crate::error::{Error, Result};

/// The encrypted secrets.
pub const SECRETS_FILE: &str = "secrets.kerno";
/// The key that encrypts them.
pub const KEY_FILE: &str = "secrets.key";
/// Held while a change is made; empty.
const LOCK_FILE: &str = "secrets.lock";
/// Written next to the secrets file, then renamed over it.
const TEMP_FILE: &str = "secrets.kerno.tmp";

/// How every message about the file starts (`error::explain` reads it).
pub const FILE_PROBLEM: &str = "Kerno's password file";

/// The file's first bytes, and the associated data of the cipher.
const MAGIC: &[u8; 5] = b"KRNS\x01";
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;
const KEY_LEN: usize = 32;

/// What the file says of one key.
#[derive(Debug, PartialEq, Eq)]
pub enum Lookup {
    /// A secret.
    Value(Secret),
    /// Removed here: what another store holds under it is out of date.
    Gone,
    /// Never seen here.
    Unknown,
}

/// The decrypted content: a secret, or `None` for a key removed here.
#[derive(Default, Serialize, Deserialize)]
struct Contents {
    secrets: BTreeMap<String, Option<String>>,
}

/// Kerno's own password file, in `dir`.
#[derive(Debug, Clone)]
pub struct FileSecretStore {
    dir: PathBuf,
}

impl FileSecretStore {
    /// The store in `dir` (the app's data directory). Nothing is read or
    /// created until a secret is.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// What the file holds under `key`. Creates nothing.
    pub fn lookup(&self, key: &str) -> Result<Lookup> {
        if !self.path(SECRETS_FILE).exists() {
            return Ok(Lookup::Unknown);
        }
        let _lock = self.lock()?;
        let contents = self.read()?;
        Ok(match contents.secrets.get(key) {
            Some(Some(secret)) => Lookup::Value(Secret::new(secret.clone())),
            Some(None) => Lookup::Gone,
            None => Lookup::Unknown,
        })
    }

    /// Stores `secret` under `key`.
    pub fn put(&self, key: &str, secret: &Secret) -> Result<()> {
        self.change(key, Some(Some(secret.expose().to_string())))
    }

    /// Marks `key` as removed here.
    pub fn mark_gone(&self, key: &str) -> Result<()> {
        self.change(key, Some(None))
    }

    /// Forgets everything about `key`. Creates nothing.
    pub fn forget(&self, key: &str) -> Result<()> {
        if !self.path(SECRETS_FILE).exists() {
            return Ok(());
        }
        self.change(key, None)
    }

    /// Sets (`Some`) or removes (`None`) the entry of `key`, under the lock,
    /// on what the file holds now.
    fn change(&self, key: &str, entry: Option<Option<String>>) -> Result<()> {
        let _lock = self.lock()?;
        let mut contents = self.read()?;
        let unchanged = match &entry {
            Some(value) => contents.secrets.get(key) == Some(value),
            None => !contents.secrets.contains_key(key),
        };
        if unchanged {
            return Ok(());
        }
        match entry {
            Some(value) => contents.secrets.insert(key.to_string(), value),
            None => contents.secrets.remove(key),
        };
        let key = self.key_for_writing()?;
        self.write(&contents, &key)
    }

    /// The exclusive lock of every read and change, the directory made if
    /// it is missing (its mode is left as it is).
    fn lock(&self) -> Result<File> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|error| io_problem("could not be opened", &error))?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(self.path(LOCK_FILE))
            .map_err(|error| io_problem("could not be opened", &error))?;
        file.lock()
            .map_err(|error| io_problem("could not be locked", &error))?;
        Ok(file)
    }

    /// The decrypted content, empty when there is no file. A file that does
    /// not decrypt is set aside and reported.
    fn read(&self) -> Result<Contents> {
        let bytes = match std::fs::read(self.path(SECRETS_FILE)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Contents::default()),
            Err(error) => return Err(io_problem("could not be read", &error)),
        };
        let key = match std::fs::read(self.path(KEY_FILE)) {
            Ok(key) if key.len() == KEY_LEN => key,
            Ok(_) => {
                return Err(self.set_aside(&format!("its key file {KEY_FILE} is not a Kerno key")));
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Err(self.set_aside(&format!("its key file {KEY_FILE} is missing")));
            }
            Err(error) => return Err(io_problem("could not be read", &error)),
        };
        if bytes.len() < MAGIC.len() + NONCE_LEN + TAG_LEN || !bytes.starts_with(MAGIC) {
            return Err(self.set_aside("it is cut short or is not a Kerno password file"));
        }
        let (nonce, sealed) = bytes[MAGIC.len()..].split_at(NONCE_LEN);
        let nonce = Nonce::<Aes256Gcm>::try_from(nonce)
            .map_err(|_| self.set_aside("it is cut short or is not a Kerno password file"))?;
        let plain = cipher(&key)?
            .decrypt(
                &nonce,
                Payload {
                    msg: sealed,
                    aad: MAGIC,
                },
            )
            .map_err(|_| {
                self.set_aside(
                    "it does not decrypt with its key: one of the two files was changed or damaged",
                )
            })?;
        // serde's message could quote the decrypted text: not kept.
        serde_json::from_slice(&plain).map_err(|_| self.set_aside("its content is damaged"))
    }

    /// The key, generated (and written, mode 0600) when there is none. A
    /// key is generated only while no secrets file exists: `read` has set
    /// aside one that had no key.
    fn key_for_writing(&self) -> Result<Vec<u8>> {
        match std::fs::read(self.path(KEY_FILE)) {
            Ok(key) if key.len() == KEY_LEN => return Ok(key),
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(io_problem("could not be read", &error)),
        }
        let mut key = vec![0u8; KEY_LEN];
        getrandom::fill(&mut key).map_err(|_| {
            Error::Secrets(format!(
                "{FILE_PROBLEM} could not be created: the system gave no random bytes for its key."
            ))
        })?;
        self.replace(KEY_FILE, &key)?;
        Ok(key)
    }

    /// Encrypts `contents` with a fresh nonce and replaces the file.
    fn write(&self, contents: &Contents, key: &[u8]) -> Result<()> {
        let plain = serde_json::to_vec(contents)
            .map_err(|_| Error::Secrets(format!("{FILE_PROBLEM} could not be written.")))?;
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).map_err(|_| {
            Error::Secrets(format!(
                "{FILE_PROBLEM} could not be written: the system gave no random bytes. The \
                 passwords saved before are kept."
            ))
        })?;
        let sealed = cipher(key)?
            .encrypt(
                &Nonce::<Aes256Gcm>::from(nonce),
                Payload {
                    msg: &plain,
                    aad: MAGIC,
                },
            )
            .map_err(|_| Error::Secrets(format!("{FILE_PROBLEM} could not be written.")))?;
        let mut bytes = Vec::with_capacity(MAGIC.len() + NONCE_LEN + sealed.len());
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&nonce);
        bytes.extend_from_slice(&sealed);
        self.replace(SECRETS_FILE, &bytes)
    }

    /// Writes `bytes` to `name` atomically: a temporary file (mode 0600 from
    /// its creation) written and `fsync`ed, renamed over `name`, then the
    /// directory `fsync`ed (best effort). On failure `name` keeps its
    /// previous content.
    fn replace(&self, name: &str, bytes: &[u8]) -> Result<()> {
        let temp = self.path(TEMP_FILE);
        let written = (|| {
            // One left by a crash: under the lock, nobody else writes it.
            if let Err(error) = std::fs::remove_file(&temp)
                && error.kind() != ErrorKind::NotFound
            {
                return Err(error);
            }
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            std::fs::rename(&temp, self.path(name))
        })();
        // The rename is the change; the directory's own sync is best effort
        // (not every file system takes one on a directory).
        if written.is_ok()
            && let Ok(dir) = File::open(&self.dir)
        {
            let _ = dir.sync_all();
        }
        written.map_err(|error| {
            let _ = std::fs::remove_file(&temp);
            Error::Secrets(format!(
                "{FILE_PROBLEM} could not be written: {}. The passwords saved before are kept.",
                plain_io(&error)
            ))
        })
    }

    /// Renames the secrets file (and its key file, when there is one) to
    /// `.damaged` names that no earlier copy has, and says so.
    fn set_aside(&self, reason: &str) -> Error {
        let names = |n: usize| match n {
            1 => (
                format!("{SECRETS_FILE}.damaged"),
                format!("{KEY_FILE}.damaged"),
            ),
            n => (
                format!("{SECRETS_FILE}-{n}.damaged"),
                format!("{KEY_FILE}-{n}.damaged"),
            ),
        };
        let (secrets, key) = (1..)
            .map(names)
            .find(|(secrets, key)| !self.path(secrets).exists() && !self.path(key).exists())
            .unwrap_or_else(|| names(1));
        let moved = std::fs::rename(self.path(SECRETS_FILE), self.path(&secrets));
        if self.path(KEY_FILE).exists() {
            let _ = std::fs::rename(self.path(KEY_FILE), self.path(&key));
        }
        tracing::warn!(%reason, aside = %secrets, "the password file could not be read");
        match moved {
            Ok(()) => Error::Secrets(format!(
                "{FILE_PROBLEM} could not be read: {reason}. It was set aside as {secrets} in \
                 Kerno's data folder: enter the password again, and Kerno saves it in a new file."
            )),
            Err(error) => Error::Secrets(format!(
                "{FILE_PROBLEM} could not be read: {reason}. It could not be set aside either ({}): \
                 move {SECRETS_FILE} out of Kerno's data folder, then enter the password again.",
                plain_io(&error)
            )),
        }
    }
}

fn cipher(key: &[u8]) -> Result<Aes256Gcm> {
    let key = Key::<Aes256Gcm>::try_from(key)
        .map_err(|_| Error::Secrets(format!("{FILE_PROBLEM} has a key of the wrong size.")))?;
    Ok(Aes256Gcm::new(&key))
}

/// An I/O failure in words: its kind, never a path's content.
fn plain_io(error: &std::io::Error) -> String {
    match error.kind() {
        ErrorKind::PermissionDenied => "Kerno may not write in its data folder".to_string(),
        ErrorKind::StorageFull => "the disk is full".to_string(),
        _ => error.to_string(),
    }
}

fn io_problem(what: &str, error: &std::io::Error) -> Error {
    Error::Secrets(format!("{FILE_PROBLEM} {what}: {}.", plain_io(error)))
}

impl SecretStore for FileSecretStore {
    fn get(&self, key: &str) -> Result<Option<Secret>> {
        Ok(match self.lookup(key)? {
            Lookup::Value(secret) => Some(secret),
            Lookup::Gone | Lookup::Unknown => None,
        })
    }

    fn set(&self, key: &str, secret: &Secret) -> Result<()> {
        self.put(key, secret)
    }

    /// Alone (`KERNO_SECRET_STORE=file`) nothing else could hold the key:
    /// it is forgotten, not marked.
    fn delete(&self, key: &str) -> Result<()> {
        self.forget(key)
    }

    fn is_persistent(&self) -> bool {
        true
    }

    fn name(&self) -> &'static str {
        super::StoreChoice::File.label()
    }
}

/// The paths a test looks at.
#[cfg(test)]
pub(super) fn paths(dir: &std::path::Path) -> [PathBuf; 3] {
    [SECRETS_FILE, KEY_FILE, LOCK_FILE].map(|name| dir.join(name))
}
