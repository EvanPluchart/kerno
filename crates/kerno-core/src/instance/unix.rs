//! The running instance's channel on Unix: a socket in the data directory,
//! the user's alone (0600). A socket file left by a crash refuses
//! connections: it is replaced.
//!
//! A socket's path is short (104 bytes on macOS, 108 on Linux, its end
//! included): a data directory too deep for it has its socket in a private
//! folder of the user's runtime or temporary directory instead, named after
//! the data directory (K56).

use std::io::{self, BufRead as _, BufReader, ErrorKind, Read as _, Write as _};
pub use std::os::unix::net::UnixListener as Listener;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use super::{MAX_LINE, READ_TIMEOUT};

/// The socket's file name in the data directory.
pub const SOCKET: &str = "instance.sock";

/// The longest socket path both systems take, in bytes, without the end
/// byte `sun_path` keeps for it (macOS has room for 104 with it).
const MAX_SOCKET_PATH: usize = 103;

/// The socket of the instance using `data_dir`: in it when the path fits,
/// otherwise in a private folder of the user's runtime directory
/// (`XDG_RUNTIME_DIR`) or temporary directory, named after a hash of the
/// data directory, one per data directory. That folder is made here when
/// missing (0700); one that other users could reach, or that is not the
/// data directory's owner's, is refused: no URL is ever sent there.
pub fn socket_path(data_dir: &Path) -> io::Result<PathBuf> {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .unwrap_or_else(std::env::temp_dir);
    socket_path_under(data_dir, &base)
}

/// [`socket_path`], with the short socket's folder under `base`.
fn socket_path_under(data_dir: &Path, base: &Path) -> io::Result<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
    let path = data_dir.join(SOCKET);
    if path.as_os_str().len() <= MAX_SOCKET_PATH {
        return Ok(path);
    }
    let owner = std::fs::metadata(data_dir)?.uid();
    let dir = base.join(format!("kerno-{owner}"));
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Err(error) if error.kind() != ErrorKind::AlreadyExists => return Err(error),
        _ => {}
    }
    // Never followed: a link put there by someone else is refused.
    let found = std::fs::symlink_metadata(&dir)?;
    if !found.is_dir() || found.uid() != owner || found.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::new(
            ErrorKind::PermissionDenied,
            format!(
                "{} is not a folder of this user's alone: the instance socket is not put there",
                dir.display()
            ),
        ));
    }
    Ok(dir.join(format!(
        "{:016x}.sock",
        fnv1a(data_dir.as_os_str().as_bytes())
    )))
}

/// FNV-1a, 64 bits: the same for a data directory in every build, so an
/// instance and a `kerno <url>` of another version still meet.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Hands `line` to the running instance. Fails when none listens.
pub fn send(path: &Path, line: &str) -> io::Result<()> {
    let mut stream = UnixStream::connect(path)?;
    stream.write_all(line.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()
}

/// Whether a running instance listens on `path`.
pub fn running(path: &Path) -> bool {
    UnixStream::connect(path).is_ok()
}

/// Nothing to do at a clean exit: the socket is left, and the next start
/// replaces it (Windows removes its file, `windows.rs`).
pub fn forget() {}

/// Listens on `path`, replacing a socket left by a crash. `None` when a
/// running instance already listens there: it is left alone.
pub fn listen(path: &Path) -> io::Result<Option<UnixListener>> {
    match UnixStream::connect(path) {
        Ok(_) => return Ok(None),
        // Nobody behind it: a crash left it.
        Err(error) if error.kind() == ErrorKind::ConnectionRefused => std::fs::remove_file(path)?,
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let listener = UnixListener::bind(path)?;
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(Some(listener))
}

/// Accepts peers one after the other and hands each one's line to
/// `on_line`, until `on_line` returns false. Blocking: run it on its own
/// thread.
pub fn serve(listener: UnixListener, mut on_line: impl FnMut(String) -> bool) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        if let Some(line) = read_line(stream)
            && !on_line(line)
        {
            return;
        }
    }
}

/// One line from a peer, without its line break; `None` when it sent
/// nothing usable in time.
fn read_line(stream: UnixStream) -> Option<String> {
    // macOS refuses socket options once the peer has closed its end (a
    // peer that wrote its line and left): then nothing can block anyway.
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let mut line = String::new();
    BufReader::new(stream.take(MAX_LINE))
        .read_line(&mut line)
        .ok()?;
    let line = line.trim();
    (!line.is_empty()).then(|| line.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// K56: a data directory too deep for a socket gets one in a private
    /// folder elsewhere, the same each time and one per data directory;
    /// both ends find it; a folder other users can reach is refused.
    #[test]
    fn a_long_data_directory_gets_a_short_socket_of_its_own() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let base = tempfile::tempdir().unwrap();
        let short = dir.path().join("data");
        std::fs::create_dir_all(&short).unwrap();
        assert_eq!(
            socket_path_under(&short, base.path()).unwrap(),
            short.join(SOCKET)
        );
        let long = dir.path().join("d".repeat(60)).join("e".repeat(60));
        let other = dir.path().join("d".repeat(60)).join("f".repeat(60));
        for data in [&long, &other] {
            std::fs::create_dir_all(data).unwrap();
        }
        // The defect: the system refuses a socket there.
        assert!(long.join(SOCKET).as_os_str().len() > MAX_SOCKET_PATH);
        assert!(listen(&long.join(SOCKET)).is_err());
        let path = socket_path_under(&long, base.path()).unwrap();
        assert!(path.as_os_str().len() <= MAX_SOCKET_PATH, "{path:?}");
        assert_eq!(socket_path_under(&long, base.path()).unwrap(), path);
        assert_ne!(socket_path_under(&other, base.path()).unwrap(), path);
        let folder = path.parent().unwrap();
        let mode = std::fs::metadata(folder).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);

        let listener = listen(&path).unwrap().expect("nobody listens yet");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let sent = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            read_line(stream)
        });
        send(
            &socket_path_under(&long, base.path()).unwrap(),
            "postgres://h/k56",
        )
        .unwrap();
        assert_eq!(sent.join().unwrap().as_deref(), Some("postgres://h/k56"));

        // A folder other users can reach is not used.
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o755)).unwrap();
        let refused = socket_path_under(&long, base.path()).unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::PermissionDenied);
    }

    #[test]
    fn a_socket_left_by_a_crash_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("first-run").join(SOCKET);
        // No instance: the hand-off fails, the caller opens the window.
        assert!(send(&path, "postgres://h").is_err());
        // The directory is made on a first run.
        drop(listen(&path).unwrap().unwrap());
        assert!(path.exists(), "a dropped listener leaves its file");
        assert!(send(&path, "postgres://h").is_err());
        let listener = listen(&path)
            .unwrap()
            .expect("the stale socket is replaced");
        send(&path, "postgres://h").unwrap();
        let (stream, _) = listener.accept().unwrap();
        assert_eq!(read_line(stream).as_deref(), Some("postgres://h"));
    }
}
