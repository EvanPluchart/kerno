//! The files a saved connection names, on Windows (K69, D-138).
//!
//! A saved connection may come from an imported file (Kerno's connections
//! file, DBeaver's), from a task, or from the store edited by hand. On
//! Windows a path that names another machine (`\\host\share\…`, in any
//! spelling) is opened over SMB with the user's Windows credentials: merely
//! reading, stating or handing it to another program logs in there. So every
//! place that opens a connection's file, or hands it to another program,
//! asks [`check`] first: the drivers' connect (TLS files, the SQLite
//! database), the client tools (TLS files), both SSH clients (the key file),
//! the password command (its program). On macOS and Linux nothing is
//! refused: the rule itself ([`is_local_file`]) is strings in, a verdict
//! out, tested on every system.

use std::path::Path;

use crate::config::{ConnectParams, ConnectionConfig, Engine, SshAuth, TlsConfig};
use crate::error::{Error, Result};

/// What a connection's file must look like on Windows, for the messages
/// that refuse another.
pub const LOCAL_FILE: &str = r"a full path on this computer, such as C:\Users\you\file or ~\file";

/// Whether `path` is a file of this computer the way Windows reads it.
///
/// An allow-list, `/` read as `\` as Windows does: a drive's absolute path
/// (`X:\…`, a mapped network drive included: Windows logged in to it when the
/// user mapped it) or a path under the home (`~\…`), with no further `:` (no
/// alternate data stream, no second drive) and no NUL. Everything else is
/// refused: two leading separators in any spelling (`\\host`, `//host`,
/// `\/host`, `/\host`), `\\?\`, `\\.\`, `\??\`, a rooted path without a
/// drive (`\Users\…`), a drive-relative `X:file`, a path relative to the
/// folder Kerno happens to run in. Named pipes are
/// [`crate::ssh::is_local_pipe`]'s. On Windows the drive is confirmed by
/// `Path::components` too.
pub fn is_local_file(path: &str) -> bool {
    let path = path.replace('/', "\\");
    let rest = match path.as_bytes() {
        [drive, b':', b'\\', ..] if drive.is_ascii_alphabetic() => &path[3..],
        [b'~', b'\\', ..] => &path[2..],
        _ => return false,
    };
    if rest.contains([':', '\0']) {
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

/// On Windows, why the connection's `what` at `path` is not opened, in plain
/// words; `None` when it is a file of this computer, and always on Unix.
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

/// Checks the TLS files of a connection.
pub fn check_tls(tls: &TlsConfig) -> Result<()> {
    let files = [
        (&tls.ca_file, "CA certificate"),
        (&tls.cert_file, "client certificate"),
        (&tls.key_file, "client key"),
    ];
    for (file, what) in files {
        if let Some(file) = file {
            check(file, what)?;
        }
    }
    Ok(())
}

/// Checks every file a driver's connect opens: the TLS files, and SQLite's
/// database, which may well be on a share: a mapped drive letter is taken.
pub fn check_params(params: &ConnectParams) -> Result<()> {
    if params.engine == Engine::Sqlite
        && let Some(file) = &params.file
        && refusal(file, "database file").is_some()
    {
        return Err(Error::Config(format!(
            "the database file “{}” is not a file of this computer: Kerno opens no file of \
             another computer by its network name (Windows would log in there with your \
             account). Map the share to a drive letter (File Explorer, Map network drive) \
             and open the file there, or use {LOCAL_FILE}.",
            file.display()
        )));
    }
    check_tls(&params.tls)
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
        .filter_map(|(path, what)| {
            let path = path?;
            refusal(path, what)?;
            Some(format!(
                "the {what} {} is not a file of this computer: it will not be opened; choose \
                 {LOCAL_FILE}",
                path.display()
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A connection's file is a file of this computer, Windows' way: only a
    /// drive's absolute path or one under `~\`, whatever the separators and
    /// the case. Mutation seen: the `/` to `\` turn dropped (the `//host`,
    /// `\/host` and `C:/` cases go wrong).
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
            r"\\?\C:\id",
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
        ] {
            assert!(!is_local_file(other), "{other}");
        }
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
            let message = check_tls(&tls).unwrap_err().to_string();
            assert!(message.contains("CA certificate"), "{message}");
            assert!(message.contains(LOCAL_FILE), "{message}");
            let message = check_params(&params).unwrap_err().to_string();
            assert!(
                message.contains("Map the share to a drive letter"),
                "{message}"
            );
            params.file = Some(r"Z:\shop.db".into());
            assert!(check_params(&params).is_ok());
        } else {
            assert!(check_tls(&tls).is_ok());
            assert!(check_params(&params).is_ok());
        }
    }
}
