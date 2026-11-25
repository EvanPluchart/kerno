//! The update check and install (CLAUDE.md §3 item 12, docs/DECISIONS.md D-045): the steps that
//! run a command or touch files. What is newer, the `curl` arguments and what a tarball may hold
//! are `kerno_core::update`; the status bar item is `workspace/updates.rs`.
//!
//! Ported from the owner's Recto (`recto-app/src/update/mod.rs`) and Vigie
//! (`vigie-app/src/update.rs`), MIT, same owner: the same timing, the same refusals (no
//! checksum line, a mismatch: deleted, never run), the same "otherwise the release page". Kerno
//! swaps its own bundle on macOS and its own files on Linux, by rename, with a rollback; on
//! Windows it runs its installer for this user and quits, as Recto and Vigie do (K70, D-139).
//!
//! Every external program is run through [`Runner`] with a fixed argument array and an absolute
//! path: the real one ([`System`]) is made only in `main.rs`, after the off switches; tests use a
//! fake and never reach the network.

use std::ffi::OsString;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use gpui_kit::Global;
use kerno_core::update::{self as releases, Version};
use sha2::{Digest as _, Sha256};

/// The first check, once the window is up; then one every `EVERY` of wall time (K57, D-123),
/// looked at every `TICK`: the executor's timer stops while the computer sleeps, the wall clock
/// does not.
pub const FIRST: Duration = Duration::from_secs(3);
pub const EVERY: Duration = Duration::from_secs(6 * 3600);
pub const TICK: Duration = Duration::from_secs(60);

/// Runs the external programs of an update. One trait for `curl`, `hdiutil`, `ditto`, `tar`,
/// `whoami`, the Windows installer and the relaunch, so a test fake sees (and asserts) every
/// argument array.
pub trait Runner: Send + Sync {
    /// Runs `program` with `args` and waits: its standard output, or why it failed.
    fn output(&self, program: &Path, args: &[OsString]) -> Result<Vec<u8>, String>;
    /// Starts `program` with `args` without waiting: the new Kerno.
    fn start(&self, program: &Path, args: &[OsString]) -> Result<(), String>;
    /// [`Runner::start`] with these variables set (`Some`) or removed (`None`) for that process
    /// only: the Windows installer's, [`setup_env`].
    fn start_with_env(
        &self,
        program: &Path,
        args: &[OsString],
        env: &[(String, Option<OsString>)],
    ) -> Result<(), String>;
}

/// The environment the Windows installer is started with: `TEMP` and `TMP` set to `temp`, the
/// download's private folder, where its stub unpacks and runs its own code (K70, V1); and the
/// user's own `TEMP` and `TMP` (`own`) in `KERNO_SETUP_TEMP` and `KERNO_SETUP_TMP`, which
/// `packaging/windows/kerno.iss` puts back as soon as it starts, so that the Kerno it starts
/// (the new one, or the old one again) does not keep a `TEMP` that is removed at that start (W1).
/// An unset or empty one is passed as removed and is unset again (`KERNO_SETUP_ENV=1` says the
/// two are there to restore).
pub fn setup_env(
    temp: &Path,
    own: impl Fn(&str) -> Option<OsString>,
) -> Vec<(String, Option<OsString>)> {
    let mut env = Vec::new();
    for name in ["TEMP", "TMP"] {
        env.push((name.to_string(), Some(temp.as_os_str().to_owned())));
        env.push((
            format!("KERNO_SETUP_{name}"),
            own(name).filter(|value| !value.is_empty()),
        ));
    }
    env.push(("KERNO_SETUP_ENV".to_string(), Some("1".into())));
    env
}

/// The system's own programs.
pub struct System;

impl Runner for System {
    fn output(&self, program: &Path, args: &[OsString]) -> Result<Vec<u8>, String> {
        let mut command = std::process::Command::new(program);
        command.args(args).stdin(std::process::Stdio::null());
        kerno_core::tools::no_console(&mut command);
        let output = command
            .output()
            .map_err(|error| format!("{} could not run: {error}", program.display()))?;
        if output.status.success() {
            Ok(output.stdout)
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let why = stderr.lines().next().unwrap_or("").trim();
            Err(
                format!("{} failed ({}) {why}", program.display(), output.status)
                    .trim()
                    .to_string(),
            )
        }
    }

    fn start(&self, program: &Path, args: &[OsString]) -> Result<(), String> {
        std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(drop)
            .map_err(|error| format!("{} could not start: {error}", program.display()))
    }

    fn start_with_env(
        &self,
        program: &Path,
        args: &[OsString],
        env: &[(String, Option<OsString>)],
    ) -> Result<(), String> {
        let mut command = std::process::Command::new(program);
        for (name, value) in env {
            match value {
                Some(value) => command.env(name, value),
                None => command.env_remove(name),
            };
        }
        command
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(drop)
            .map_err(|error| format!("{} could not start: {error}", program.display()))
    }
}

/// The absolute path of a system program: the first of `places` that exists, else the first
/// (running it then says it is missing).
fn program(places: &[&str]) -> PathBuf {
    let found = places.iter().find(|place| Path::new(place).is_file());
    PathBuf::from(found.or(places.first()).copied().unwrap_or_default())
}

#[cfg(not(windows))]
fn curl() -> Result<PathBuf, String> {
    Ok(program(&["/usr/bin/curl", "/bin/curl"]))
}

/// Windows 10 1803 and later ship `curl.exe` in `System32`: that one, never through `PATH`, the
/// current folder or the application's, where a `curl.exe` put earlier would be run instead (K67,
/// D-136), and only from `C:\Windows` (K70, D-139: `kerno_core::tools::system_program`). Missing,
/// or a Windows elsewhere, the check says nothing, as offline.
#[cfg(windows)]
fn curl() -> Result<PathBuf, String> {
    kerno_core::tools::windows_program("curl.exe")
        .ok_or_else(|| r"Windows is not in C:\Windows: Kerno runs no curl.exe".to_string())
}

fn tar() -> PathBuf {
    program(&["/usr/bin/tar", "/bin/tar"])
}

const HDIUTIL: &str = "/usr/bin/hdiutil";
const DITTO: &str = "/usr/bin/ditto";
const OPEN: &str = "/usr/bin/open";

/// How this copy of Kerno can be updated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Install {
    /// macOS: this `Kerno.app`, in a folder Kerno may write.
    App(PathBuf),
    /// Linux: the tarball's install prefix (`<dir>/bin/kerno`, `<dir>/share/…`), writable. It
    /// may be `~/.local`: only the files the tarball carries are replaced, never the folder.
    Prefix(PathBuf),
    /// Windows: this `kerno.exe`, installed by the installer or unpacked from the zip (its folder
    /// holds the licences). The installer installs the new version for this user into
    /// `%LOCALAPPDATA%\Programs\Kerno`, and starts this one again when it could not.
    Setup(PathBuf),
    /// Anything else (the bare `target/release/kerno`, a read-only place, a package manager's
    /// install, a translocated app, a Kerno running as administrator on Windows): the release
    /// page.
    Page,
}

/// How the program at `exe` can be updated on `os` (`std::env::consts::OS`).
pub fn install_target(exe: &Path, os: &str) -> Install {
    let name = |path: Option<&Path>| {
        path.and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
    };
    let parent = exe.parent();
    match os {
        "macos" => {
            let contents = parent.and_then(Path::parent);
            let bundle = contents.and_then(Path::parent);
            let laid_out = name(Some(exe)).as_deref() == Some("kerno")
                && name(parent).as_deref() == Some("MacOS")
                && name(contents).as_deref() == Some("Contents")
                && name(bundle).as_deref() == Some("Kerno.app");
            match bundle {
                Some(bundle)
                    if laid_out
                        // Gatekeeper's read-only copy of a quarantined app.
                        && !bundle.to_string_lossy().contains("/AppTranslocation/")
                        && bundle.parent().is_some_and(writable) =>
                {
                    Install::App(bundle.to_path_buf())
                }
                _ => Install::Page,
            }
        }
        "linux" => {
            let dir = parent.and_then(Path::parent);
            match dir {
                Some(dir)
                    if name(Some(exe)).as_deref() == Some("kerno")
                        && name(parent).as_deref() == Some("bin")
                        && dir.join("share/applications/kerno.desktop").is_file()
                        && writable(&dir.join("bin")) =>
                {
                    Install::Prefix(dir.to_path_buf())
                }
                _ => Install::Page,
            }
        }
        // The installer writes elsewhere: nothing to probe here. A bare build
        // (`target\release\kerno.exe`) has no licences beside it.
        "windows" => {
            let laid_out = name(Some(exe))
                .is_some_and(|name| name.eq_ignore_ascii_case("kerno.exe"))
                && parent.is_some_and(|dir| dir.join("LICENSE-MIT").is_file());
            if laid_out {
                Install::Setup(exe.to_path_buf())
            } else {
                Install::Page
            }
        }
        _ => Install::Page,
    }
}

/// Windows: whether this process may run a downloaded installer, from `whoami /groups /fo csv
/// /nh` (asking Windows directly needs `unsafe`). See [`standard_user`].
#[cfg(any(test, not(feature = "test-update-without-click")))]
fn elevated(runner: &dyn Runner, whoami: Option<PathBuf>) -> bool {
    // Through `System::output`, which starts it without a console window (K67's `no_console`);
    // none outside `C:\Windows` (V3): then Kerno cannot tell, and counts it as elevated.
    let Some(whoami) = whoami else {
        return true;
    };
    let args = ["/groups", "/fo", "csv", "/nh"].map(OsString::from);
    let groups = runner.output(&whoami, &args).ok();
    !standard_user(groups.as_deref())
}

/// The integrity level of the CSV rows of `whoami /groups /fo csv /nh` (`"name","type","SID",
/// "attributes"`), read by its SID, the one column Windows never translates: true only when the
/// one level is Medium (`S-1-16-8192`), the level of a program a user starts. High (12288) and
/// System (16384) are elevated: an installer run from there would run with those rights, and a
/// process of the same user without them could swap the checked file before it runs. Low (4096)
/// is a sandbox the installer could not write from. No level, two levels, an empty answer, no
/// `whoami` or a failed one (`None`): Kerno cannot tell, and opens the release page rather than run
/// an installer.
#[cfg(any(test, not(feature = "test-update-without-click")))]
fn standard_user(groups: Option<&[u8]>) -> bool {
    let Some(groups) = groups else {
        return false;
    };
    let groups = String::from_utf8_lossy(groups);
    let mut levels = groups.lines().filter_map(|row| {
        let sid = row.trim().split("\",\"").nth(2)?;
        sid.starts_with("S-1-16-").then_some(sid)
    });
    levels.next() == Some("S-1-16-8192") && levels.next().is_none()
}

/// Whether Kerno may make (and rename) entries in `dir`: tried, not guessed.
fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".kerno-update-probe-{}", std::process::id()));
    fs::create_dir(&probe).is_ok() && fs::remove_dir(&probe).is_ok()
}

/// A new folder only this user can read, made here and failing if it exists: no one else can
/// put a file where Kerno looks.
fn private_dir(parent: &Path, prefix: &str) -> Result<PathBuf, String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or_default();
    let dir = parent.join(format!("{prefix}-{}-{stamp}", std::process::id()));
    kerno_core::private::create_dir(&dir)
        .map_err(|error| format!("could not make {}: {error}", dir.display()))?;
    Ok(dir)
}

/// The newest release when it is newer than `current`; `Ok(None)` when Kerno is up to date;
/// `Err` when it could not tell (offline, no `curl`, no release published, an odd answer).
pub fn check(runner: &dyn Runner, current: &str) -> Result<Option<Version>, String> {
    let head = runner.output(&curl()?, &releases::check_args(current))?;
    let latest = releases::latest_from_head(&String::from_utf8_lossy(&head))
        .ok_or("GitHub named no release")?;
    Ok(releases::newer(current, latest))
}

/// Downloads this system's file of `version` and its `SHA256SUMS` into a new private folder
/// under `temp` ([`downloads_dir`]), and checks the file's SHA-256 against its line. Any failure
/// deletes the folder: an unchecked file is never kept, run or opened.
pub fn download(
    runner: &dyn Runner,
    current: &str,
    version: Version,
    temp: &Path,
) -> Result<PathBuf, String> {
    let name = releases::artifact(version, std::env::consts::OS, std::env::consts::ARCH)
        .ok_or("no release file for this system")?;
    fs::create_dir_all(temp)
        .map_err(|error| format!("could not make {}: {error}", temp.display()))?;
    let dir = private_dir(temp, "kerno-update")?;
    let checked = (|| {
        let sums_path = dir.join("SHA256SUMS");
        let url = version.download_url("SHA256SUMS");
        let args = releases::download_args(current, &url, &sums_path, releases::SUMS_MAX_BYTES);
        runner.output(&curl()?, &args)?;
        let sums = fs::read_to_string(&sums_path).map_err(|_| "the checksums could not be read")?;
        let expected = releases::checksum_for(&sums, &name)
            .ok_or_else(|| format!("the release lists no checksum for {name}"))?;
        // Kerno's own name for the file, never one the server sends.
        let path = dir.join(&name);
        let args = releases::download_args(
            current,
            &version.download_url(&name),
            &path,
            releases::ARTIFACT_MAX_BYTES,
        );
        runner.output(&curl()?, &args)?;
        if fs::metadata(&path).map_or(0, |meta| meta.len()) == 0 {
            return Err("the download is empty".to_string());
        }
        open_checked(&path, expected)?;
        Ok(path)
    })();
    if checked.is_err() {
        let _ = fs::remove_dir_all(&dir);
    }
    checked
}

/// Opens `path` held shut and checks its SHA-256 THROUGH that handle: while the returned file
/// lives, on Windows nobody may write, rename or delete it (share mode: read only), so what was
/// checked is what a program started from it runs. A file someone else holds open for writing
/// cannot be opened so: refused. On macOS and Linux the download's folder is 0700 from its creation
/// (`private::create_dir`): no other account can reach the file at all.
fn open_checked(path: &Path, expected: [u8; 32]) -> Result<fs::File, String> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        // FILE_SHARE_READ alone.
        options.share_mode(1);
    }
    let mut file = options
        .open(path)
        .map_err(|error| format!("could not hold {} shut: {error}", path.display()))?;
    if sha256(&mut file)? != expected {
        return Err("the download does not match its checksum".to_string());
    }
    Ok(file)
}

fn sha256(file: &mut fs::File) -> Result<[u8; 32], String> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

/// Installs the checked `file` of `version` over `target` and starts the new Kerno; this
/// process should then quit. On a failure everything is put back as it was. On Windows the
/// installer does the work once this process has quit: `data_dir` keeps the note
/// [`after_install`] reads at the next start.
pub fn install(
    runner: &dyn Runner,
    target: &Install,
    file: &Path,
    version: Version,
    data_dir: Option<&Path>,
) -> Result<(), String> {
    match target {
        Install::App(bundle) => install_app(runner, bundle, file),
        Install::Prefix(dir) => install_prefix(runner, dir, file, version),
        Install::Setup(exe) => install_setup(runner, exe, file, version, data_dir),
        Install::Page => Err("this copy of Kerno opens the release page instead".into()),
    }
}

/// The note an update on Windows leaves in the data folder before it runs the installer: the
/// version and the download's folder, one per line.
pub const PENDING: &str = "update-pending.txt";
/// The installer's own log of that update, in the data folder.
pub const SETUP_LOG: &str = "update-setup.log";

/// The installer's switches for an update from the app: no window and no question, never a
/// restart of Windows, no Kerno closed by it, and the new Kerno started at the end
/// (`packaging/windows/kerno.iss`).
const SETUP_ARGS: [&str; 5] = [
    "/VERYSILENT",
    "/SUPPRESSMSGBOXES",
    "/NORESTART",
    "/NOCLOSEAPPLICATIONS",
    "/relaunch=1",
];

/// Windows: notes the update in `data_dir`, then starts the checked installer for this user; it
/// waits for this process (`/pid`) to quit, and starts this `exe` again when it could not install.
/// The installer is checked again against the `SHA256SUMS` downloaded with it, through a handle
/// held from that check until the installer's process exists ([`open_checked`]). The download
/// stays: the installer reads its files from it while it runs.
fn install_setup(
    runner: &dyn Runner,
    exe: &Path,
    setup: &Path,
    version: Version,
    data_dir: Option<&Path>,
) -> Result<(), String> {
    let folder = setup.parent().ok_or("the download has no folder")?;
    let name = setup.file_name().ok_or("the download has no name")?;
    let sums = fs::read_to_string(folder.join("SHA256SUMS"))
        .map_err(|_| "the checksums could not be read")?;
    let expected = releases::checksum_for(&sums, &name.to_string_lossy())
        .ok_or("the release lists no checksum for the installer")?;
    let held = open_checked(setup, expected)?;
    let pending = data_dir.map(|dir| dir.join(PENDING));
    if let Some(pending) = &pending {
        fs::write(pending, format!("{version}\n{}\n", folder.display()))
            .map_err(|error| format!("could not write {}: {error}", pending.display()))?;
    }
    let with = |flag: &str, value: &Path| {
        let mut arg = OsString::from(flag);
        arg.push(value);
        arg
    };
    let mut args: Vec<OsString> = SETUP_ARGS.map(OsString::from).into();
    args.push(format!("/pid={}", std::process::id()).into());
    args.push(with("/kerno=", exe));
    if let Some(dir) = data_dir {
        args.push(with("/LOG=", &dir.join(SETUP_LOG)));
    }
    // The installer's stub unpacks its own code into `TEMP` and runs it: the download's private
    // folder, not a `%TEMP%` other accounts may write (V1); the user's own comes back for the
    // Kerno it starts (W1).
    let env = setup_env(folder, |name| std::env::var_os(name));
    let started = runner.start_with_env(setup, &args, &env).inspect_err(|_| {
        if let Some(pending) = &pending {
            let _ = fs::remove_file(pending);
        }
    });
    // Only now: the installer's process has its file open.
    drop(held);
    started
}

/// Where downloads go: on Windows `Kerno\data\updates` in the profile's local application data
/// (`%LOCALAPPDATA%`), whatever `KERNO_DATA_DIR` says, since `%TEMP%` may be a folder other
/// accounts can write and Windows gives a new folder no access list of Kerno's (D-131); `None`
/// when Windows names no such folder. Elsewhere the system's temporary folder, where the download's
/// own folder is made 0700.
pub fn downloads_dir() -> Option<PathBuf> {
    // The Windows runner's update check only: a folder with a space and an accent (V6).
    #[cfg(feature = "test-update-without-click")]
    if let Some(dir) = std::env::var_os("KERNO_TEST_DOWNLOADS_DIR") {
        return Some(PathBuf::from(dir));
    }
    if cfg!(windows) {
        kerno_core::storage::AppPaths::local_data_dir().map(|dir| dir.join("updates"))
    } else {
        Some(std::env::temp_dir())
    }
}

/// Windows: removes every `kerno-update-*` folder of `downloads` (a failed install's, a download
/// that was declined), except the folders of a Kerno that still runs: a folder is named
/// `kerno-update-<pid>-<time>` by the Kerno that downloads into it, and one whose `<pid>` is
/// `kerno_alive` (another Kerno's download in progress, or checked and waiting for its quit
/// question) is left (W2). One still in use (an installer running from it) cannot be removed and
/// is returned, to try again later (K70, V4).
pub fn sweep_downloads(downloads: &Path, kerno_alive: &dyn Fn(u32) -> bool) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(downloads) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(rest) = name.strip_prefix("kerno-update-") else {
                return false;
            };
            let owner = rest
                .split('-')
                .next()
                .and_then(|pid| pid.parse::<u32>().ok());
            entry.file_type().is_ok_and(|kind| kind.is_dir()) && !owner.is_some_and(kerno_alive)
        })
        .map(|entry| entry.path())
        .filter(|folder| fs::remove_dir_all(folder).is_err() && folder.exists())
        .collect()
}

/// Windows: whether process `pid` is a running `kerno.exe` (this one included), from
/// `tasklist /fi "PID eq <pid>" /fo csv /nh`; when `tasklist.exe` cannot answer, it counts as
/// running, and the folder stays (W2).
pub fn kerno_alive(runner: &dyn Runner, pid: u32) -> bool {
    if pid == std::process::id() {
        return true;
    }
    let Some(tasklist) = kerno_core::tools::windows_program("tasklist.exe") else {
        return true;
    };
    let args = ["/fi", &format!("PID eq {pid}"), "/fo", "csv", "/nh"].map(OsString::from);
    runner.output(&tasklist, &args).map_or(true, |out| {
        tasklist_names_kerno(&String::from_utf8_lossy(&out), pid)
    })
}

/// Whether `tasklist /fo csv /nh`'s answer has a row `"kerno.exe","<pid>",…`. Any other answer
/// (no such process, in whatever language Windows says it; another program now holding that pid)
/// is false.
fn tasklist_names_kerno(out: &str, pid: u32) -> bool {
    let pid = pid.to_string();
    out.lines().any(|row| {
        let mut fields = row.trim().split("\",\"");
        fields.next().is_some_and(|image| {
            image
                .trim_start_matches('"')
                .eq_ignore_ascii_case("kerno.exe")
        }) && fields.next() == Some(pid.as_str())
    })
}

/// What the last update on Windows left, read at each start (on every system: the note exists
/// only after one). The note goes, with the download it names. `None` when there was none, or
/// when this Kerno is that version or newer; else that version and why it was not installed, said
/// once.
pub fn after_install(data_dir: &Path, current: &str, temp: &Path) -> Option<(Version, String)> {
    let pending = data_dir.join(PENDING);
    let note = fs::read_to_string(&pending).ok()?;
    let _ = fs::remove_file(&pending);
    let mut lines = note.lines();
    let version = lines.next().and_then(Version::parse)?;
    // Only a download folder of Kerno's own name, in the temporary folder.
    if let Some(folder) = lines.next().map(PathBuf::from)
        && folder.parent() == Some(temp)
        && folder
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("kerno-update-"))
    {
        let _ = fs::remove_dir_all(&folder);
    }
    releases::newer(current, version)?;
    let why = format!(
        "its installer stopped before it finished, and this is still Kerno {current}; what it \
         did is in {}",
        data_dir.join(SETUP_LOG).display()
    );
    tracing::warn!(%version, "the update was not installed: {why}");
    Some((version, why))
}

/// macOS: mounts the disk image read-only without showing it, copies its `Kerno.app` beside
/// `bundle`, swaps the two by rename, starts the new one.
fn install_app(runner: &dyn Runner, bundle: &Path, dmg: &Path) -> Result<(), String> {
    let parent = bundle.parent().ok_or("the app has no folder")?;
    let work = dmg.parent().ok_or("the download has no folder")?;
    let mount = private_dir(work, "mount")?;
    let os = |text: &str| OsString::from(text);
    let attach = [
        os("attach"),
        os("-nobrowse"),
        os("-readonly"),
        os("-noautoopen"),
        os("-mountpoint"),
        mount.clone().into(),
        dmg.into(),
    ];
    runner.output(Path::new(HDIUTIL), &attach)?;
    let pid = std::process::id();
    let staged = parent.join(format!("Kerno.app.new-{pid}"));
    let copied = (|| {
        let source = mount.join("Kerno.app");
        if !source.join("Contents/MacOS/kerno").is_file() {
            return Err("the disk image holds no Kerno.app".to_string());
        }
        runner.output(Path::new(DITTO), &[source.into(), staged.clone().into()])?;
        Ok(())
    })();
    // Detached whatever happened.
    let _ = runner.output(
        Path::new(HDIUTIL),
        &[os("detach"), mount.clone().into(), os("-quiet")],
    );
    let _ = fs::remove_dir(&mount);
    if let Err(error) = copied {
        let _ = fs::remove_dir_all(&staged);
        return Err(error);
    }
    let old = parent.join(format!("Kerno.app.old-{pid}"));
    swap_in(bundle, &staged, &old)?;
    if let Err(error) = runner.start(Path::new(OPEN), &[os("-n"), bundle.into()]) {
        undo_swap(bundle, &staged, &old);
        let _ = fs::remove_dir_all(&staged);
        return Err(error);
    }
    // The old bundle is removed at the next start ([`remove_leftovers`]).
    Ok(())
}

/// `current` → `old`, `new` → `current`; on a failure `current` is what it was.
fn swap_in(current: &Path, new: &Path, old: &Path) -> Result<(), String> {
    fs::rename(current, old).map_err(|error| {
        let _ = fs::remove_dir_all(new);
        format!("could not move {} aside: {error}", current.display())
    })?;
    if let Err(error) = fs::rename(new, current) {
        let _ = fs::rename(old, current);
        let _ = fs::remove_dir_all(new);
        return Err(format!("could not put the new version in place: {error}"));
    }
    Ok(())
}

/// Puts `old` back at `current`, the new version going back to `new`.
fn undo_swap(current: &Path, new: &Path, old: &Path) {
    if fs::rename(current, new).is_ok() {
        let _ = fs::rename(old, current);
    }
}

/// Linux: checks the tarball's listing, extracts it into a private folder inside `dir` (the
/// same disk, so renames work), replaces each of its files in `dir` by rename (the old one kept
/// aside until the new Kerno has started), starts `<dir>/bin/kerno`.
fn install_prefix(
    runner: &dyn Runner,
    dir: &Path,
    tarball: &Path,
    version: Version,
) -> Result<(), String> {
    let top = releases::tarball_folder(version, std::env::consts::ARCH);
    let os = |text: &str| OsString::from(text);
    let listing = |verbose: bool| {
        let mut args = vec![os("-t")];
        if verbose {
            args.push(os("-v"));
        }
        args.extend([os("-z"), os("-f"), tarball.into()]);
        runner
            .output(&tar(), &args)
            .map(|out| String::from_utf8_lossy(&out).into_owned())
    };
    releases::check_tarball(&listing(false)?, &listing(true)?, &top)?;
    let work = private_dir(dir, ".kerno-update")?;
    let result = (|| {
        let extract = [
            os("-x"),
            os("-z"),
            os("-f"),
            tarball.into(),
            os("-C"),
            work.clone().into(),
        ];
        runner.output(&tar(), &extract)?;
        let root = work.join(&top);
        let mut files = Vec::new();
        collect_files(&root, Path::new(""), &mut files)?;
        if !files.iter().any(|file| file == Path::new("bin/kerno")) {
            return Err("the archive holds no bin/kerno".to_string());
        }
        let suffix = format!("old-{}", std::process::id());
        let swapped = replace_files(dir, &root, &files, &suffix)?;
        if let Err(error) = runner.start(&dir.join("bin/kerno"), &[]) {
            restore_files(&swapped);
            return Err(error);
        }
        // The running Kerno keeps its open file; the old copies can go now.
        for old in swapped.into_iter().filter_map(|(_, old)| old) {
            let _ = fs::remove_file(old);
        }
        Ok(())
    })();
    let _ = fs::remove_dir_all(&work);
    result
}

/// The regular files under `root/rel`, relative to `root`; a link or anything else refuses.
fn collect_files(root: &Path, rel: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(root.join(rel)).map_err(|error| error.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let rel = rel.join(entry.file_name());
        let kind = entry.file_type().map_err(|error| error.to_string())?;
        if kind.is_dir() {
            collect_files(root, &rel, files)?;
        } else if kind.is_file() {
            files.push(rel);
        } else {
            return Err(format!("the archive holds {}, not a file", rel.display()));
        }
    }
    Ok(())
}

/// A file put in place, and where the one it replaced was moved.
type Swapped = (PathBuf, Option<PathBuf>);

/// Moves each of `files` from `root` into `dir`; the file it replaces goes to
/// `<file>.<suffix>`. On a failure the files already moved are put back.
fn replace_files(
    dir: &Path,
    root: &Path,
    files: &[PathBuf],
    suffix: &str,
) -> Result<Vec<Swapped>, String> {
    let mut done: Vec<Swapped> = Vec::new();
    for file in files {
        let target = dir.join(file);
        let step = (|| {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            let old = if target.exists() {
                let mut name = target.file_name().unwrap_or_default().to_os_string();
                name.push(format!(".{suffix}"));
                let old = target.with_file_name(name);
                fs::rename(&target, &old).map_err(|error| error.to_string())?;
                Some(old)
            } else {
                None
            };
            if let Err(error) = fs::rename(root.join(file), &target) {
                if let Some(old) = &old {
                    let _ = fs::rename(old, &target);
                }
                return Err(error.to_string());
            }
            Ok(old)
        })();
        match step {
            Ok(old) => done.push((target, old)),
            Err(error) => {
                restore_files(&done);
                return Err(format!("could not replace {}: {error}", target.display()));
            }
        }
    }
    Ok(done)
}

/// Undoes [`replace_files`], last file first.
fn restore_files(done: &[Swapped]) {
    for (target, old) in done.iter().rev() {
        let _ = fs::remove_file(target);
        if let Some(old) = old {
            let _ = fs::rename(old, target);
        }
    }
}

/// Removes what an earlier update left beside the app: the old bundle it replaced, a copy it
/// did not finish. Only Kerno's own names, only next to this app. Called at start by `main.rs`.
pub fn remove_leftovers(target: &Install) {
    let Install::App(bundle) = target else {
        return;
    };
    let Some(Ok(entries)) = bundle.parent().map(fs::read_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let ours = ["Kerno.app.old-", "Kerno.app.new-"].iter().any(|prefix| {
            name.strip_prefix(prefix)
                .is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
        });
        if ours {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Where an update stands, for the status bar item.
#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Idle,
    /// A newer version is out.
    Available(Version),
    Downloading(Version),
    /// Downloaded and checked: kept for the next click when the quit questions were declined.
    Ready(Version, PathBuf),
    Installing(Version),
    /// Why the last attempt failed; the release page is offered.
    Failed(Version, String),
}

impl Phase {
    /// The version the item is about.
    pub fn version(&self) -> Option<Version> {
        match self {
            Phase::Idle => None,
            Phase::Available(version)
            | Phase::Downloading(version)
            | Phase::Ready(version, _)
            | Phase::Installing(version)
            | Phase::Failed(version, _) => Some(*version),
        }
    }
}

/// The update state of the app. Installed only by `main.rs` once the off switches allow it, and
/// by the update tests with a fake runner: without it, Kerno never checks.
pub struct Updates {
    pub runner: Arc<dyn Runner>,
    /// This Kerno's version (a test may pretend an older one).
    pub current: String,
    pub target: Install,
    /// Where downloads go: the system's temporary folder.
    pub temp: PathBuf,
    /// Kerno's data folder, where an update on Windows leaves its note ([`after_install`]).
    pub data_dir: Option<PathBuf>,
    /// Windows, a copy that is not the installed one (the zip's): the version and `kerno.exe` of
    /// the Kerno installed in the profile, which an update of that version or older starts
    /// instead of installing it again (K70, V5).
    pub installed: Option<(Version, PathBuf)>,
    pub phase: Phase,
    /// A check is under way.
    pub checking: bool,
    /// The newest version the update dialog was shown for in this run (D-097, D-123): once per
    /// version, and never for one older than it.
    pub offered: Option<Version>,
    /// The version whose notes were asked for in this run (K43): once per version.
    pub notes_asked: Option<Version>,
    /// What a newer version brings, once its notes were read.
    pub notes: Option<(Version, releases::WhatsNew)>,
    /// The wall clock; a test sets its own.
    pub clock: Arc<dyn Fn() -> SystemTime + Send + Sync>,
    /// When a check was last sent in this run, by the wall clock (K57).
    pub last_check: Option<SystemTime>,
    /// The wall clock and the executor's clock at the last look of [`Updates::due`]: their
    /// gaps differ by the time the computer slept.
    looked: Option<(SystemTime, Instant)>,
}

impl Global for Updates {}

impl Updates {
    pub fn new(runner: Arc<dyn Runner>, current: &str, target: Install, temp: PathBuf) -> Self {
        Self {
            runner,
            current: current.to_string(),
            target,
            temp,
            data_dir: None,
            installed: None,
            phase: Phase::Idle,
            checking: false,
            offered: None,
            notes_asked: None,
            notes: None,
            clock: Arc::new(SystemTime::now),
            last_check: None,
            looked: None,
        }
    }

    /// Whether a timed check is due now (K57, D-123): none was sent yet, or `EVERY` of wall
    /// time has passed since the last one, the computer awake or asleep in between. Not at the
    /// first look after a sleep, though: the network may not be back yet, and the next look, a
    /// `TICK` later, checks. `timer` is the executor's clock, which stops during a sleep.
    pub fn due(&mut self, timer: Instant) -> bool {
        let now = (self.clock)();
        let woke = self.looked.is_some_and(|(wall, at)| {
            now.duration_since(wall).unwrap_or_default()
                > timer.saturating_duration_since(at) + TICK
        });
        self.looked = Some((now, timer));
        // The clock was set back: the interval starts again from now.
        if self.last_check.is_some_and(|last| last > now) {
            self.last_check = Some(now);
        }
        !woke
            && self.last_check.is_none_or(|last| {
                now.duration_since(last)
                    .is_ok_and(|elapsed| elapsed >= EVERY)
            })
    }
}

/// What `version` brings, read from its notes (K43); `None` on any failure: the dialog then
/// stays as it was.
pub fn read_notes(
    runner: &dyn Runner,
    current: &str,
    version: Version,
) -> Option<releases::WhatsNew> {
    let notes = runner
        .output(&curl().ok()?, &releases::notes_args(current, version))
        .ok()?;
    releases::whats_new(&notes)
}

/// Whether the environment turns the check off for this run: `KERNO_NO_UPDATE_CHECK`, or a
/// bench run (`KERNO_BENCH`). `var` tells whether a variable is set.
fn switched_off(var: impl Fn(&str) -> bool) -> bool {
    !releases::checks_allowed(var("KERNO_NO_UPDATE_CHECK"), var("KERNO_BENCH"))
}

/// What `kerno --version` prints. A test build says so, and `scripts/bundle-windows.sh` refuses to
/// package one (K70, V9).
#[cfg(not(feature = "test-update-without-click"))]
pub const VERSION_LINE: &str = concat!("kerno ", env!("CARGO_PKG_VERSION"));
#[cfg(feature = "test-update-without-click")]
pub const VERSION_LINE: &str = concat!(
    "kerno ",
    env!("CARGO_PKG_VERSION"),
    "+test-update-without-click"
);

/// This Kerno's version. A build with the `test-update-without-click` feature (the Windows
/// runner's update check only, never a release) says 0.0.0, so that the release it is given is
/// newer.
#[cfg(not(feature = "test-update-without-click"))]
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
#[cfg(feature = "test-update-without-click")]
pub const CURRENT: &str = "0.0.0";

/// The real updater, or `None` when an off switch is set (`KERNO_NO_UPDATE_CHECK`, a bench run)
/// or in a test build. `var` tells whether an environment variable is set. On Windows a Kerno
/// running as administrator never runs a downloaded installer: it opens the release page.
pub fn for_this_run(
    var: impl Fn(&str) -> bool,
    exe: Option<PathBuf>,
    data_dir: PathBuf,
) -> Option<Updates> {
    if cfg!(test) || switched_off(var) {
        return None;
    }
    let mut target = exe.map_or(Install::Page, |exe| {
        install_target(&exe, std::env::consts::OS)
    });
    let downloads = downloads_dir();
    if matches!(target, Install::Setup(_)) && (downloads.is_none() || curl().is_err()) {
        tracing::warn!(
            "no local application data folder, or Windows not in C:\\Windows: an update opens \
             the release page"
        );
        target = Install::Page;
    }
    // GitHub's Windows runner gives only administrators: the feature's build, there to run the
    // installer on that runner, leaves this check out (D-139).
    #[cfg(not(feature = "test-update-without-click"))]
    if matches!(target, Install::Setup(_))
        && elevated(&System, kerno_core::tools::windows_program("whoami.exe"))
    {
        tracing::info!("Kerno runs as administrator: an update opens the release page");
        target = Install::Page;
    }
    let installed = match &target {
        Install::Setup(exe) => {
            kerno_core::storage::AppPaths::user_programs_dir().and_then(|programs| {
                let reg = kerno_core::tools::windows_program("reg.exe");
                installed_elsewhere(&System, exe, &programs, reg)
            })
        }
        _ => None,
    };
    let downloads = downloads.unwrap_or_else(std::env::temp_dir);
    let mut updates = Updates::new(Arc::new(System), CURRENT, target, downloads);
    updates.data_dir = Some(data_dir);
    updates.installed = installed;
    Some(updates)
}

/// Inno Setup's uninstall entry of Kerno (`packaging/windows/kerno.iss`'s `AppId`).
const UNINSTALL_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\{A1106071-CFCF-4417-8D07-D209FC6B030D}_is1";

/// Windows: the Kerno installed in the profile (`<programs>\Kerno\kerno.exe`, the installer's
/// fixed folder) and its version from the uninstall entry, when `exe` is another copy (the zip's).
/// `reg.exe` from `C:\Windows` only (V3); any doubt is `None`, and Update installs as before.
fn installed_elsewhere(
    runner: &dyn Runner,
    exe: &Path,
    programs: &Path,
    reg: Option<PathBuf>,
) -> Option<(Version, PathBuf)> {
    let installed = programs.join("Kerno").join("kerno.exe");
    let same = |a: &Path, b: &Path| {
        a.to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy())
    };
    if same(exe, &installed) || !installed.is_file() {
        return None;
    }
    let reg = reg?;
    let args = ["query", UNINSTALL_KEY, "/v", "DisplayVersion"].map(OsString::from);
    let out = runner.output(&reg, &args).ok()?;
    Some((display_version(&String::from_utf8_lossy(&out))?, installed))
}

/// The version of `reg query … /v DisplayVersion`'s answer (`    DisplayVersion    REG_SZ    X.Y.Z`).
fn display_version(reg: &str) -> Option<Version> {
    reg.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        (words.next()? == "DisplayVersion" && words.next()? == "REG_SZ")
            .then(|| Version::parse(words.next()?))
            .flatten()
    })
}

#[cfg(test)]
pub mod tests {
    use std::sync::Mutex;

    use super::*;

    /// What the fake answers a program run with these arguments.
    type Answer = Box<dyn Fn(&Path, &[String]) -> Result<Vec<u8>, String> + Send + Sync>;

    /// A runner that runs nothing: it records each call and answers from `answer`.
    pub struct Fake {
        pub calls: Mutex<Vec<(PathBuf, Vec<String>)>>,
        answer: Answer,
    }

    impl Fake {
        pub fn new(
            answer: impl Fn(&Path, &[String]) -> Result<Vec<u8>, String> + Send + Sync + 'static,
        ) -> Arc<Fake> {
            Arc::new(Fake {
                calls: Mutex::new(Vec::new()),
                answer: Box::new(answer),
            })
        }

        pub fn calls(&self) -> Vec<(PathBuf, Vec<String>)> {
            self.calls.lock().unwrap().clone()
        }

        fn call(&self, program: &Path, args: &[OsString]) -> Result<Vec<u8>, String> {
            let args: Vec<String> = args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            self.calls
                .lock()
                .unwrap()
                .push((program.to_path_buf(), args.clone()));
            (self.answer)(program, &args)
        }
    }

    impl Runner for Fake {
        fn output(&self, program: &Path, args: &[OsString]) -> Result<Vec<u8>, String> {
            self.call(program, args)
        }

        fn start(&self, program: &Path, args: &[OsString]) -> Result<(), String> {
            self.call(program, args).map(drop)
        }

        /// Records each variable after the arguments: `NAME=value`, or `-NAME` when removed.
        fn start_with_env(
            &self,
            program: &Path,
            args: &[OsString],
            env: &[(String, Option<OsString>)],
        ) -> Result<(), String> {
            let mut args = args.to_vec();
            for (name, value) in env {
                args.push(match value {
                    Some(value) => {
                        let mut arg = OsString::from(format!("{name}="));
                        arg.push(value);
                        arg
                    }
                    None => format!("-{name}").into(),
                });
            }
            self.call(program, &args).map(drop)
        }
    }

    /// `curl --head`'s answer naming `tag` as the latest release.
    pub fn redirect_to(tag: &str) -> Vec<u8> {
        format!(
            "HTTP/2 302\r\nlocation: https://github.com/EvanPluchart/kerno/releases/tag/{tag}\r\n\r\n"
        )
        .into_bytes()
    }

    /// Whether `program` is `curl`: `/usr/bin/curl`, or `curl.exe` on Windows.
    pub fn is_curl(program: &Path) -> bool {
        program.file_stem().is_some_and(|stem| stem == "curl")
    }

    /// The value after `flag` in `args`.
    pub fn value_of<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
        let at = args.iter().position(|arg| arg == flag)?;
        args.get(at + 1).map(String::as_str)
    }

    /// A curl fake serving `files` (by the last part of the URL) for downloads.
    pub fn serving(files: Vec<(String, Vec<u8>)>) -> Arc<Fake> {
        Fake::new(move |_, args| {
            let output = value_of(args, "--output").ok_or("no output")?;
            let url = args.last().ok_or("no url")?;
            let name = url.rsplit('/').next().unwrap_or_default();
            let (_, body) = files
                .iter()
                .find(|(file, _)| file == name)
                .ok_or("curl: (22) The requested URL returned error: 404")?;
            fs::write(output, body).map_err(|error| error.to_string())?;
            Ok(Vec::new())
        })
    }

    fn hex(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn this_artifact(version: &str) -> String {
        releases::artifact(v(version), std::env::consts::OS, std::env::consts::ARCH).unwrap()
    }

    #[test]
    fn the_check_asks_once_and_reads_the_redirect() {
        let fake = Fake::new(|_, _| Ok(redirect_to("v0.2.0")));
        assert_eq!(check(&*fake, "0.1.0"), Ok(Some(v("0.2.0"))));
        assert_eq!(check(&*fake, "0.2.0"), Ok(None));
        assert_eq!(check(&*fake, "0.3.0"), Ok(None));
        let calls = fake.calls();
        assert_eq!(calls.len(), 3);
        let (program, args) = &calls[0];
        assert!(program.is_absolute() && is_curl(program), "{program:?}");
        let expected: Vec<String> = releases::check_args("0.1.0")
            .into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect();
        assert_eq!(args, &expected);

        // Offline, no release (404), a malformed answer: no version.
        for answer in [
            Err("curl: (6) Could not resolve host".to_string()),
            Ok(b"HTTP/2 404\r\n\r\n".to_vec()),
            Ok(redirect_to("v0.2")),
        ] {
            let fake = Fake::new(move |_, _| answer.clone());
            assert!(check(&*fake, "0.1.0").is_err());
        }
    }

    #[test]
    fn a_download_is_kept_only_when_its_checksum_matches() {
        let temp = tempfile::tempdir().unwrap();
        let name = this_artifact("0.2.0");
        let body = b"the release".to_vec();

        let good = format!("{}  {name}\n", hex(&body));
        let fake = serving(vec![
            ("SHA256SUMS".into(), good.into_bytes()),
            (name.clone(), body.clone()),
        ]);
        let path = download(&*fake, "0.1.0", v("0.2.0"), temp.path()).unwrap();
        assert_eq!(path.file_name().unwrap().to_string_lossy(), name);
        assert_eq!(fs::read(&path).unwrap(), body);
        let folder = path.parent().unwrap();
        assert_eq!(folder.parent(), Some(temp.path()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(folder).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        // The two requests: the checksums first, then the file; Kerno's names for both.
        let calls = fake.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0].1.last().unwrap(),
            "https://github.com/EvanPluchart/kerno/releases/download/v0.2.0/SHA256SUMS"
        );
        assert_eq!(
            value_of(&calls[1].1, "--output").map(PathBuf::from),
            Some(path.clone())
        );
        fs::remove_dir_all(folder).unwrap();

        // A mismatch, a missing line, a missing SHA256SUMS, an empty file: refused, deleted.
        let wrong = format!("{}  {name}\n", hex(b"something else"));
        let other = format!("{}  kerno-9.9.9.tar.gz\n", hex(&body));
        let empty_sum = format!("{}  {name}\n", hex(b""));
        for (sums, file) in [
            (Some(wrong), body.clone()),
            (Some(other), body.clone()),
            (None, body.clone()),
            (Some(empty_sum), Vec::new()),
        ] {
            let mut files = vec![(name.clone(), file)];
            files.extend(sums.map(|sums| ("SHA256SUMS".to_string(), sums.into_bytes())));
            let fake = serving(files);
            let refused = download(&*fake, "0.1.0", v("0.2.0"), temp.path());
            assert!(refused.is_err(), "{refused:?}");
            assert_eq!(
                fs::read_dir(temp.path()).unwrap().count(),
                0,
                "nothing is kept"
            );
        }
    }

    /// A fake `Kerno.app` holding `marker`.
    fn fake_bundle(at: &Path, marker: &str) {
        fs::create_dir_all(at.join("Contents/MacOS")).unwrap();
        fs::write(at.join("Contents/MacOS/kerno"), marker).unwrap();
    }

    // The macOS bundle swap is tested on macOS only: `install_target` makes an `Install::App`
    // nowhere else, and `hdiutil`, `ditto` and `open` exist only there.
    #[cfg(target_os = "macos")]
    fn marker(bundle: &Path) -> String {
        fs::read_to_string(bundle.join("Contents/MacOS/kerno")).unwrap()
    }

    /// A runner for the macOS install: `hdiutil attach` puts a new `Kerno.app` in the mount
    /// point, `ditto` copies it, `open` fails when `relaunch_fails`.
    #[cfg(target_os = "macos")]
    fn mac_runner(relaunch_fails: bool) -> Arc<Fake> {
        Fake::new(move |program, args| {
            match program.to_str().unwrap() {
                "/usr/bin/hdiutil" if args[0] == "attach" => {
                    let mount = value_of(args, "-mountpoint").unwrap();
                    fake_bundle(&Path::new(mount).join("Kerno.app"), "new");
                }
                // Detaching empties the mount point.
                "/usr/bin/hdiutil" => {
                    fs::remove_dir_all(Path::new(&args[1]).join("Kerno.app")).unwrap();
                }
                "/usr/bin/ditto" => {
                    let (from, to) = (Path::new(&args[0]), Path::new(&args[1]));
                    fake_bundle(to, &marker(from));
                }
                "/usr/bin/open" if relaunch_fails => return Err("open failed".into()),
                "/usr/bin/open" => {}
                other => return Err(format!("unexpected {other}")),
            }
            Ok(Vec::new())
        })
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn the_app_bundle_is_swapped_by_rename_and_put_back_on_a_failure() {
        let apps = tempfile::tempdir().unwrap();
        let bundle = apps.path().join("Kerno.app");
        fake_bundle(&bundle, "old");
        let download = tempfile::tempdir().unwrap();
        let dmg = download.path().join("Kerno-0.2.0-macos-arm64.dmg");
        fs::write(&dmg, "disk image").unwrap();
        let target = Install::App(bundle.clone());

        // The relaunch fails: the old bundle is back, nothing else is left.
        let fake = mac_runner(true);
        assert!(install(&*fake, &target, &dmg, v("0.2.0"), None).is_err());
        assert_eq!(marker(&bundle), "old");
        let names = |dir: &Path| -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(dir)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        };
        assert_eq!(names(apps.path()), ["Kerno.app"]);

        let fake = mac_runner(false);
        install(&*fake, &target, &dmg, v("0.2.0"), None).unwrap();
        assert_eq!(marker(&bundle), "new");
        let pid = std::process::id();
        assert_eq!(
            names(apps.path()),
            ["Kerno.app".to_string(), format!("Kerno.app.old-{pid}")]
        );
        // The mount point: a new private folder beside the download, gone once detached.
        let mount = value_of(&fake.calls()[0].1, "-mountpoint")
            .unwrap()
            .to_string();
        assert_eq!(Path::new(&mount).parent(), Some(download.path()));
        assert!(!Path::new(&mount).exists());
        let calls: Vec<(String, Vec<String>)> = fake
            .calls()
            .into_iter()
            .map(|(program, args)| (program.display().to_string(), args))
            .collect();
        let staged = apps.path().join(format!("Kerno.app.new-{pid}"));
        assert_eq!(
            calls,
            [
                (
                    "/usr/bin/hdiutil".to_string(),
                    vec![
                        "attach".to_string(),
                        "-nobrowse".into(),
                        "-readonly".into(),
                        "-noautoopen".into(),
                        "-mountpoint".into(),
                        mount.clone(),
                        dmg.display().to_string(),
                    ]
                ),
                (
                    "/usr/bin/ditto".to_string(),
                    vec![format!("{mount}/Kerno.app"), staged.display().to_string()]
                ),
                (
                    "/usr/bin/hdiutil".to_string(),
                    vec!["detach".to_string(), mount, "-quiet".into()]
                ),
                (
                    "/usr/bin/open".to_string(),
                    vec!["-n".to_string(), bundle.display().to_string()]
                ),
            ]
        );

        // The next start removes the old bundle, and only Kerno's own names.
        fs::create_dir(apps.path().join("Kerno.app.old-notapid")).unwrap();
        fs::create_dir(apps.path().join("Other.app")).unwrap();
        remove_leftovers(&target);
        assert_eq!(
            names(apps.path()),
            ["Kerno.app", "Kerno.app.old-notapid", "Other.app"]
        );
    }

    /// A runner for the Linux install: `tar -t` lists `entries` (verbose: `kinds`), `tar -x`
    /// writes the new files, the relaunch fails when `relaunch_fails`.
    fn linux_runner(top: String, kinds: &'static str, relaunch_fails: bool) -> Arc<Fake> {
        Fake::new(move |program, args| {
            if program.ends_with("tar") {
                let names = format!(
                    "{top}/\n{top}/bin/\n{top}/bin/kerno\n{top}/share/doc/kerno/README.md\n"
                );
                return match args[0].as_str() {
                    "-t" if args[1] == "-v" => Ok(kinds.as_bytes().to_vec()),
                    "-t" => Ok(names.into_bytes()),
                    "-x" => {
                        let root = Path::new(value_of(args, "-C").unwrap()).join(&top);
                        fs::create_dir_all(root.join("bin")).unwrap();
                        fs::create_dir_all(root.join("share/doc/kerno")).unwrap();
                        fs::write(root.join("bin/kerno"), "new").unwrap();
                        fs::write(root.join("share/doc/kerno/README.md"), "new").unwrap();
                        Ok(Vec::new())
                    }
                    _ => Err("unexpected tar".into()),
                };
            }
            if relaunch_fails {
                Err("could not start".into())
            } else {
                Ok(Vec::new())
            }
        })
    }

    #[test]
    fn the_files_of_the_tarball_are_replaced_by_rename_and_put_back_on_a_failure() {
        let prefix = tempfile::tempdir().unwrap();
        let dir = prefix.path();
        fs::create_dir_all(dir.join("bin")).unwrap();
        fs::create_dir_all(dir.join("share/applications")).unwrap();
        fs::write(dir.join("bin/kerno"), "old").unwrap();
        fs::write(dir.join("share/applications/kerno.desktop"), "entry").unwrap();
        // Someone else's file in the same prefix (it may be ~/.local): never touched.
        fs::write(dir.join("bin/other-tool"), "theirs").unwrap();
        let target = install_target(&dir.join("bin/kerno"), "linux");
        assert_eq!(target, Install::Prefix(dir.to_path_buf()));
        let tarball = dir.join("download.tar.gz");
        fs::write(&tarball, "archive").unwrap();
        let top = releases::tarball_folder(v("0.2.0"), std::env::consts::ARCH);
        let kinds = "drwx x\ndrwx x\n-rwx x\n-rw- x\n";

        let failing = linux_runner(top.clone(), kinds, true);
        assert!(install(&*failing, &target, &tarball, v("0.2.0"), None).is_err());
        assert_eq!(fs::read_to_string(dir.join("bin/kerno")).unwrap(), "old");
        assert!(!dir.join("share/doc/kerno/README.md").exists());

        // A link in the archive: refused before anything is extracted.
        let linked = linux_runner(
            top.clone(),
            "drwx x\ndrwx x\nlrwx x -> /etc\n-rw- x\n",
            false,
        );
        assert!(install(&*linked, &target, &tarball, v("0.2.0"), None).is_err());
        assert!(linked.calls().iter().all(|(_, args)| args[0] != "-x"));

        let fake = linux_runner(top, kinds, false);
        install(&*fake, &target, &tarball, v("0.2.0"), None).unwrap();
        assert_eq!(fs::read_to_string(dir.join("bin/kerno")).unwrap(), "new");
        assert_eq!(
            fs::read_to_string(dir.join("share/doc/kerno/README.md")).unwrap(),
            "new"
        );
        assert_eq!(
            fs::read_to_string(dir.join("bin/other-tool")).unwrap(),
            "theirs"
        );
        let mut left: Vec<String> = fs::read_dir(dir.join("bin"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["kerno", "other-tool"]);
        // The private extraction folder is gone.
        assert!(fs::read_dir(dir).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".kerno")
        }));
        let (program, args) = fake.calls().last().unwrap().clone();
        assert_eq!(
            (program, args),
            (dir.join("bin/kerno"), Vec::<String>::new())
        );
    }

    #[test]
    fn only_a_writable_bundle_or_tarball_layout_installs() {
        let root = tempfile::tempdir().unwrap();
        let bundle = root.path().join("Kerno.app");
        fake_bundle(&bundle, "old");
        let exe = bundle.join("Contents/MacOS/kerno");
        assert_eq!(install_target(&exe, "macos"), Install::App(bundle.clone()));
        // The bare binary, another name, a translocated copy, another system: the page.
        let other = root.path().join("Other.app/Contents/MacOS/kerno");
        let translocated =
            Path::new("/private/var/folders/x/AppTranslocation/1/d/Kerno.app/Contents/MacOS/kerno");
        for (exe, os) in [
            (Path::new("/Users/x/kerno/target/release/kerno"), "macos"),
            (other.as_path(), "macos"),
            (translocated, "macos"),
            (exe.as_path(), "windows"),
            // A package manager's install: /usr/bin is not Kerno's to write.
            (Path::new("/usr/bin/kerno"), "linux"),
            (Path::new("/nonexistent/bin/kerno"), "linux"),
        ] {
            assert_eq!(install_target(exe, os), Install::Page, "{exe:?} {os}");
        }
        // A prefix without the tarball's desktop entry is not the tarball's.
        let prefix = root.path().join("prefix");
        fs::create_dir_all(prefix.join("bin")).unwrap();
        assert_eq!(
            install_target(&prefix.join("bin/kerno"), "linux"),
            Install::Page
        );
    }

    /// K57 (D-123): a timed check is due after `EVERY` of wall time, the computer asleep or not,
    /// never at the first look after a sleep, and a clock set back starts the interval again.
    #[test]
    fn a_timed_check_is_due_after_six_hours_of_wall_time_and_not_right_at_a_wake() {
        let wall = Arc::new(Mutex::new(
            SystemTime::UNIX_EPOCH + Duration::from_secs(1 << 30),
        ));
        let mut updates = Updates::new(
            Fake::new(|_, _| Ok(Vec::new())),
            "0.1.0",
            Install::Page,
            "/".into(),
        );
        let clock = wall.clone();
        updates.clock = Arc::new(move || *clock.lock().unwrap());
        let mut timer = Instant::now();
        // `by` of wall time, of which `awake` counted by the timer.
        let mut pass = |updates: &mut Updates, by: Duration, awake: Duration| {
            *wall.lock().unwrap() += by;
            timer += awake;
            updates.due(timer)
        };
        // None sent yet: due.
        assert!(pass(&mut updates, Duration::ZERO, Duration::ZERO));
        updates.last_check = Some((updates.clock)());
        // Awake: due at six hours, not a minute before.
        for _ in 0..359 {
            assert!(!pass(&mut updates, TICK, TICK));
        }
        assert!(pass(&mut updates, TICK, TICK));
        // Asleep for six hours: not at the first look, at the next one.
        updates.last_check = Some((updates.clock)());
        assert!(!pass(&mut updates, EVERY, TICK));
        assert!(pass(&mut updates, TICK, TICK));
        // Asleep for five hours: not due, whatever the looks.
        updates.last_check = Some((updates.clock)());
        assert!(!pass(&mut updates, Duration::from_secs(5 * 3600), TICK));
        assert!(!pass(&mut updates, TICK, TICK));
        assert!(!pass(&mut updates, TICK, Duration::ZERO));
        // The clock set back a day: the interval starts again from there.
        *wall.lock().unwrap() -= Duration::from_secs(24 * 3600);
        assert!(!pass(&mut updates, Duration::ZERO, Duration::ZERO));
        assert_eq!(updates.last_check, Some((updates.clock)()));
        assert!(!pass(&mut updates, EVERY - TICK, EVERY - TICK));
        assert!(pass(&mut updates, TICK, TICK));
    }

    /// K67: on Windows the check runs `System32\curl.exe` by its absolute path, whatever
    /// `SystemRoot` says: never a bare name, which Windows would look for in the current folder
    /// and on `PATH`. K70's `whoami.exe` is placed by the same rule.
    #[test]
    fn windows_curl_is_always_system32_s_by_an_absolute_path() {
        use kerno_core::tools::system_program;
        for root in [r"C:\Windows", r"C:\WINDOWS\", r"c:\windows/"] {
            assert_eq!(
                system_program(Some(root), "curl.exe"),
                Some(PathBuf::from(r"C:\Windows\System32\curl.exe")),
                "{root:?}"
            );
        }
        assert_eq!(
            system_program(Some(r"C:\Windows"), r"OpenSSH\ssh.exe"),
            Some(PathBuf::from(r"C:\Windows\System32\OpenSSH\ssh.exe"))
        );
        // K70 (V3): a folder named by the environment is never trusted, a drive's absolute path
        // included: another drive, a relative or UNC path, empty, unset.
        for root in [
            Some(r"D:\Windows"),
            Some(r"D:\WINNT\"),
            Some(r"C:\Users\x\Windows"),
            Some(r"C:\Windows\.."),
            Some("Windows"),
            Some(r"..\x"),
            Some(r"\\host\share"),
            Some(r"\\?\C:\Windows"),
            Some("C:"),
            Some(""),
            None,
        ] {
            assert_eq!(system_program(root, "whoami.exe"), None, "{root:?}");
        }
    }

    #[test]
    fn the_environment_switches_and_a_test_build_make_no_updater() {
        assert!(!switched_off(|_| false));
        assert!(switched_off(|name| name == "KERNO_NO_UPDATE_CHECK"));
        assert!(switched_off(|name| name == "KERNO_BENCH"));
        assert!(!switched_off(|name| name == "KERNO_NO_UPDATE"));
        // Whatever the environment says, a test build never makes the real one.
        assert!(for_this_run(|_| false, None, "/".into()).is_none());
    }

    #[test]
    fn on_windows_the_zip_and_the_installed_folder_update_through_the_installer() {
        let folder = tempfile::tempdir().unwrap();
        let exe = folder.path().join("kerno.exe");
        fs::write(&exe, "exe").unwrap();
        // A bare build: no licences beside it.
        assert_eq!(install_target(&exe, "windows"), Install::Page);
        fs::write(folder.path().join("LICENSE-MIT"), "MIT").unwrap();
        assert_eq!(install_target(&exe, "windows"), Install::Setup(exe.clone()));
        let other = folder.path().join("other.exe");
        assert_eq!(install_target(&other, "windows"), Install::Page);
    }

    /// `whoami /groups /fo csv /nh` of GitHub's `windows-2025` runner (an administrator, High),
    /// captured by the K70 probe.
    const RUNNER_GROUPS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/windows/whoami-groups-runner.csv"
    ));

    /// The same groups with another integrity row, as the documented CSV form writes it: built,
    /// not captured (no Windows at hand runs at Medium); `label` is the row's translated name.
    fn groups(label: &str, sid: &str) -> Vec<u8> {
        let mut out: String = RUNNER_GROUPS
            .lines()
            .filter(|row| !row.contains("\"S-1-16-"))
            .map(|row| format!("{row}\r\n"))
            .collect();
        out.push_str(&format!("\"{label}\",\"Label\",\"{sid}\",\"\"\r\n"));
        out.into_bytes()
    }

    #[test]
    fn only_a_process_at_medium_integrity_runs_the_installer() {
        // The runner, as captured: High.
        assert!(!standard_user(Some(RUNNER_GROUPS.as_bytes())));
        let medium = groups("Mandatory Label\\Medium Mandatory Level", "S-1-16-8192");
        assert!(standard_user(Some(&medium)));
        // Read by the SID: a French Windows translates the names, never the SIDs.
        let medium_fr = groups(
            "Étiquette obligatoire\\Niveau obligatoire moyen",
            "S-1-16-8192",
        );
        assert!(standard_user(Some(&medium_fr)));
        let high_fr = groups(
            "Étiquette obligatoire\\Niveau obligatoire élevé",
            "S-1-16-12288",
        );
        assert!(!standard_user(Some(&high_fr)));
        // A name saying Medium with another SID is that SID.
        let lying = groups("Mandatory Label\\Medium Mandatory Level", "S-1-16-12288");
        assert!(!standard_user(Some(&lying)));
        for (sid, what) in [
            ("S-1-16-12288", "high"),
            ("S-1-16-16384", "system"),
            (
                "S-1-16-4096",
                "low: a sandbox the installer could not write from",
            ),
            ("S-1-16-81920", "not a level Kerno knows"),
            ("S-1-16-819", "not a level Kerno knows"),
        ] {
            assert!(!standard_user(Some(&groups("x", sid))), "{what}");
        }
        // Kerno cannot tell: the release page.
        let none = groups("x", "S-1-5-11");
        assert!(!standard_user(Some(&none)), "no integrity row");
        let mut two = medium.clone();
        two.extend_from_slice(b"\"y\",\"Label\",\"S-1-16-12288\",\"\"\r\n");
        assert!(!standard_user(Some(&two)), "two integrity rows");
        assert!(!standard_user(Some(b"")), "an empty answer");
        assert!(!standard_user(Some(b"S-1-16-8192")), "not the CSV form");
        assert!(!standard_user(None), "whoami missing or failed");

        // The program and its arguments: Windows' own whoami, by an absolute path.
        let whoami = || Some(PathBuf::from(r"C:\Windows\System32\whoami.exe"));
        let fake = Fake::new(move |_, _| Ok(medium.clone()));
        assert!(!elevated(&*fake, whoami()));
        let (program, args) = fake.calls()[0].clone();
        assert_eq!(Some(program), whoami());
        assert_eq!(args, ["/groups", "/fo", "csv", "/nh"]);
        assert!(elevated(
            &*Fake::new(|_, _| Err("no whoami".into())),
            whoami()
        ));
        // No `whoami.exe` of `C:\Windows` (V3): elevated, and nothing run.
        let fake = Fake::new(|_, _| Ok(Vec::new()));
        assert!(elevated(&*fake, None));
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn the_installer_is_started_after_a_note_and_the_note_says_what_happened() {
        let data = tempfile::tempdir().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let download = temp.path().join("kerno-update-1-2");
        fs::create_dir(&download).unwrap();
        let setup = setup_in(&download);
        let exe = PathBuf::from(r"C:\Users\x\AppData\Local\Programs\Kerno\kerno.exe");
        let target = Install::Setup(exe.clone());

        // The installer does not start: no note is left, the download is the caller's.
        let failing = Fake::new(|_, _| Err("could not start".into()));
        assert!(install(&*failing, &target, &setup, v("0.2.0"), Some(data.path())).is_err());
        assert!(!data.path().join(PENDING).exists());

        let fake = Fake::new(|_, _| Ok(Vec::new()));
        install(&*fake, &target, &setup, v("0.2.0"), Some(data.path())).unwrap();
        let calls = fake.calls();
        assert_eq!(calls.len(), 1);
        let (program, args) = calls[0].clone();
        assert_eq!(program, setup);
        // The user's own temporary folders, as this process has them, go to the installer to
        // be put back (W1).
        let own = |name: &str| match std::env::var_os(name).filter(|value| !value.is_empty()) {
            Some(value) => format!("KERNO_SETUP_{name}={}", value.to_string_lossy()),
            None => format!("-KERNO_SETUP_{name}"),
        };
        assert_eq!(
            args,
            [
                "/VERYSILENT".to_string(),
                "/SUPPRESSMSGBOXES".into(),
                "/NORESTART".into(),
                "/NOCLOSEAPPLICATIONS".into(),
                "/relaunch=1".into(),
                format!("/pid={}", std::process::id()),
                format!("/kerno={}", exe.display()),
                format!("/LOG={}", data.path().join(SETUP_LOG).display()),
                // V1: the installer unpacks its own code into the download's folder.
                format!("TEMP={}", download.display()),
                own("TEMP"),
                format!("TMP={}", download.display()),
                own("TMP"),
                "KERNO_SETUP_ENV=1".into(),
            ]
        );
        assert!(
            setup.is_file(),
            "the installer reads its files from the download"
        );
        assert_eq!(
            fs::read_to_string(data.path().join(PENDING)).unwrap(),
            format!("0.2.0\n{}\n", download.display())
        );

        // The next start: still the old version, so it is said, once.
        let (version, why) = after_install(data.path(), "0.1.0", temp.path()).unwrap();
        assert_eq!(version, v("0.2.0"));
        assert!(why.contains("still Kerno 0.1.0"), "{why}");
        assert!(why.contains(SETUP_LOG), "{why}");
        assert!(!data.path().join(PENDING).exists());
        assert!(!download.exists(), "the download goes with the note");
        assert_eq!(after_install(data.path(), "0.1.0", temp.path()), None);

        // Installed: nothing to say; the download goes all the same.
        fs::create_dir(&download).unwrap();
        install(
            &*fake,
            &target,
            &setup_in(&download),
            v("0.2.0"),
            Some(data.path()),
        )
        .unwrap();
        assert_eq!(after_install(data.path(), "0.2.0", temp.path()), None);
        assert!(!download.exists());
        assert!(!data.path().join(PENDING).exists());

        // A note naming another folder never removes it.
        let theirs = temp.path().join("not-kerno");
        fs::create_dir(&theirs).unwrap();
        fs::write(
            data.path().join(PENDING),
            format!("0.2.0\n{}\n", theirs.display()),
        )
        .unwrap();
        assert!(after_install(data.path(), "0.1.0", temp.path()).is_some());
        assert!(theirs.is_dir());
        let elsewhere = tempfile::tempdir().unwrap();
        let outside = elsewhere.path().join("kerno-update-9-9");
        fs::create_dir(&outside).unwrap();
        fs::write(
            data.path().join(PENDING),
            format!("0.2.0\n{}\n", outside.display()),
        )
        .unwrap();
        assert_eq!(after_install(data.path(), "0.3.0", temp.path()), None);
        assert!(outside.is_dir());
        // A note that is not Kerno's form: removed, nothing said.
        fs::write(data.path().join(PENDING), "garbage").unwrap();
        assert_eq!(after_install(data.path(), "0.1.0", temp.path()), None);
        assert!(!data.path().join(PENDING).exists());
    }

    /// A downloaded installer in `folder`, with the `SHA256SUMS` that lists it.
    fn setup_in(folder: &Path) -> PathBuf {
        let name = "Kerno-0.2.0-windows-x86_64-setup.exe";
        let setup = folder.join(name);
        fs::write(&setup, "installer").unwrap();
        fs::write(
            folder.join("SHA256SUMS"),
            format!("{}  {name}\n", hex(b"installer")),
        )
        .unwrap();
        setup
    }

    /// V4, W2: at a start every download folder goes but those of a Kerno still running and those
    /// in use.
    #[test]
    fn every_download_folder_not_in_use_goes_at_the_next_start() {
        let downloads = tempfile::tempdir().unwrap();
        let old = downloads.path().join("kerno-update-1-2");
        fs::create_dir(&old).unwrap();
        fs::write(old.join("SHA256SUMS"), "sums").unwrap();
        let held = downloads.path().join("kerno-update-3-4");
        fs::create_dir(&held).unwrap();
        fs::write(held.join("setup.exe"), "installer").unwrap();
        // Someone else's, and a file of Kerno's name: never touched.
        fs::create_dir(downloads.path().join("other")).unwrap();
        fs::write(downloads.path().join("kerno-update-file"), "x").unwrap();
        // On Windows a running installer's file cannot be deleted: held as Kerno holds it.
        #[cfg(windows)]
        let running = {
            use std::os::windows::fs::OpenOptionsExt as _;
            fs::OpenOptions::new()
                .read(true)
                .share_mode(1)
                .open(held.join("setup.exe"))
                .unwrap()
        };
        // Another Kerno's download, in progress or checked and waiting: its pid is a running
        // `kerno.exe`, and its folder stays (W2).
        let theirs = downloads.path().join("kerno-update-77-5");
        fs::create_dir(&theirs).unwrap();
        let alive = |pid| pid == 77;
        let left = sweep_downloads(downloads.path(), &alive);
        assert!(theirs.is_dir());
        assert!(!old.exists());
        assert!(downloads.path().join("other").is_dir());
        assert!(downloads.path().join("kerno-update-file").is_file());
        #[cfg(windows)]
        {
            assert_eq!(left, std::slice::from_ref(&held));
            drop(running);
            assert!(sweep_downloads(downloads.path(), &alive).is_empty());
        }
        #[cfg(not(windows))]
        assert!(left.is_empty());
        assert!(!held.exists());
        // No folder at all: nothing to do.
        assert!(sweep_downloads(&downloads.path().join("none"), &alive).is_empty());
        // That Kerno gone (or its pid now another program's): its folder goes too.
        assert!(sweep_downloads(downloads.path(), &|_| false).is_empty());
        assert!(!theirs.exists());
    }

    /// W2: `tasklist`'s answer names a running `kerno.exe` by its pid, in any language.
    #[test]
    fn only_a_running_kerno_keeps_its_download() {
        let row = |image: &str, pid: &str| {
            format!("\"{image}\",\"{pid}\",\"Console\",\"1\",\"123,456 K\"\r\n")
        };
        assert!(tasklist_names_kerno(&row("kerno.exe", "4242"), 4242));
        assert!(tasklist_names_kerno(&row("KERNO.EXE", "4242"), 4242));
        // The pid now another program's, another pid, no such process (localised), nothing.
        assert!(!tasklist_names_kerno(&row("notepad.exe", "4242"), 4242));
        assert!(!tasklist_names_kerno(&row("kerno.exe", "42420"), 4242));
        assert!(!tasklist_names_kerno(
            "INFO: No tasks are running which match the specified criteria.\r\n",
            4242
        ));
        assert!(!tasklist_names_kerno(
            "INFORMATIONS : aucune tâche en service ne correspond aux critères spécifiés.\r\n",
            4242
        ));
        assert!(!tasklist_names_kerno("", 4242));
        // This process is always alive; with no `tasklist.exe` (outside `C:\Windows`, or not on
        // Windows) a folder counts as alive and stays.
        let fake = Fake::new(|_, _| Ok(Vec::new()));
        assert!(kerno_alive(&*fake, std::process::id()));
        if kerno_core::tools::windows_program("tasklist.exe").is_none() {
            assert!(kerno_alive(&*fake, 4242));
            assert!(fake.calls().is_empty());
        }
    }

    /// W1: the installer gets the download's folder as `TEMP` and `TMP`, and the user's own in
    /// `KERNO_SETUP_*` to put back; unset or empty ones are passed as removed.
    #[test]
    fn the_installer_is_given_the_users_temporary_folders_to_put_back() {
        let own = |name: &str| match name {
            "TEMP" => Some(OsString::from(r"C:\Users\Élodie Dupont\AppData\Local\Temp")),
            "TMP" => Some(OsString::new()),
            _ => None,
        };
        let env = setup_env(Path::new(r"D:\updates\kerno-update-1-2"), own);
        let folder = Some(OsString::from(r"D:\updates\kerno-update-1-2"));
        assert_eq!(
            env,
            [
                ("TEMP".to_string(), folder.clone()),
                (
                    "KERNO_SETUP_TEMP".to_string(),
                    Some(r"C:\Users\Élodie Dupont\AppData\Local\Temp".into())
                ),
                ("TMP".to_string(), folder),
                ("KERNO_SETUP_TMP".to_string(), None),
                ("KERNO_SETUP_ENV".to_string(), Some("1".into())),
            ]
        );
    }

    #[test]
    fn a_zip_copy_starts_the_installed_kerno_instead_of_installing_it_again() {
        let programs = tempfile::tempdir().unwrap();
        let installed = programs.path().join("Kerno").join("kerno.exe");
        let zip = programs
            .path()
            .join("kerno-0.1.0-windows-x86_64")
            .join("kerno.exe");
        let answer =
            "\r\nHKEY_CURRENT_USER\\Software\\…\r\n    DisplayVersion    REG_SZ    0.2.0\r\n\r\n";
        let reg = || Some(PathBuf::from(r"C:\Windows\System32\reg.exe"));
        let fake = Fake::new(move |_, _| Ok(answer.as_bytes().to_vec()));
        // Nothing installed: the zip copy installs as before.
        assert_eq!(
            installed_elsewhere(&*fake, &zip, programs.path(), reg()),
            None
        );
        assert!(fake.calls().is_empty());
        fs::create_dir_all(installed.parent().unwrap()).unwrap();
        fs::write(&installed, "exe").unwrap();
        assert_eq!(
            installed_elsewhere(&*fake, &zip, programs.path(), reg()),
            Some((v("0.2.0"), installed.clone()))
        );
        let (program, args) = fake.calls()[0].clone();
        assert_eq!(program, reg().unwrap());
        assert_eq!(args, ["query", UNINSTALL_KEY, "/v", "DisplayVersion"]);
        // The installed copy itself, no `reg.exe` (Windows not in C:\Windows), no entry: none.
        assert_eq!(
            installed_elsewhere(&*fake, &installed, programs.path(), reg()),
            None
        );
        assert_eq!(
            installed_elsewhere(&*fake, &zip, programs.path(), None),
            None
        );
        let failing = Fake::new(|_, _| {
            Err("ERROR: The system was unable to find the specified registry key".into())
        });
        assert_eq!(
            installed_elsewhere(&*failing, &zip, programs.path(), reg()),
            None
        );
        assert_eq!(
            display_version("    DisplayVersion    REG_DWORD    0x1"),
            None
        );
        assert_eq!(display_version("    DisplayName    REG_SZ    Kerno"), None);
        assert_eq!(
            display_version("    DisplayVersion    REG_SZ    0.2.0-beta"),
            None
        );
    }

    /// V9: a test build says so in `--version`, which `scripts/bundle-windows.sh` refuses.
    #[test]
    fn a_test_build_says_so_in_its_version() {
        assert!(VERSION_LINE.starts_with("kerno "));
        assert_eq!(
            VERSION_LINE.ends_with("+test-update-without-click"),
            cfg!(feature = "test-update-without-click")
        );
        assert_eq!(
            VERSION_LINE == concat!("kerno ", env!("CARGO_PKG_VERSION")),
            !cfg!(feature = "test-update-without-click")
        );
    }

    /// K70: the installer is checked again through a handle that stays open until its process is
    /// started; on Windows nobody may write, rename or delete it in between (tried from the fake's
    /// start, as another process would). A changed installer or a missing line runs nothing.
    #[test]
    fn the_installer_is_held_shut_from_its_check_to_its_start() {
        let temp = tempfile::tempdir().unwrap();
        let setup = setup_in(temp.path());
        let target = Install::Setup(PathBuf::from("kerno.exe"));
        let tried = Arc::new(Mutex::new(Vec::<bool>::new()));
        let seen = tried.clone();
        let fake = Fake::new(move |program, _| {
            seen.lock().unwrap().extend([
                fs::OpenOptions::new().write(true).open(program).is_ok(),
                fs::remove_file(program).is_ok(),
            ]);
            Ok(Vec::new())
        });
        install(&*fake, &target, &setup, v("0.2.0"), None).unwrap();
        let tried = tried.lock().unwrap().clone();
        if cfg!(windows) {
            assert_eq!(tried, [false, false], "written to or deleted while held");
            assert!(setup.is_file());
        } else {
            // The 0700 folder keeps other accounts out; this one may still write.
            assert_eq!(tried.len(), 2);
        }

        // Changed after the download's check, or no longer listed: nothing is started.
        let fake = Fake::new(|_, _| Ok(Vec::new()));
        let setup = setup_in(temp.path());
        fs::write(&setup, "another installer").unwrap();
        assert_eq!(
            install(&*fake, &target, &setup, v("0.2.0"), None),
            Err("the download does not match its checksum".to_string())
        );
        let setup = setup_in(temp.path());
        fs::write(temp.path().join("SHA256SUMS"), "").unwrap();
        assert!(install(&*fake, &target, &setup, v("0.2.0"), None).is_err());
        assert!(fake.calls().is_empty());
    }

    /// K67 (D-136), on the Windows runner of the probe workflow only: the check, as the app runs
    /// it, through Windows' own `curl.exe`, finds a release at least as new as 2026.11.3, the
    /// first with a Windows zip. The one request this test makes is the app's own.
    #[test]
    fn windows_k67_the_check_finds_the_latest_release_with_system32_curl() {
        if !kerno_core::testing::k67_runner() {
            return;
        }
        let expected = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32")
            .join("curl.exe");
        assert_eq!(curl(), Ok(expected));
        let latest = check(&System, "0.0.1")
            .expect("GitHub answered")
            .expect("a release");
        assert!(latest >= v("2026.11.3"), "{latest}");
        eprintln!("K67 update check: {:?} found {latest}", curl());
    }
}
