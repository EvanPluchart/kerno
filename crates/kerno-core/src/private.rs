//! Files and folders only this user may read (K64, D-131): the one place that says it with a
//! Unix mode, and what Windows does instead.
//!
//! Unix: the modes Kerno has always used, given as the file or folder is made (0600, 0700),
//! never set on one made first.
//!
//! Windows: nothing is set or read here. The workspace allows no new `unsafe` and no new
//! dependency, so Kerno writes and reads no access list. A file or folder made under the
//! user's profile (`%APPDATA%`, `%LOCALAPPDATA%`, `%TEMP%`) inherits the profile's, which
//! grants the user, SYSTEM and the administrators only: Kerno's data directory and temporary
//! folders are there. One made elsewhere (a `KERNO_DATA_DIR` outside the profile) gets what
//! that folder grants, which may be more.

use std::fs::{DirBuilder, Metadata, OpenOptions};
use std::io;
use std::path::Path;

/// Options that make a file only its owner can read or write: mode 0600 from its creation on
/// Unix; on Windows, what the folder passes on (see the module).
#[cfg(unix)]
pub fn file_options() -> OpenOptions {
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut options = OpenOptions::new();
    options.mode(0o600);
    options
}

/// Options that make a file only its owner can read or write: mode 0600 from its creation on
/// Unix; on Windows, what the folder passes on (see the module).
#[cfg(not(unix))]
pub fn file_options() -> OpenOptions {
    OpenOptions::new()
}

/// Makes the folder `path` (its parent must exist), one only its owner can enter: mode 0700
/// from its creation on Unix; on Windows, what the parent passes on. Fails when it exists.
#[cfg(unix)]
pub fn create_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    DirBuilder::new().mode(0o700).create(path)
}

/// Makes the folder `path` (its parent must exist), one only its owner can enter: mode 0700
/// from its creation on Unix; on Windows, what the parent passes on. Fails when it exists.
#[cfg(not(unix))]
pub fn create_dir(path: &Path) -> io::Result<()> {
    DirBuilder::new().create(path)
}

/// Sets the mode of an existing file or folder on Unix. Does nothing on Windows: its access
/// list is the one it inherited.
#[cfg(unix)]
pub fn restrict(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

/// Sets the mode of an existing file or folder on Unix. Does nothing on Windows: its access
/// list is the one it inherited.
#[cfg(not(unix))]
pub fn restrict(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

/// The permission bits of a file (`mode & 0o777`) on Unix. `None` on Windows: Kerno reads no
/// access list there, so it cannot tell who else may read or change the file.
#[cfg(unix)]
pub fn mode(meta: &Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt as _;
    Some(meta.permissions().mode() & 0o777)
}

/// The permission bits of a file (`mode & 0o777`) on Unix. `None` on Windows: Kerno reads no
/// access list there, so it cannot tell who else may read or change the file.
#[cfg(not(unix))]
pub fn mode(_meta: &Metadata) -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unix: the modes, from the creation. Windows: the file and the folder are made, and
    /// nothing claims to know who may read them.
    #[test]
    fn files_and_folders_are_made_for_their_owner() {
        use std::io::Write as _;
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("private");
        create_dir(&folder).unwrap();
        assert_eq!(
            create_dir(&folder).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        let file = folder.join("secret");
        file_options()
            .write(true)
            .create_new(true)
            .open(&file)
            .unwrap()
            .write_all(b"x")
            .unwrap();
        let mode_of = |path: &Path| mode(&std::fs::metadata(path).unwrap());
        let shared = dir.path().join("shared");
        std::fs::write(&shared, "x").unwrap();
        restrict(&shared, 0o600).unwrap();
        if cfg!(unix) {
            assert_eq!(mode_of(&folder), Some(0o700));
            assert_eq!(mode_of(&file), Some(0o600));
            assert_eq!(mode_of(&shared), Some(0o600));
        } else {
            assert_eq!(mode_of(&folder), None);
            assert_eq!(mode_of(&file), None);
            assert_eq!(mode_of(&shared), None);
        }
    }
}
