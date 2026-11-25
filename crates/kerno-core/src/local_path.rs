//! The files a saved connection or a saved task names, on Windows (K69, D-138).
//!
//! A saved connection may come from an imported file (Kerno's connections
//! file, DBeaver's), a task too, or from the store edited by hand. On Windows
//! a path that names another machine (`\\host\share\…`, in any spelling) is
//! opened over SMB with the user's Windows credentials: merely reading,
//! stating or handing it to another program logs in there. So every place
//! that opens such a file, or hands it to another program, takes it through
//! [`resolve`] first, which expands `~\` with the home and then checks it, so
//! that what is checked is what is opened: the drivers' connect (TLS files,
//! the SQLite database: [`resolve_params`]), the client tools (TLS files and
//! a task's files), both SSH clients (the key file), the password command (its
//! program). On macOS and Linux nothing is expanded or refused: the rule
//! itself ([`is_local_file`]) is strings in, a verdict out, tested on every
//! system.

use std::path::{Path, PathBuf};

use crate::config::{ConnectParams, ConnectionConfig, Engine, SshAuth, TlsConfig};
use crate::error::{Error, Result};

/// What a file of a connection or a task must look like on Windows, for the
/// messages that refuse another.
pub const LOCAL_FILE: &str = r"a full path on this computer, such as C:\Users\you\file or ~\file";

/// Windows' device names: a path component named so (any case, with or
/// without an extension) opens the device, not a file.
const DEVICES: [&str; 6] = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];

/// Whether `path` is a file of this computer the way Windows reads it.
///
/// An allow-list, `/` read as `\` as Windows does: a drive's absolute path
/// (`X:\…`, a mapped network drive included: Windows logged in to it when the
/// user mapped it; also in the exact spelling `\\?\X:\…` that Windows gives a
/// path made canonical, which can only name a drive) or a path under the home
/// (`~\…`), with no further `:` (no
/// alternate data stream, no second drive), no NUL and no component that is a
/// device name (`CON`, `NUL`, `COM1`, `LPT1.txt`…). Everything else is
/// refused: two leading separators in any spelling (`\\host`, `//host`,
/// `\/host`, `/\host`), `\\?\UNC\…` and every other `\\?\` (a volume, a
/// device, `//?/` with slashes), `\\.\`, `\??\`, a rooted path without a
/// drive (`\Users\…`: when Kerno's working folder is a share, it names that
/// share), a drive-relative `X:file`, a path relative to the folder Kerno
/// happens to run in. Named pipes are [`crate::ssh::is_local_pipe`]'s. A
/// link on the local disk that leads to a share is the machine owner's own
/// doing, not a saved connection's, and is followed. On Windows the drive is
/// confirmed by `Path::components` too.
pub fn is_local_file(path: &str) -> bool {
    // A canonical path's prefix, backslashes only: what follows must be a
    // drive's absolute path, and Windows then reads no `/` as a separator.
    let (canonical, path) = match path.strip_prefix(r"\\?\") {
        Some(rest) => (true, rest),
        None => (false, path),
    };
    if canonical && path.contains('/') {
        return false;
    }
    let path = path.replace('/', "\\");
    let rest = match path.as_bytes() {
        [drive, b':', b'\\', ..] if drive.is_ascii_alphabetic() => &path[3..],
        [b'~', b'\\', ..] if !canonical => &path[2..],
        _ => return false,
    };
    if rest.contains([':', '\0']) || rest.split('\\').any(is_device) {
        return false;
    }
    #[cfg(windows)]
    if !path.starts_with('~') {
        use std::path::{Component, Prefix};
        let disk = matches!(
            Path::new(&path).components().next(),
            Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_))
        );
        if !disk {
            return false;
        }
    }
    true
}

/// Whether a path component names a device: its name before the first `.`,
/// trailing spaces and dots left out, is one of [`DEVICES`] or `COM`/`LPT`
/// followed by a digit (`1`–`9`, or the superscripts `¹²³`, which Windows
/// takes too).
fn is_device(component: &str) -> bool {
    let stem = component.split('.').next().unwrap_or(component);
    let stem = stem.trim_end_matches([' ', '.']).to_uppercase();
    if DEVICES.contains(&stem.as_str()) {
        return true;
    }
    let numbered = stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"));
    numbered.is_some_and(|digit| {
        let mut chars = digit.chars();
        matches!(
            (chars.next(), chars.next()),
            (Some('1'..='9' | '¹' | '²' | '³'), None)
        )
    })
}

/// On Windows, why the `what` at `path` is not opened, in plain words; `None`
/// when it is a file of this computer, and always on Unix.
pub fn refusal(path: &Path, what: &str) -> Option<String> {
    (cfg!(windows) && !is_local_file(&path.to_string_lossy())).then(|| {
        format!(
            "the {what} “{}” is not a file of this computer, and Kerno opens no file of \
             another computer (Windows would log in there with your account): use {LOCAL_FILE}.",
            path.display()
        )
    })
}

/// [`refusal`] as an error.
pub fn check(path: &Path, what: &str) -> Result<()> {
    refusal(path, what).map_or(Ok(()), |refusal| Err(Error::Config(refusal)))
}

/// `path` with `~\` (or `~/`) put under the home, on Windows only; the
/// check is [`check`]'s, where the file is used.
pub fn expand(path: &Path) -> PathBuf {
    if !cfg!(windows) {
        return path.to_path_buf();
    }
    crate::home::expand(&path.to_string_lossy(), crate::home::dir().as_deref())
}

/// The path to open for `path`, a file of a connection or a task: on Windows
/// `~\` (or `~/`) put under the home ([`crate::home::expand`]), then checked
/// ([`check`]); refused when `~` cannot be expanded. On Unix `path` as it is.
pub fn resolve(path: &Path, what: &str) -> Result<PathBuf> {
    if !cfg!(windows) {
        return Ok(path.to_path_buf());
    }
    resolve_with(path, what, crate::home::dir().as_deref())
}

/// [`resolve`] with the home given: the rule itself, on every system.
pub(crate) fn resolve_with(path: &Path, what: &str, home: Option<&Path>) -> Result<PathBuf> {
    let expanded = crate::home::expand(&path.to_string_lossy(), home);
    if expanded.to_string_lossy().starts_with('~') || !is_local_file(&expanded.to_string_lossy()) {
        return Err(Error::Config(format!(
            "the {what} “{}” is not a file of this computer, and Kerno opens no file of \
             another computer (Windows would log in there with your account): use {LOCAL_FILE}.",
            path.display()
        )));
    }
    Ok(expanded)
}

/// The TLS files of a connection, resolved ([`resolve`]).
pub fn resolve_tls(tls: &TlsConfig) -> Result<TlsConfig> {
    let file =
        |file: &Option<PathBuf>, what| file.as_deref().map(|file| resolve(file, what)).transpose();
    Ok(TlsConfig {
        ca_file: file(&tls.ca_file, "CA certificate")?,
        cert_file: file(&tls.cert_file, "client certificate")?,
        key_file: file(&tls.key_file, "client key")?,
        ..tls.clone()
    })
}

/// The settings a driver's connect opens files from, resolved: the TLS files,
/// and SQLite's database, which may well be on a share: a mapped drive letter
/// is taken, the network name is not, and the message says how.
pub fn resolve_params(params: &ConnectParams) -> Result<ConnectParams> {
    let mut resolved = params.clone();
    if params.engine == Engine::Sqlite
        && let Some(file) = &params.file
        && !file.as_os_str().is_empty()
    {
        resolved.file = Some(resolve(file, "database file").map_err(|_| {
            Error::Config(format!(
                "the database file “{}” is not a file of this computer: Kerno opens no file of \
                 another computer by its network name (Windows would log in there with your \
                 account). Map the share to a drive letter (File Explorer, Map network drive) \
                 and open the file there, or use {LOCAL_FILE}.",
                file.display()
            ))
        })?);
    }
    resolved.tls = resolve_tls(&params.tls)?;
    Ok(resolved)
}

/// What an import says under a connection whose files Kerno will not open
/// ([`refusal`]): one line per file, empty on Unix.
pub fn notes(config: &ConnectionConfig) -> Vec<String> {
    let key = config.ssh.as_ref().and_then(|ssh| match &ssh.auth {
        SshAuth::KeyFile { path } => Some(path),
        _ => None,
    });
    let files = [
        (key, "SSH key"),
        (config.tls.ca_file.as_ref(), "CA certificate"),
        (config.tls.cert_file.as_ref(), "client certificate"),
        (config.tls.key_file.as_ref(), "client key"),
        (config.file.as_ref(), "database file"),
    ];
    files
        .into_iter()
        .filter_map(|(path, what)| note(path?, what))
        .collect()
}

/// The import's line for one file Kerno will not open, `None` when it will.
pub fn note(path: &Path, what: &str) -> Option<String> {
    refusal(path, what)?;
    Some(format!(
        "the {what} {} is not a file of this computer: it will not be opened; choose \
         {LOCAL_FILE}",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A connection's file is a file of this computer, Windows' way: only a
    /// drive's absolute path or one under `~\`, whatever the separators and
    /// the case, and no device. Mutations seen: the `/` to `\` turn dropped
    /// (the `//host`, `\/host` and `C:/` cases go wrong); the device check
    /// dropped (`NUL`, `com1.txt` pass).
    #[test]
    fn a_connection_file_is_a_file_of_this_computer() {
        for local in [
            r"C:\Users\ada\.ssh\id_ed25519",
            "c:/Users/ada/.ssh/id",
            r"D:\keys/id",
            r"z:\k",
            r"Z:\mapped share\shop.db",
            r"~\.ssh\id_ed25519",
            "~/.ssh/id_ed25519",
            r"C:\\double\\separators\inside",
            r"\\?\C:\Users\ada\AppData\Local\Temp\shop.sql",
            r"\\?\d:\keys\id",
            r"C:\console\conx.d\com10\lpt0\nul-file",
        ] {
            assert!(is_local_file(local), "{local}");
        }
        for other in [
            r"\\host\share\id",
            "//host/share/id",
            r"\/host\share\id",
            r"/\host\share\id",
            r"\\HOST\SHARE\ID",
            r"\\?\UNC\host\share\id",
            r"//?/unc/host/share/id",
            r"//?/C:/id",
            r"\\?\\\?\C:\id",
            r"\\?\\\host\share\id",
            r"\\?\Volume{4c1b02c1-d990-11dc-99ae-806e6f6e6963}\id",
            r"\\?\GLOBALROOT\Device\Mup\host\share\id",
            r"\\?\C:id",
            r"\\?\~\id",
            r"\\?\C:/id",
            r"\\?\C:\id:stream",
            r"\\.\C:\id",
            r"//./c:/id",
            r"\\.\pipe\openssh-ssh-agent",
            r"\??\UNC\host\share\id",
            r"\??\C:\id",
            r"/??/c:/id",
            r"\Users\ada\id",
            "/Users/ada/.ssh/id",
            "C:id",
            "c:relative/id",
            r"C:\id:stream",
            r"c:/id:$DATA",
            r"C:\a\D:\b",
            r"~\id:stream",
            "~",
            "~id",
            "~ada/id",
            "id_ed25519",
            r".ssh\id",
            r"..\id",
            "",
            "C:\\id\0",
            r"C:\NUL",
            r"c:\dir\con",
            r"C:\keys\com1.txt",
            r"C:\keys\LPT9",
            r"C:\keys\nul.tar.gz",
            r"C:\keys\aux \x",
            r"C:\keys\CON..",
            r"C:\keys\conin$",
            r"C:\keys\COM¹",
            r"~\nul",
        ] {
            assert!(!is_local_file(other), "{other}");
        }
    }

    /// `~\` is expanded with the home before it is checked: what is checked
    /// is what is opened; no home, no expansion, refused. Mutation seen: the
    /// expansion dropped (`~\k` comes back as it is).
    #[test]
    fn a_home_path_is_expanded_then_checked() {
        let home = Path::new(r"C:\Users\Jean Dupont");
        // `~/`: a separator on every system (`~\` is one on Windows only).
        assert_eq!(
            resolve_with(Path::new("~/.ssh/id"), "key", Some(home)).unwrap(),
            home.join(".ssh/id")
        );
        assert_eq!(
            resolve_with(Path::new(r"D:\k"), "key", Some(home)).unwrap(),
            PathBuf::from(r"D:\k")
        );
        assert!(resolve_with(Path::new("~/.ssh/id"), "key", None).is_err());
        let share = Path::new(r"\\host\share");
        assert!(resolve_with(Path::new("~/x"), "key", Some(share)).is_err());
        assert!(resolve_with(Path::new(r"\\host\x"), "key", Some(home)).is_err());
    }

    /// A task's `~\` folder is the profile's, a full path once expanded (what
    /// the backup wizard judges, S4); on Unix it stays as it is.
    #[test]
    fn a_home_folder_is_a_full_path_once_expanded() {
        let folder = expand(Path::new(r"~\dumps"));
        assert_eq!(folder.is_absolute(), cfg!(windows), "{}", folder.display());
    }

    /// The refusals are Windows' only, and say what a valid path looks like.
    #[test]
    fn only_windows_refuses_and_says_what_to_use() {
        let share = Path::new(r"\\host\share\ca.pem");
        let tls = TlsConfig {
            ca_file: Some(share.into()),
            ..TlsConfig::default()
        };
        let mut params = crate::testing::params(Engine::Postgres);
        params.engine = Engine::Sqlite;
        params.file = Some(r"\\host\share\shop.db".into());
        let mut config = ConnectionConfig::new(Engine::Postgres);
        config.tls = tls.clone();
        assert_eq!(notes(&config).len(), usize::from(cfg!(windows)));
        if cfg!(windows) {
            let message = resolve_tls(&tls).unwrap_err().to_string();
            assert!(message.contains("CA certificate"), "{message}");
            assert!(message.contains(LOCAL_FILE), "{message}");
            let message = resolve_params(&params).unwrap_err().to_string();
            assert!(
                message.contains("Map the share to a drive letter"),
                "{message}"
            );
            params.file = Some(r"Z:\shop.db".into());
            assert!(resolve_params(&params).is_ok());
        } else {
            assert_eq!(resolve_tls(&tls).unwrap(), tls);
            assert_eq!(resolve_params(&params).unwrap().file, params.file);
        }
    }
}
