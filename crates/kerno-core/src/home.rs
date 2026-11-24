//! The user's home folder: one owner for every reader (K67, D-136). On Unix it is `HOME`, as it
//! always was; on Windows, where `HOME` is not set, the profile folder (`USERPROFILE`, else the
//! system's known folder), where Windows' OpenSSH keeps `.ssh\config` and `known_hosts` too.
//! Readers that tests drive take the home as a value ([`crate::password::PasswordEnv`],
//! [`crate::openssh::host_aliases`]); only the app reads this one.

use std::path::{Path, PathBuf};

/// The user's home folder, `None` when the system names none.
pub fn dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE")
            .filter(|profile| !profile.is_empty())
            .map(PathBuf::from)
            .or_else(|| directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()))
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

/// The user's own `known_hosts` (OpenSSH's, on Windows too): read by the built-in SSH client,
/// never written.
pub fn known_hosts() -> Option<PathBuf> {
    dir().map(|home| home.join(".ssh").join("known_hosts"))
}

/// `path` as the user typed it, with a leading `~` and a separator (`~/`; `~\` too on Windows)
/// put under `home`. Anything else, and every path when `home` is `None`, is kept as it is.
pub fn expand(path: &str, home: Option<&Path>) -> PathBuf {
    let rest = path
        .strip_prefix('~')
        .and_then(|rest| rest.strip_prefix(std::path::is_separator));
    match (rest, home) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(path),
    }
}

/// `path` as the user writes it: `~` and the rest when it is under `home`.
pub fn display(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) => format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display()),
        None => path.display().to_string(),
    }
}

/// The password file libpq reads: `PGPASSFILE` when set (`pgpassfile`), else on Windows
/// `%APPDATA%\postgresql\pgpass.conf` (libpq's documentation; `APPDATA` is the roaming
/// application data folder). `None` on Unix without `PGPASSFILE`: the file is then `~/.pgpass`,
/// which [`crate::password::resolve`] finds from the home.
pub fn pgpassfile(pgpassfile: Option<PathBuf>) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        pgpassfile.or_else(|| {
            directories::BaseDirs::new()
                .map(|dirs| dirs.config_dir().join("postgresql").join("pgpass.conf"))
        })
    }
    #[cfg(not(windows))]
    {
        pgpassfile
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tilde_and_a_separator_go_under_the_home() {
        let home = Path::new("/home/ada");
        assert_eq!(
            expand("~/.ssh/id_ed25519", Some(home)),
            home.join(".ssh/id_ed25519")
        );
        // Windows reads both slashes as separators; Unix only `/`.
        let backslash = expand(r"~\.ssh\id", Some(home));
        if cfg!(windows) {
            assert_eq!(backslash, home.join(r".ssh\id"));
        } else {
            assert_eq!(backslash, PathBuf::from(r"~\.ssh\id"));
        }
        for kept in ["~", "~ada/x", "/etc/hosts", "relative/~/x"] {
            assert_eq!(expand(kept, Some(home)), PathBuf::from(kept), "{kept}");
        }
        assert_eq!(expand("~/x", None), PathBuf::from("~/x"));
    }

    #[test]
    fn a_path_under_the_home_is_shown_from_the_tilde() {
        let home = Path::new("/home/ada");
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(
            display(&home.join(".pgpass"), Some(home)),
            format!("~{sep}.pgpass")
        );
        assert_eq!(display(Path::new("/etc/x"), Some(home)), "/etc/x");
        assert_eq!(display(Path::new("/etc/x"), None), "/etc/x");
    }

    #[test]
    fn pgpassfile_wins_and_unix_has_no_other_default() {
        let given = PathBuf::from("/srv/pgpass");
        assert_eq!(pgpassfile(Some(given.clone())), Some(given));
        if cfg!(unix) {
            assert_eq!(pgpassfile(None), None);
        }
    }
}
