//! Updates (CLAUDE.md §3 item 12, docs/DECISIONS.md D-045): which release is newer, the one
//! request and the downloads as `curl` arguments, the checksum a download must have, and what a
//! release tarball may hold. No I/O here: the app runs the commands (`crates/kerno/src/update.rs`).
//!
//! `Version`, `newer`, `checksum_for` and `release_page` are taken from Recto
//! (`recto-core/src/update.rs`, github.com/EvanPluchart/recto, MIT, same owner); the address check
//! is stricter here: only Kerno's own release tags are read.

use std::ffi::OsString;
use std::fmt;
use std::path::{Component, Path};

mod notes;

pub use notes::{NOTES_MAX_BYTES, WhatsNew, notes_args, notes_url, whats_new};

/// Where GitHub redirects to the newest published release; the only address Kerno asks by
/// itself.
pub const LATEST_URL: &str = "https://github.com/EvanPluchart/kerno/releases/latest";
/// What the redirect of [`LATEST_URL`] must start with to be read.
const TAG_PREFIX: &str = "https://github.com/EvanPluchart/kerno/releases/tag/";

/// Largest `SHA256SUMS` accepted, and largest artifact.
pub const SUMS_MAX_BYTES: u64 = 64 * 1024;
pub const ARTIFACT_MAX_BYTES: u64 = 1024 * 1024 * 1024;

/// The release page of `version`, for a copy that cannot install itself.
pub fn release_page(version: Version) -> String {
    format!("{TAG_PREFIX}v{version}")
}

/// A release version, `major.minor.patch`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    /// `0.1.2` or `v0.1.2`. Anything else (a pre-release, a stray suffix) is `None`: Kerno
    /// offers only plain releases.
    pub fn parse(text: &str) -> Option<Version> {
        let text = text.trim();
        let text = text.strip_prefix('v').unwrap_or(text);
        let mut parts = text.split('.');
        let mut next = || -> Option<u32> {
            let part = parts.next()?;
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            part.parse().ok()
        };
        let version = Version {
            major: next()?,
            minor: next()?,
            patch: next()?,
        };
        parts.next().is_none().then_some(version)
    }

    /// Where a file of this release downloads from: constants and the three numbers only.
    pub fn download_url(self, file: &str) -> String {
        format!("https://github.com/EvanPluchart/kerno/releases/download/v{self}/{file}")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// `latest` when it is strictly newer than `current`; never a downgrade or the same version.
pub fn newer(current: &str, latest: Version) -> Option<Version> {
    (Version::parse(current)? < latest).then_some(latest)
}

/// The version GitHub's answer to [`LATEST_URL`] names: `curl --head` output whose last
/// response is a `302` to one of Kerno's release tags. Anything else (no release yet: a 404 or
/// a redirect to the list; another host; a pre-release) is `None`.
pub fn latest_from_head(head: &str) -> Option<Version> {
    // A proxy answers `CONNECT` first: the server's response is the last block.
    let head = head.replace("\r\n", "\n");
    let block = head
        .split("\n\n")
        .filter(|block| !block.trim().is_empty())
        .last()?;
    let mut lines = block.lines();
    let status = lines.next()?;
    let mut words = status.split_whitespace();
    if !words.next()?.starts_with("HTTP/") || words.next()? != "302" {
        return None;
    }
    let location = lines.find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("location")
            .then(|| value.trim())
    })?;
    let tag = location.strip_prefix(TAG_PREFIX)?;
    Version::parse(tag.strip_suffix('/').unwrap_or(tag))
}

/// The SHA-256 that `SHA256SUMS` (`sha256sum` / `shasum -a 256` output) gives for `file`.
pub fn checksum_for(sums: &str, file: &str) -> Option<[u8; 32]> {
    sums.lines().find_map(|line| {
        let (hex, name) = line.trim().split_once(char::is_whitespace)?;
        // `sha256sum` marks binary mode with a `*` before the name.
        let name = name.trim_start().trim_start_matches('*');
        (name == file).then(|| parse_hex(hex)).flatten()
    })
}

pub(crate) fn parse_hex(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// The release file for this system, as `scripts/release.sh` names it (keep both in step):
/// `Kerno-X.Y.Z-macos-<arch>.dmg`, `kerno-X.Y.Z-linux-<arch>.tar.gz`, and on Windows the installer
/// of `scripts/bundle-windows.sh`, `Kerno-X.Y.Z-windows-<arch>-setup.exe`; `arch` is
/// `std::env::consts::ARCH`, named as `uname -m` names it. `None` elsewhere.
/// These names and the ones `scripts/release.sh` writes go together: change both.
pub fn artifact(version: Version, os: &str, arch: &str) -> Option<String> {
    match os {
        "macos" => {
            let arch = if arch == "aarch64" { "arm64" } else { arch };
            Some(format!("Kerno-{version}-macos-{arch}.dmg"))
        }
        "linux" => Some(format!("{}.tar.gz", tarball_folder(version, arch))),
        "windows" => Some(format!("Kerno-{version}-windows-{arch}-setup.exe")),
        _ => None,
    }
}

/// The one folder at the top of the Linux tarball.
pub fn tarball_folder(version: Version, arch: &str) -> String {
    format!("kerno-{version}-linux-{arch}")
}

/// The update check: one `HEAD` of [`LATEST_URL`], the redirect not followed. `--disable` comes
/// first so no `~/.curlrc` adds anything; no cookie, no identifier, a user agent naming Kerno.
pub fn check_args(current: &str) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "--disable",
        "--silent",
        "--head",
        "--max-time",
        "10",
        "--proto",
        "=https",
        "--max-redirs",
        "0",
        "--user-agent",
    ]
    .map(OsString::from)
    .into();
    args.push(format!("Kerno/{current}").into());
    args.extend(["--", LATEST_URL].map(OsString::from));
    args
}

/// A download of `url` into `output` (a name of Kerno's, never the server's), HTTPS only, at
/// most `max_bytes`.
pub fn download_args(current: &str, url: &str, output: &Path, max_bytes: u64) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "--disable",
        "--silent",
        "--show-error",
        "--fail",
        "--location",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--max-time",
        "600",
        "--max-filesize",
    ]
    .map(OsString::from)
    .into();
    args.push(max_bytes.to_string().into());
    args.push("--user-agent".into());
    args.push(format!("Kerno/{current}").into());
    args.push("--output".into());
    args.push(output.into());
    args.push("--".into());
    args.push(url.into());
    args
}

/// Checks a tarball's listings before anything is extracted: `names` is `tar -tzf` output,
/// `verbose` is `tar -tvzf` output. Every entry is a plain file or a folder (no link: a name
/// alone cannot stop a write through one), relative, without `..`, inside `top`.
pub fn check_tarball(names: &str, verbose: &str, top: &str) -> Result<(), String> {
    let names: Vec<&str> = names.lines().filter(|line| !line.is_empty()).collect();
    if names.is_empty() {
        return Err("the archive is empty".into());
    }
    for name in &names {
        let mut components = Path::new(name).components();
        let inside = matches!(components.next(), Some(Component::Normal(first)) if first == top)
            && components.all(|part| matches!(part, Component::Normal(_)));
        if !inside {
            return Err(format!("the archive holds “{name}”, outside {top}/"));
        }
    }
    let kinds: Vec<&str> = verbose.lines().filter(|line| !line.is_empty()).collect();
    if kinds.len() != names.len() {
        return Err("the archive's two listings differ".into());
    }
    if let Some(line) = kinds
        .iter()
        .find(|line| !(line.starts_with('-') || line.starts_with('d')))
    {
        return Err(format!(
            "the archive holds a link or a special file: {line}"
        ));
    }
    Ok(())
}

/// Whether the update check may run: never when `KERNO_NO_UPDATE_CHECK` is set, never in a
/// bench run (`KERNO_BENCH`). The setting is read at each check, not here.
pub fn checks_allowed(no_update_check: bool, bench: bool) -> bool {
    !no_update_check && !bench
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn versions_compare_by_number() {
        assert_eq!(v("v0.1.2"), v("0.1.2"));
        assert!(v("0.1.10") > v("0.1.9"));
        assert!(v("1.0.0") > v("0.99.99"));
        for bad in [
            "",
            "v",
            "0.1",
            "0.1.2.3",
            "0.1.2-beta",
            "0.1.2-rc.1",
            "0.1.x",
            "0..1",
            " v0.1.+2",
            "99999999999.0.0",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn only_a_newer_release_is_offered() {
        assert_eq!(newer("0.1.1", v("0.1.2")), Some(v("0.1.2")));
        assert_eq!(newer("0.1.2", v("0.1.2")), None);
        assert_eq!(newer("0.2.0", v("0.1.9")), None);
        assert_eq!(newer("not a version", v("9.9.9")), None);
    }

    fn head(status: &str, location: &str) -> String {
        format!("HTTP/2 {status}\r\ndate: x\r\nLocation: {location}\r\ncontent-length: 0\r\n\r\n")
    }

    #[test]
    fn the_redirect_of_the_latest_release_names_the_version() {
        let tag = "https://github.com/EvanPluchart/kerno/releases/tag/v0.2.0";
        assert_eq!(latest_from_head(&head("302", tag)), Some(v("0.2.0")));
        // HTTP/1.1, a lowercase header, a trailing slash.
        assert_eq!(
            latest_from_head(&format!("HTTP/1.1 302 Found\nlocation: {tag}/\n\n")),
            Some(v("0.2.0"))
        );
        // Through a proxy: its own answer comes first.
        assert_eq!(
            latest_from_head(&format!(
                "HTTP/1.1 200 Connection established\r\n\r\n{}",
                head("302", tag)
            )),
            Some(v("0.2.0"))
        );
    }

    #[test]
    fn anything_but_a_redirect_to_a_kerno_tag_offers_nothing() {
        let tag = "https://github.com/EvanPluchart/kerno/releases/tag/v0.2.0";
        // The repository does not exist (yet): 404.
        assert_eq!(
            latest_from_head("HTTP/2 404\r\ncontent-length: 9\r\n\r\n"),
            None
        );
        // No release yet: the redirect goes to the list.
        assert_eq!(
            latest_from_head(&head(
                "302",
                "https://github.com/EvanPluchart/kerno/releases"
            )),
            None
        );
        for (status, location) in [
            ("301", tag),
            ("200", tag),
            (
                "302",
                "https://evil.example/EvanPluchart/kerno/releases/tag/v9.0.0",
            ),
            (
                "302",
                "http://github.com/EvanPluchart/kerno/releases/tag/v9.0.0",
            ),
            (
                "302",
                "https://github.com/EvanPluchart/recto/releases/tag/v9.0.0",
            ),
            (
                "302",
                "https://github.com/EvanPluchart/kerno/releases/tag/v0.3.0-beta",
            ),
            (
                "302",
                "https://github.com/EvanPluchart/kerno/releases/tag/v0.3.0?x=1",
            ),
        ] {
            assert_eq!(
                latest_from_head(&head(status, location)),
                None,
                "{status} {location}"
            );
        }
        assert_eq!(latest_from_head(""), None);
        assert_eq!(latest_from_head("garbage"), None);
    }

    #[test]
    fn checksums_are_read_from_sha256sum_output() {
        let a = "a".repeat(64);
        let sums = format!(
            "{a}  kerno-0.2.0-linux-x86_64.tar.gz\n{}  Kerno-0.2.0-macos-arm64.dmg\n{}  *kerno-0.2.0-linux-aarch64.tar.gz\n",
            "0f".repeat(32),
            "1e".repeat(32)
        );
        assert_eq!(
            checksum_for(&sums, "Kerno-0.2.0-macos-arm64.dmg"),
            Some([0x0f; 32])
        );
        assert_eq!(
            checksum_for(&sums, "kerno-0.2.0-linux-aarch64.tar.gz"),
            Some([0x1e; 32])
        );
        // A missing line, or a malformed one, gives nothing: the download is refused.
        assert_eq!(checksum_for(&sums, "Kerno-0.2.1-macos-arm64.dmg"), None);
        assert_eq!(checksum_for(&sums, "Kerno-0.2.0-macos-arm"), None);
        assert_eq!(
            checksum_for(
                "xyz  Kerno-0.2.0-macos-arm64.dmg",
                "Kerno-0.2.0-macos-arm64.dmg"
            ),
            None
        );
    }

    #[test]
    fn artifacts_are_named_as_the_release_script_names_them() {
        let version = v("0.2.0");
        assert_eq!(
            artifact(version, "macos", "aarch64").as_deref(),
            Some("Kerno-0.2.0-macos-arm64.dmg")
        );
        assert_eq!(
            artifact(version, "macos", "x86_64").as_deref(),
            Some("Kerno-0.2.0-macos-x86_64.dmg")
        );
        assert_eq!(
            artifact(version, "linux", "x86_64").as_deref(),
            Some("kerno-0.2.0-linux-x86_64.tar.gz")
        );
        assert_eq!(
            artifact(version, "linux", "aarch64").as_deref(),
            Some("kerno-0.2.0-linux-aarch64.tar.gz")
        );
        assert_eq!(
            artifact(version, "windows", "x86_64").as_deref(),
            Some("Kerno-0.2.0-windows-x86_64-setup.exe")
        );
        assert_eq!(artifact(version, "freebsd", "x86_64"), None);
        assert_eq!(
            version.download_url("SHA256SUMS"),
            "https://github.com/EvanPluchart/kerno/releases/download/v0.2.0/SHA256SUMS"
        );
        assert_eq!(
            release_page(version),
            "https://github.com/EvanPluchart/kerno/releases/tag/v0.2.0"
        );
    }

    fn strings(args: Vec<OsString>) -> Vec<String> {
        args.into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect()
    }

    #[test]
    fn the_request_is_one_head_with_fixed_arguments() {
        assert_eq!(
            strings(check_args("0.1.0")),
            [
                "--disable",
                "--silent",
                "--head",
                "--max-time",
                "10",
                "--proto",
                "=https",
                "--max-redirs",
                "0",
                "--user-agent",
                "Kerno/0.1.0",
                "--",
                "https://github.com/EvanPluchart/kerno/releases/latest",
            ]
        );
        assert_eq!(
            strings(download_args(
                "0.1.0",
                &v("0.2.0").download_url("SHA256SUMS"),
                Path::new("/tmp/kerno-update-1/SHA256SUMS"),
                SUMS_MAX_BYTES,
            )),
            [
                "--disable",
                "--silent",
                "--show-error",
                "--fail",
                "--location",
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "--max-time",
                "600",
                "--max-filesize",
                "65536",
                "--user-agent",
                "Kerno/0.1.0",
                "--output",
                "/tmp/kerno-update-1/SHA256SUMS",
                "--",
                "https://github.com/EvanPluchart/kerno/releases/download/v0.2.0/SHA256SUMS",
            ]
        );
    }

    #[test]
    fn a_tarball_entry_may_not_leave_its_folder() {
        let top = "kerno-0.2.0-linux-x86_64";
        let names =
            format!("{top}/\n{top}/bin/\n{top}/bin/kerno\n{top}/share/doc/kerno/README.md\n");
        let verbose = "drwxr-xr-x 0/0 0 x kerno/\ndrwxr-xr-x x\n-rwxr-xr-x x\n-rw-r--r-- x\n";
        assert_eq!(check_tarball(&names, verbose, top), Ok(()));
        for bad in [
            "/etc/passwd".to_string(),
            format!("{top}/../escape"),
            format!("{top}/bin/../../escape"),
            "other/bin/kerno".to_string(),
            "./bin/kerno".to_string(),
            "../kerno".to_string(),
        ] {
            assert!(
                check_tarball(&format!("{bad}\n"), "-rw-r--r-- x\n", top).is_err(),
                "{bad}"
            );
        }
        // A link, a hard link, a device: refused, whatever their name.
        for kind in ["lrwxrwxrwx x -> /etc", "hrw-r--r-- x", "crw-r--r-- x"] {
            assert!(
                check_tarball(&format!("{top}/bin/kerno\n"), kind, top).is_err(),
                "{kind}"
            );
        }
        assert!(check_tarball("", "", top).is_err());
        assert!(check_tarball(&format!("{top}/a\n{top}/b\n"), "-rw x\n", top).is_err());
    }

    #[test]
    fn the_environment_switches_turn_the_check_off() {
        assert!(checks_allowed(false, false));
        assert!(!checks_allowed(true, false));
        assert!(!checks_allowed(false, true));
    }
}
