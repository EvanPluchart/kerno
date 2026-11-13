//! The update check and install (CLAUDE.md §3 item 12, docs/DECISIONS.md D-045): the steps that
//! run a command or touch files. What is newer, the `curl` arguments and what a tarball may hold
//! are `kerno_core::update`; the status bar item is `workspace/updates.rs`.
//!
//! Ported from the owner's Recto (`recto-app/src/update/mod.rs`) and Vigie
//! (`vigie-app/src/update.rs`), MIT, same owner: the same timing, the same refusals (no
//! checksum line, a mismatch: deleted, never run), the same "otherwise the release page". They
//! install on Windows through an installer; Kerno swaps its own bundle on macOS and its own files
//! on Linux, by rename, with a rollback.
//!
//! Every external program is run through [`Runner`] with a fixed argument array and an absolute
//! path: the real one ([`System`]) is made only in `main.rs`, after the off switches; tests use a
//! fake and never reach the network.

use std::ffi::OsString;
use std::fs;
use std::io::Read as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::Global;
use kerno_core::update::{self as releases, Version};
use sha2::{Digest as _, Sha256};

/// The first check, once the window is up; then one every `EVERY`.
pub const FIRST: Duration = Duration::from_secs(3);
pub const EVERY: Duration = Duration::from_secs(6 * 3600);

/// Runs the external programs of an update. One trait for `curl`, `hdiutil`, `ditto`, `tar`
/// and the relaunch, so a test fake sees (and asserts) every argument array.
pub trait Runner: Send + Sync {
    /// Runs `program` with `args` and waits: its standard output, or why it failed.
    fn output(&self, program: &Path, args: &[OsString]) -> Result<Vec<u8>, String>;
    /// Starts `program` with `args` without waiting: the new Kerno.
    fn start(&self, program: &Path, args: &[OsString]) -> Result<(), String>;
}

/// The system's own programs.
pub struct System;

impl Runner for System {
    fn output(&self, program: &Path, args: &[OsString]) -> Result<Vec<u8>, String> {
        let output = std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
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
}

/// The absolute path of a system program: the first of `places` that exists, else the first
/// (running it then says it is missing).
fn program(places: &[&str]) -> PathBuf {
    let found = places.iter().find(|place| Path::new(place).is_file());
    PathBuf::from(found.or(places.first()).copied().unwrap_or_default())
}

fn curl() -> PathBuf {
    program(&["/usr/bin/curl", "/bin/curl"])
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
    /// Anything else (the bare `target/release/kerno`, a read-only place, a package manager's
    /// install, a translocated app): the release page.
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
        _ => Install::Page,
    }
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
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&dir)
        .map_err(|error| format!("could not make {}: {error}", dir.display()))?;
    Ok(dir)
}

/// The newest release when it is newer than `current`; `Ok(None)` when Kerno is up to date;
/// `Err` when it could not tell (offline, no `curl`, no release published, an odd answer).
pub fn check(runner: &dyn Runner, current: &str) -> Result<Option<Version>, String> {
    let head = runner.output(&curl(), &releases::check_args(current))?;
    let latest = releases::latest_from_head(&String::from_utf8_lossy(&head))
        .ok_or("GitHub named no release")?;
    Ok(releases::newer(current, latest))
}

/// Downloads this system's file of `version` and its `SHA256SUMS` into a new private folder
/// under `temp`, and checks the file's SHA-256 against its line. Any failure deletes the folder:
/// an unchecked file is never kept, run or opened.
pub fn download(
    runner: &dyn Runner,
    current: &str,
    version: Version,
    temp: &Path,
) -> Result<PathBuf, String> {
    let name = releases::artifact(version, std::env::consts::OS, std::env::consts::ARCH)
        .ok_or("no release file for this system")?;
    let dir = private_dir(temp, "kerno-update")?;
    let checked = (|| {
        let sums_path = dir.join("SHA256SUMS");
        let url = version.download_url("SHA256SUMS");
        let args = releases::download_args(current, &url, &sums_path, releases::SUMS_MAX_BYTES);
        runner.output(&curl(), &args)?;
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
        runner.output(&curl(), &args)?;
        if fs::metadata(&path).map_or(0, |meta| meta.len()) == 0 {
            return Err("the download is empty".to_string());
        }
        if sha256_file(&path)? != expected {
            return Err("the download does not match its checksum".to_string());
        }
        Ok(path)
    })();
    if checked.is_err() {
        let _ = fs::remove_dir_all(&dir);
    }
    checked
}

fn sha256_file(path: &Path) -> Result<[u8; 32], String> {
    let mut file = fs::File::open(path).map_err(|error| error.to_string())?;
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
/// process should then quit. On a failure everything is put back as it was.
pub fn install(
    runner: &dyn Runner,
    target: &Install,
    file: &Path,
    version: Version,
) -> Result<(), String> {
    match target {
        Install::App(bundle) => install_app(runner, bundle, file),
        Install::Prefix(dir) => install_prefix(runner, dir, file, version),
        Install::Page => Err("this copy of Kerno opens the release page instead".into()),
    }
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
    pub phase: Phase,
    /// A check is under way.
    pub checking: bool,
}

impl Global for Updates {}

impl Updates {
    pub fn new(runner: Arc<dyn Runner>, current: &str, target: Install, temp: PathBuf) -> Self {
        Self {
            runner,
            current: current.to_string(),
            target,
            temp,
            phase: Phase::Idle,
            checking: false,
        }
    }
}

/// Whether the environment turns the check off for this run: `KERNO_NO_UPDATE_CHECK`, or a
/// bench run (`KERNO_BENCH`). `var` tells whether a variable is set.
fn switched_off(var: impl Fn(&str) -> bool) -> bool {
    !releases::checks_allowed(var("KERNO_NO_UPDATE_CHECK"), var("KERNO_BENCH"))
}

/// The real updater, or `None` when an off switch is set (`KERNO_NO_UPDATE_CHECK`, a bench run)
/// or in a test build. `var` tells whether an environment variable is set.
pub fn for_this_run(var: impl Fn(&str) -> bool, exe: Option<PathBuf>) -> Option<Updates> {
    if cfg!(test) || switched_off(var) {
        return None;
    }
    let target = exe.map_or(Install::Page, |exe| {
        install_target(&exe, std::env::consts::OS)
    });
    Some(Updates::new(
        Arc::new(System),
        env!("CARGO_PKG_VERSION"),
        target,
        std::env::temp_dir(),
    ))
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
    }

    /// `curl --head`'s answer naming `tag` as the latest release.
    pub fn redirect_to(tag: &str) -> Vec<u8> {
        format!(
            "HTTP/2 302\r\nlocation: https://github.com/EvanPluchart/kerno/releases/tag/{tag}\r\n\r\n"
        )
        .into_bytes()
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
        assert!(
            program.is_absolute() && program.ends_with("curl"),
            "{program:?}"
        );
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
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(folder).unwrap().permissions().mode() & 0o777,
            0o700
        );
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

    fn marker(bundle: &Path) -> String {
        fs::read_to_string(bundle.join("Contents/MacOS/kerno")).unwrap()
    }

    /// A runner for the macOS install: `hdiutil attach` puts a new `Kerno.app` in the mount
    /// point, `ditto` copies it, `open` fails when `relaunch_fails`.
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
        assert!(install(&*fake, &target, &dmg, v("0.2.0")).is_err());
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
        install(&*fake, &target, &dmg, v("0.2.0")).unwrap();
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
        assert!(install(&*failing, &target, &tarball, v("0.2.0")).is_err());
        assert_eq!(fs::read_to_string(dir.join("bin/kerno")).unwrap(), "old");
        assert!(!dir.join("share/doc/kerno/README.md").exists());

        // A link in the archive: refused before anything is extracted.
        let linked = linux_runner(
            top.clone(),
            "drwx x\ndrwx x\nlrwx x -> /etc\n-rw- x\n",
            false,
        );
        assert!(install(&*linked, &target, &tarball, v("0.2.0")).is_err());
        assert!(linked.calls().iter().all(|(_, args)| args[0] != "-x"));

        let fake = linux_runner(top, kinds, false);
        install(&*fake, &target, &tarball, v("0.2.0")).unwrap();
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

    #[test]
    fn the_environment_switches_and_a_test_build_make_no_updater() {
        assert!(!switched_off(|_| false));
        assert!(switched_off(|name| name == "KERNO_NO_UPDATE_CHECK"));
        assert!(switched_off(|name| name == "KERNO_BENCH"));
        assert!(!switched_off(|name| name == "KERNO_NO_UPDATE"));
        // Whatever the environment says, a test build never makes the real one.
        assert!(for_this_run(|_| false, None).is_none());
    }
}
