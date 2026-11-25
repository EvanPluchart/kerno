//! The update check and install (docs/plans/M10.md Part 13, CLAUDE.md §3 item 12). A fake
//! runner answers for `curl`, `hdiutil`, `ditto` and `open`: nothing here reaches the network,
//! and nothing touches a real `Kerno.app`.

use super::tool_wizards::paints;
use super::*;
use crate::tabs::TabAction;
use crate::update::tests::{Fake, is_curl, redirect_to, value_of};
use crate::update::{self, Install, Phase, Updates};
use crate::workspace::updates::{update_label, update_tooltip, what_update_does};
use kerno_core::update::{self as releases, Version};

fn v(text: &str) -> Version {
    Version::parse(text).unwrap()
}

/// Lets this run check, as `main.rs` does once the off switches allow it, with `runner` in
/// place of the system's programs.
fn allow_updates(
    app: &Fixture,
    cx: &mut TestAppContext,
    runner: Arc<Fake>,
    current: &str,
    target: Install,
    temp: &Path,
) {
    let updates = Updates::new(runner, current, target, temp.to_path_buf());
    cx.update(|cx| cx.set_global(updates));
    app.step(cx, |_, _| ());
}

/// Gives the updater a wall clock that moves with the test's timer, plus the returned time the
/// computer spent asleep, which the timer does not count (K57).
fn wall_clock(cx: &mut TestAppContext) -> Arc<std::sync::Mutex<Duration>> {
    let executor = cx.executor();
    let (start, base) = (executor.now(), std::time::SystemTime::now());
    let slept = Arc::new(std::sync::Mutex::new(Duration::ZERO));
    let asleep = slept.clone();
    cx.update(|cx| {
        cx.global_mut::<Updates>().clock =
            Arc::new(move || base + (executor.now() - start) + *asleep.lock().unwrap())
    });
    slept
}

fn phase(cx: &mut TestAppContext) -> Phase {
    cx.update(|cx| cx.global::<Updates>().phase.clone())
}

fn set_phase(app: &Fixture, cx: &mut TestAppContext, phase: Phase) {
    cx.update(|cx| cx.global_mut::<Updates>().phase = phase);
    app.step(cx, |_, _| ());
}

fn label(cx: &mut TestAppContext) -> Option<String> {
    cx.update(|cx| update_label(cx).map(|(text, _)| text))
}

/// A check, timed (`manual` false) or asked for, waited for.
fn check(app: &Fixture, cx: &mut TestAppContext, manual: bool) {
    let workspace = app.workspace.clone();
    app.step(cx, |window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.check_updates(manual, window, cx)
        })
    });
    app.wait_until(cx, "the check", |_, cx| {
        cx.update(|cx| !cx.try_global::<Updates>().is_some_and(|u| u.checking))
    });
}

fn update_does(app: &Fixture, cx: &mut TestAppContext, action: TabAction) {
    let workspace = app.workspace.clone();
    app.step(cx, |window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.perform(None, action, window, cx)
        })
    });
}

/// The `curl` calls of the check, the downloads and the install; the release notes read for the
/// dialog (K43) are counted by [`notes_requests`].
fn curl_args(fake: &Fake) -> Vec<Vec<String>> {
    fake.calls()
        .into_iter()
        .filter(|(program, args)| is_curl(program) && !is_notes(args))
        .map(|(_, args)| args)
        .collect()
}

fn is_notes(args: &[String]) -> bool {
    args.iter()
        .any(|arg| arg.contains("raw.githubusercontent.com"))
}

#[gpui_kit::test]
fn only_a_strictly_newer_plain_release_is_offered(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    let temp = tempfile::tempdir().unwrap();
    let expected: Vec<String> = releases::check_args("0.1.0")
        .into_iter()
        .map(|arg| arg.into_string().unwrap())
        .collect();
    for (answer, offered) in [
        (redirect_to("v0.2.0"), Some(v("0.2.0"))),
        (redirect_to("v0.1.0"), None),
        (redirect_to("v0.0.9"), None),
        (redirect_to("v0.3.0-beta.1"), None),
        (redirect_to("latest"), None),
        // The repository does not exist yet: GitHub answers 404.
        (
            b"HTTP/2 404\r\ncontent-type: text/html\r\n\r\n".to_vec(),
            None,
        ),
    ] {
        let fake = Fake::new(move |_, _| Ok(answer.clone()));
        allow_updates(&app, cx, fake.clone(), "0.1.0", Install::Page, temp.path());
        check(&app, cx, false);
        // One request, with exactly these arguments.
        assert_eq!(curl_args(&fake), std::slice::from_ref(&expected));
        match offered {
            Some(version) => {
                assert_eq!(phase(cx), Phase::Available(version));
                assert_eq!(
                    label(cx).as_deref(),
                    Some("Kerno 0.2.0 is available · Update")
                );
                assert!(app.has(cx, "status-update"));
            }
            None => {
                assert_eq!(phase(cx), Phase::Idle);
                assert_eq!(label(cx), None);
                assert!(!app.has(cx, "status-update"));
            }
        }
    }
    // Offline: silent, and tried again at the next check.
    let fake = Fake::new(|_, _| Err("curl: (6) Could not resolve host: github.com".into()));
    allow_updates(&app, cx, fake.clone(), "0.1.0", Install::Page, temp.path());
    let before = notification_count(&app, cx);
    check(&app, cx, false);
    assert_eq!(phase(cx), Phase::Idle);
    assert_eq!(notification_count(&app, cx), before);
    // Asked for, the same failure is said.
    check(&app, cx, true);
    assert_eq!(notification_count(&app, cx), before + 1);
    assert_eq!(curl_args(&fake).len(), 2);
}

#[gpui_kit::test]
fn the_setting_and_the_environment_switch_stop_the_check(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    let temp = tempfile::tempdir().unwrap();

    // KERNO_NO_UPDATE_CHECK, a bench run, a test: `main.rs` installs no updater, so there is
    // nothing to run a request with. A timed check does nothing; one asked for says why.
    assert!(cx.update(|cx| cx.try_global::<Updates>().is_none()));
    let before = notification_count(&app, cx);
    check(&app, cx, false);
    assert_eq!(notification_count(&app, cx), before);
    check(&app, cx, true);
    assert_eq!(notification_count(&app, cx), before + 1);

    // The setting off: the timed checks make no call; asking still checks.
    let fake = Fake::new(|_, _| Ok(redirect_to("v0.2.0")));
    allow_updates(&app, cx, fake.clone(), "0.1.0", Install::Page, temp.path());
    cx.update(|cx| AppState::update_settings(cx, |settings| settings.check_updates = false));
    check(&app, cx, false);
    assert!(fake.calls().is_empty());
    assert_eq!(phase(cx), Phase::Idle);
    check(&app, cx, true);
    assert_eq!(fake.calls().len(), 1);
    assert_eq!(phase(cx), Phase::Available(v("0.2.0")));

    // The setting is in Settings ▸ Updates, on by default.
    assert!(Settings::default().check_updates);
    app.dispatch(cx, actions::OpenSettings);
    let index = crate::settings_view::SECTIONS
        .iter()
        .position(|(_, label)| *label == "Updates")
        .unwrap();
    app.click(cx, ("settings-section", index));
    app.click(cx, "settings-check-updates");
    assert!(settings_now(cx).check_updates);
}

#[gpui_kit::test]
fn the_timed_checks_run_three_seconds_after_the_window_opens_then_every_six_hours(
    cx: &mut TestAppContext,
) {
    let app = open(cx, memory_storage());
    let temp = tempfile::tempdir().unwrap();
    let fake = Fake::new(|_, _| Ok(redirect_to("v0.1.0")));
    allow_updates(&app, cx, fake.clone(), "0.1.0", Install::Page, temp.path());
    wall_clock(cx);
    let workspace = app.workspace.clone();
    app.step(cx, |window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.start_update_checks(window, cx)
        })
    });
    let calls = |cx: &mut TestAppContext| {
        cx.run_until_parked();
        fake.calls().len()
    };
    assert_eq!(calls(cx), 0);
    cx.executor().advance_clock(Duration::from_millis(2900));
    assert_eq!(calls(cx), 0);
    cx.executor().advance_clock(Duration::from_millis(100));
    // Its answer is in too: a check still under way would make the next one wait.
    let done = |calls: usize| {
        let fake = fake.clone();
        move |_: &Fixture, cx: &mut TestAppContext| {
            fake.calls().len() == calls && cx.update(|cx| !cx.global::<Updates>().checking)
        }
    };
    app.wait_until(cx, "the first check", done(1));
    cx.executor()
        .advance_clock(Duration::from_secs(6 * 3600 - 1));
    assert_eq!(calls(cx), 1);
    cx.executor().advance_clock(Duration::from_secs(1));
    app.wait_until(cx, "the second check", done(2));
}

/// A runner serving `files` to `curl` and acting as `hdiutil`, `ditto` and `open` for a macOS
/// install.
fn release_runner(files: Vec<(String, Vec<u8>)>) -> Arc<Fake> {
    release_runner_then(files, || {})
}

/// [`release_runner`], with `on_open` run when the new Kerno would start.
fn release_runner_then(
    files: Vec<(String, Vec<u8>)>,
    on_open: impl Fn() + Send + Sync + 'static,
) -> Arc<Fake> {
    Fake::new(move |program, args| {
        let fake_bundle = |at: &Path, marker: &str| {
            std::fs::create_dir_all(at.join("Contents/MacOS")).unwrap();
            std::fs::write(at.join("Contents/MacOS/kerno"), marker).unwrap();
        };
        match program.to_str().unwrap() {
            _ if is_curl(program) => {
                // The check (`--head`, no file): 0.2.0 is the latest.
                let Some(output) = value_of(args, "--output") else {
                    return Ok(redirect_to("v0.2.0"));
                };
                let name = args.last().unwrap().rsplit('/').next().unwrap();
                let (_, body) = files
                    .iter()
                    .find(|(file, _)| file == name)
                    .ok_or("curl: (22) The requested URL returned error: 404")?;
                std::fs::write(output, body).unwrap();
            }
            "/usr/bin/hdiutil" if args[0] == "attach" => {
                let mount = value_of(args, "-mountpoint").unwrap();
                fake_bundle(&Path::new(mount).join("Kerno.app"), "new");
            }
            "/usr/bin/hdiutil" => {
                std::fs::remove_dir_all(Path::new(&args[1]).join("Kerno.app")).unwrap();
            }
            "/usr/bin/ditto" => fake_bundle(Path::new(&args[1]), "new"),
            "/usr/bin/open" => on_open(),
            other => return Err(format!("unexpected {other}")),
        }
        Ok(Vec::new())
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The release file of 0.2.0 for this system, and `SHA256SUMS` giving `sum` for it.
fn release(sum: &str) -> Vec<(String, Vec<u8>)> {
    let name =
        releases::artifact(v("0.2.0"), std::env::consts::OS, std::env::consts::ARCH).unwrap();
    vec![
        ("SHA256SUMS".into(), format!("{sum}  {name}\n").into_bytes()),
        (name, b"the release".to_vec()),
    ]
}

/// A folder holding a fake `Kerno.app` saying "old".
fn applications() -> (tempfile::TempDir, std::path::PathBuf) {
    let apps = tempfile::tempdir().unwrap();
    let bundle = apps.path().join("Kerno.app");
    std::fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
    std::fs::write(bundle.join("Contents/MacOS/kerno"), "old").unwrap();
    (apps, bundle)
}

fn bundle_says(bundle: &Path) -> String {
    std::fs::read_to_string(bundle.join("Contents/MacOS/kerno")).unwrap()
}

#[gpui_kit::test]
fn a_checksum_mismatch_refuses_deletes_the_download_and_offers_the_page(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    let temp = tempfile::tempdir().unwrap();
    let (_apps, bundle) = applications();
    let fake = release_runner(release(&sha256_hex(b"another file")));
    allow_updates(
        &app,
        cx,
        fake.clone(),
        "0.1.0",
        Install::App(bundle.clone()),
        temp.path(),
    );
    set_phase(&app, cx, Phase::Available(v("0.2.0")));
    app.click(cx, "status-update-button");
    app.wait_until(cx, "the refusal", |_, cx| {
        matches!(phase(cx), Phase::Failed(..))
    });
    let Phase::Failed(_, why) = phase(cx) else {
        unreachable!()
    };
    assert!(why.contains("does not match its checksum"), "{why}");
    // Nothing kept, nothing opened or run: only the two downloads were made.
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    assert_eq!(fake.calls().len(), 2);
    assert_eq!(bundle_says(&bundle), "old");
    assert_eq!(
        label(cx).as_deref(),
        Some("Kerno 0.2.0 was not installed · Release page")
    );
    app.click(cx, "status-update-button");
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://github.com/EvanPluchart/kerno/releases/tag/v0.2.0")
    );
}

#[gpui_kit::test]
fn update_installs_the_checked_release_and_starts_it(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    let temp = tempfile::tempdir().unwrap();
    let (apps, bundle) = applications();
    // What the new Kerno finds in the store when it starts: the text typed just before.
    let stored_at_start = Arc::new(std::sync::Mutex::new(None));
    let (storage, seen) = (app.storage.clone(), stored_at_start.clone());
    let fake = release_runner_then(release(&sha256_hex(b"the release")), move || {
        *seen.lock().unwrap() = Some(storage.tabs::<TabSnapshot>().unwrap());
    });
    allow_updates(
        &app,
        cx,
        fake.clone(),
        "0.1.0",
        Install::App(bundle.clone()),
        temp.path(),
    );
    app.press(cx, &["secondary-t"]);
    app.type_text(cx, "select 'typed before the update'");
    set_phase(&app, cx, Phase::Available(v("0.2.0")));
    // No tab holds a transaction or a pending change: no question, the install follows.
    app.click(cx, "status-update-button");
    // The new Kerno has started once it read the store: the fake runner
    // lists the `open` call before it runs it, on another thread.
    app.wait_until(cx, "the relaunch", |_, _| {
        stored_at_start.lock().unwrap().is_some()
    });
    assert!(
        fake.calls()
            .iter()
            .any(|(program, _)| program.ends_with("open"))
    );
    assert_eq!(phase(cx), Phase::Installing(v("0.2.0")));
    assert_eq!(bundle_says(&bundle), "new");
    let stored = stored_at_start.lock().unwrap().clone();
    assert!(
        matches!(stored.as_deref(), Some([TabSnapshot::Query { sql, .. }]) if sql == "select 'typed before the update'"),
        "{stored:?}"
    );
    let (program, args) = fake.calls().last().unwrap().clone();
    assert_eq!(program, Path::new("/usr/bin/open"));
    assert_eq!(args, ["-n".to_string(), bundle.display().to_string()]);
    let old = apps
        .path()
        .join(format!("Kerno.app.old-{}", std::process::id()));
    assert_eq!(bundle_says(&old), "old");
    // The checked download is removed once installed.
    app.wait_until(cx, "the download removed", |_, _| {
        std::fs::read_dir(temp.path()).unwrap().count() == 0
    });

    // A copy that cannot install itself opens the release page instead.
    let page = release_runner(Vec::new());
    allow_updates(&app, cx, page.clone(), "0.1.0", Install::Page, temp.path());
    set_phase(&app, cx, Phase::Available(v("0.2.0")));
    app.click(cx, "status-update-button");
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://github.com/EvanPluchart/kerno/releases/tag/v0.2.0")
    );
    assert!(page.calls().is_empty());
}

#[gpui_kit::test]
fn quitting_to_update_asks_first_and_declining_keeps_the_download(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    let temp = tempfile::tempdir().unwrap();
    let (_apps, bundle) = applications();
    let fake = release_runner(release(&sha256_hex(b"the release")));
    allow_updates(
        &app,
        cx,
        fake.clone(),
        "0.1.0",
        Install::App(bundle.clone()),
        temp.path(),
    );
    app.press(cx, &["secondary-t"]);
    app.open_transaction(cx, "select 1 as kerno_ui_update_quit");
    set_phase(&app, cx, Phase::Available(v("0.2.0")));

    app.click(cx, "status-update-button");
    app.wait_until(cx, "the quit question", |app, cx| {
        app.has(cx, "end-question")
    });
    assert!(app.shows_label(cx, "Quit Kerno to install 0.2.0?"));
    assert!(app.shows_label(cx, "“Query 1”: a transaction is open."));
    app.click(cx, "end-cancel");
    assert!(!app.dialog_open(cx));
    let Phase::Ready(version, file) = phase(cx) else {
        panic!("not ready: {:?}", phase(cx))
    };
    assert_eq!(version, v("0.2.0"));
    assert!(file.is_file(), "the checked download is kept");
    assert_eq!(bundle_says(&bundle), "old");
    assert_eq!(app.tx_state(cx), kerno_core::TxState::Open);
    let downloads = curl_args(&fake).len();

    // The next click asks again without downloading again; rolling back installs.
    app.click(cx, "status-update-button");
    assert!(app.has(cx, "end-question"));
    app.click(cx, "end-rollback");
    app.wait_until(cx, "the install", |_, cx| {
        phase(cx) == Phase::Installing(v("0.2.0"))
            && fake
                .calls()
                .iter()
                .any(|(program, _)| program.ends_with("open"))
    });
    assert_eq!(curl_args(&fake).len(), downloads);
    assert_eq!(bundle_says(&bundle), "new");
    assert_eq!(app.tx_state(cx), kerno_core::TxState::Idle);
}

#[gpui_kit::test]
fn the_update_item_answers_a_right_click_and_skip_is_remembered(cx: &mut TestAppContext) {
    let storage = memory_storage();
    let app = open(cx, storage.clone());
    let temp = tempfile::tempdir().unwrap();
    let fake = Fake::new(|_, _| Ok(redirect_to("v0.2.0")));
    allow_updates(&app, cx, fake.clone(), "0.1.0", Install::Page, temp.path());
    check(&app, cx, false);
    assert_eq!(phase(cx), Phase::Available(v("0.2.0")));
    // The dialog of the timed check first (D-097): Escape is "Later".
    assert!(update_dialog(&app, cx));
    app.press(cx, &["escape"]);
    assert!(!app.dialog_open(cx));

    app.right_click(cx, "status-update", None);
    for entry in [
        "Update",
        "Open release page",
        "Skip this version",
        "Turn off update checks",
    ] {
        assert!(app.shows_label(cx, entry), "{entry}");
    }
    app.click_label(cx, "Skip this version");
    // The item stays (D-097), its tooltip saying the version was skipped.
    assert_eq!(phase(cx), Phase::Available(v("0.2.0")));
    assert!(app.has(cx, "status-update"));
    assert!(cx.update(|cx| update_tooltip(cx)).is_some());
    assert_eq!(settings_now(cx).skipped_update.as_deref(), Some("0.2.0"));
    app.wait_until(cx, "the stored skip", |_, _| {
        Settings::load(&storage).skipped_update.as_deref() == Some("0.2.0")
    });

    // The timed checks keep the item and open no dialog; asking offers it too.
    check(&app, cx, false);
    assert_eq!(phase(cx), Phase::Available(v("0.2.0")));
    check(&app, cx, true);
    assert_eq!(phase(cx), Phase::Available(v("0.2.0")));
    assert!(!app.dialog_open(cx));
    // A newer one than the skipped one is offered again, without the skip's tooltip.
    let newer = Fake::new(|_, _| Ok(redirect_to("v0.3.0")));
    allow_updates(&app, cx, newer, "0.1.0", Install::Page, temp.path());
    check(&app, cx, false);
    assert_eq!(phase(cx), Phase::Available(v("0.3.0")));
    assert!(cx.update(|cx| update_tooltip(cx)).is_none());
    app.press(cx, &["escape"]);

    update_does(&app, cx, TabAction::OpenReleasePage);
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://github.com/EvanPluchart/kerno/releases/tag/v0.3.0")
    );
    update_does(&app, cx, TabAction::TurnOffUpdateChecks);
    assert!(!settings_now(cx).check_updates);
    assert_eq!(phase(cx), Phase::Idle);
}

/// Whether the update dialog of D-097 is on screen.
/// The update checks (`curl --head`) `fake` was asked for.
fn checks(fake: &Fake) -> usize {
    curl_args(fake)
        .iter()
        .filter(|args| args.iter().any(|arg| arg == "--head"))
        .count()
}

/// The release notes requests `fake` was asked for (K43).
fn notes_requests(fake: &Fake) -> usize {
    fake.calls()
        .iter()
        .filter(|(_, args)| is_notes(args))
        .count()
}

fn update_dialog(app: &Fixture, cx: &mut TestAppContext) -> bool {
    app.has(cx, "update-dialog-title")
}

/// A new run of Kerno on the same store: the settings read back from it, a new updater (no
/// dialog shown yet) with `runner`.
fn next_run(
    app: &Fixture,
    cx: &mut TestAppContext,
    runner: Arc<Fake>,
    target: Install,
    temp: &Path,
) {
    let wanted = settings_now(cx).skipped_update;
    app.wait_until(cx, "the stored settings", |app, _| {
        Settings::load(&app.storage).skipped_update == wanted
    });
    let stored = Settings::load(&app.storage);
    cx.update(|cx| cx.global_mut::<AppState>().settings = stored);
    allow_updates(app, cx, runner, "0.1.0", target, temp);
}

#[gpui_kit::test]
fn a_timed_check_that_finds_a_newer_version_opens_the_dialog_once_per_version_per_run(
    cx: &mut TestAppContext,
) {
    let app = open(cx, memory_storage());
    let temp = tempfile::tempdir().unwrap();
    let fake = Fake::new(|_, _| Ok(redirect_to("v0.2.0")));
    allow_updates(&app, cx, fake.clone(), "0.1.0", Install::Page, temp.path());
    check(&app, cx, false);
    assert!(update_dialog(&app, cx));
    assert!(paints(&app, cx, "Kerno 0.2.0 is available"));
    assert!(paints(&app, cx, "You have 0.1.0."));
    assert!(paints(
        &app,
        cx,
        "Update opens the release page in your browser"
    ));
    // The status bar item is there too.
    assert!(app.has(cx, "status-update"));

    // "What's new" opens the release page in the browser: Kerno makes no request of its own.
    app.click(cx, "update-dialog-notes");
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://github.com/EvanPluchart/kerno/releases/tag/v0.2.0")
    );
    assert!(update_dialog(&app, cx));
    // One check; the release's notes are read too (K43), but no page.
    assert_eq!(checks(&fake), 1);

    // Later closes, stores nothing, keeps the item.
    app.click(cx, "update-dialog-later");
    assert!(!app.dialog_open(cx));
    assert_eq!(settings_now(cx).skipped_update, None);
    assert_eq!(phase(cx), Phase::Available(v("0.2.0")));
    assert_eq!(checks(&fake), 1);

    // The 6-hour check of the same run: the status bar is enough for that version (D-097); a
    // newer one is offered then (K57, D-123), once; an older one, never again.
    check(&app, cx, false);
    assert!(!app.dialog_open(cx));
    cx.update(|cx| {
        let newer = Fake::new(|_, _| Ok(redirect_to("v0.3.0")));
        cx.global_mut::<Updates>().runner = newer;
    });
    check(&app, cx, false);
    assert_eq!(phase(cx), Phase::Available(v("0.3.0")));
    assert!(update_dialog(&app, cx));
    assert!(paints(&app, cx, "Kerno 0.3.0 is available"));
    app.click(cx, "update-dialog-later");
    check(&app, cx, false);
    assert!(!app.dialog_open(cx));
    cx.update(|cx| {
        let older = Fake::new(|_, _| Ok(redirect_to("v0.2.0")));
        cx.global_mut::<Updates>().runner = older;
    });
    check(&app, cx, false);
    assert_eq!(phase(cx), Phase::Available(v("0.2.0")));
    assert!(!app.dialog_open(cx));

    // Help ▸ Check for Updates… says it in a notification, not with the dialog.
    let fake = Fake::new(|_, _| Ok(redirect_to("v0.2.0")));
    allow_updates(&app, cx, fake.clone(), "0.1.0", Install::Page, temp.path());
    check(&app, cx, true);
    assert_eq!(phase(cx), Phase::Available(v("0.2.0")));
    assert!(!app.dialog_open(cx));

    // The same version or an older one: no dialog.
    for tag in ["v0.1.0", "v0.0.9"] {
        let fake = Fake::new(move |_, _| Ok(redirect_to(tag)));
        allow_updates(&app, cx, fake, "0.1.0", Install::Page, temp.path());
        check(&app, cx, false);
        assert!(!app.dialog_open(cx), "{tag}");
    }

    // The check turned off: no request, no dialog.
    let fake = Fake::new(|_, _| Ok(redirect_to("v0.2.0")));
    allow_updates(&app, cx, fake.clone(), "0.1.0", Install::Page, temp.path());
    cx.update(|cx| AppState::update_settings(cx, |settings| settings.check_updates = false));
    check(&app, cx, false);
    assert!(fake.calls().is_empty());
    assert!(!app.dialog_open(cx));
}

#[gpui_kit::test]
fn later_asks_again_at_the_next_run_and_skip_stops_asking_for_that_version_only(
    cx: &mut TestAppContext,
) {
    let app = open(cx, memory_storage());
    let temp = tempfile::tempdir().unwrap();
    let latest = |tag: &'static str| Fake::new(move |_, _| Ok(redirect_to(tag)));
    allow_updates(
        &app,
        cx,
        latest("v0.2.0"),
        "0.1.0",
        Install::Page,
        temp.path(),
    );
    check(&app, cx, false);
    app.click(cx, "update-dialog-later");
    assert!(!app.dialog_open(cx));

    // The next run asks again.
    next_run(&app, cx, latest("v0.2.0"), Install::Page, temp.path());
    check(&app, cx, false);
    assert!(update_dialog(&app, cx));
    app.click(cx, "update-dialog-skip");
    assert!(!app.dialog_open(cx));
    assert_eq!(settings_now(cx).skipped_update.as_deref(), Some("0.2.0"));
    // The item stays, so it can still be installed.
    assert_eq!(phase(cx), Phase::Available(v("0.2.0")));
    assert!(app.has(cx, "status-update"));
    assert_eq!(
        label(cx).as_deref(),
        Some("Kerno 0.2.0 is available · Update")
    );

    // The run after: no dialog for 0.2.0, the item still there.
    next_run(&app, cx, latest("v0.2.0"), Install::Page, temp.path());
    check(&app, cx, false);
    assert!(!app.dialog_open(cx));
    assert!(app.has(cx, "status-update"));
    assert!(cx.update(|cx| update_tooltip(cx)).is_some());

    // A newer version asks again.
    next_run(&app, cx, latest("v0.3.0"), Install::Page, temp.path());
    check(&app, cx, false);
    assert!(update_dialog(&app, cx));
    assert!(paints(&app, cx, "Kerno 0.3.0 is available"));
}

#[gpui_kit::test]
fn ask_again_in_settings_clears_the_skipped_version(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    cx.update(|cx| {
        AppState::update_settings(cx, |settings| {
            settings.skipped_update = Some("0.3.0".into())
        })
    });
    app.dispatch(cx, actions::OpenSettings);
    let index = crate::settings_view::SECTIONS
        .iter()
        .position(|(_, label)| *label == "Updates")
        .unwrap();
    app.click(cx, ("settings-section", index));
    assert!(paints(&app, cx, "Kerno 0.3.0 was skipped"));
    app.click(cx, "settings-ask-again");
    assert_eq!(settings_now(cx).skipped_update, None);
    assert!(!app.has(cx, "settings-skipped-update"));
    assert!(!app.has(cx, "settings-ask-again"));
}

#[gpui_kit::test]
fn update_in_the_dialog_takes_the_path_of_the_status_bar_item(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    let temp = tempfile::tempdir().unwrap();
    let (_apps, bundle) = applications();
    let fake = release_runner(release(&sha256_hex(b"the release")));
    let target = Install::App(bundle.clone());
    allow_updates(&app, cx, fake.clone(), "0.1.0", target.clone(), temp.path());
    check(&app, cx, false);
    assert!(update_dialog(&app, cx));
    let does = what_update_does(&target, v("0.2.0"));
    assert!(does.contains(&bundle.display().to_string()), "{does}");
    assert!(paints(&app, cx, "and starts it again"));
    app.click(cx, "update-dialog-update");
    // Checked against SHA256SUMS, installed, started: as from the status bar.
    app.wait_until(cx, "the relaunch", |_, _| {
        fake.calls()
            .iter()
            .any(|(program, _)| program.ends_with("open"))
    });
    assert_eq!(phase(cx), Phase::Installing(v("0.2.0")));
    assert_eq!(bundle_says(&bundle), "new");
    // The check, the checksums, the file.
    assert_eq!(curl_args(&fake).len(), 3);

    // A copy that cannot install itself: Update opens the release page, nothing is downloaded.
    let page = release_runner(Vec::new());
    allow_updates(&app, cx, page.clone(), "0.1.0", Install::Page, temp.path());
    check(&app, cx, false);
    assert!(update_dialog(&app, cx));
    app.click(cx, "update-dialog-update");
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://github.com/EvanPluchart/kerno/releases/tag/v0.2.0")
    );
    assert_eq!(curl_args(&page).len(), 1);
    assert!(!app.dialog_open(cx));
}

#[gpui_kit::test]
fn the_dialog_waits_for_another_dialog_to_close(cx: &mut TestAppContext) {
    use gpui_kit::component::WindowExt as _;
    use gpui_kit::{InteractiveElement as _, ParentElement as _, TestSupportExt as _, div};
    let app = open(cx, memory_storage());
    let temp = tempfile::tempdir().unwrap();
    let fake = Fake::new(|_, _| Ok(redirect_to("v0.2.0")));
    allow_updates(&app, cx, fake, "0.1.0", Install::Page, temp.path());
    app.step(cx, |window, cx| {
        window.open_dialog(cx, |dialog, _, _| {
            dialog.title(
                div()
                    .id("other-dialog")
                    .test_support()
                    .child("Another question"),
            )
        })
    });
    assert!(app.has(cx, "other-dialog"));
    check(&app, cx, false);
    assert!(!update_dialog(&app, cx));
    assert!(app.has(cx, "other-dialog"));
    app.step(cx, |window, cx| window.close_dialog(cx));
    cx.executor().advance_clock(Duration::from_secs(1));
    app.step(cx, |_, _| ());
    assert!(!app.has(cx, "other-dialog"));
    assert!(update_dialog(&app, cx));
}

/// Every answer can be given from the keyboard, the harmless ones first: Tab goes to the
/// dialog's close button, then Later, What's new, Skip this version, and Update last.
#[gpui_kit::test]
fn the_keyboard_reaches_every_answer_of_the_dialog_later_first(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    let temp = tempfile::tempdir().unwrap();
    let answer = |app: &Fixture, cx: &mut TestAppContext, tabs: usize| {
        let latest = Fake::new(|_, _| Ok(redirect_to("v0.2.0")));
        next_run(app, cx, latest, Install::Page, temp.path());
        check(app, cx, false);
        assert!(update_dialog(app, cx));
        let mut keys = vec!["tab"; tabs];
        keys.push("space");
        app.press(cx, &keys);
    };
    // One Tab (the close button) and two (Later): closed, nothing stored or opened.
    for tabs in [1, 2] {
        answer(&app, cx, tabs);
        assert!(!app.dialog_open(cx), "{tabs}");
        assert_eq!(settings_now(cx).skipped_update, None);
        assert_eq!(cx.opened_url(), None);
    }
    // Three: What's new opens the release page and leaves the question on screen.
    answer(&app, cx, 3);
    assert!(update_dialog(&app, cx));
    assert!(cx.opened_url().is_some());
    app.press(cx, &["escape"]);
    // Five: Update, which for this copy opens the release page and closes.
    answer(&app, cx, 5);
    assert!(!app.dialog_open(cx));
    assert_eq!(settings_now(cx).skipped_update, None);
    // Four: Skip this version.
    answer(&app, cx, 4);
    assert!(!app.dialog_open(cx));
    assert_eq!(settings_now(cx).skipped_update.as_deref(), Some("0.2.0"));
}

#[gpui_kit::test]
fn keys_typed_as_the_dialog_arrives_neither_install_nor_skip(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    let temp = tempfile::tempdir().unwrap();
    let (_apps, bundle) = applications();
    let fake = release_runner(release(&sha256_hex(b"the release")));
    allow_updates(
        &app,
        cx,
        fake.clone(),
        "0.1.0",
        Install::App(bundle.clone()),
        temp.path(),
    );
    app.press(cx, &["secondary-t"]);
    app.type_text(cx, "select 1");
    check(&app, cx, false);
    assert!(update_dialog(&app, cx));
    app.press(cx, &["enter", "space", "enter"]);
    app.type_text(cx, " ");
    assert!(update_dialog(&app, cx));
    assert_eq!(curl_args(&fake).len(), 1, "only the check");
    assert_eq!(cx.opened_url(), None);
    assert_eq!(settings_now(cx).skipped_update, None);
    assert_eq!(phase(cx), Phase::Available(v("0.2.0")));
    assert_eq!(bundle_says(&bundle), "old");

    // Escape is "Later"; the keyboard goes back to the editor.
    app.press(cx, &["escape"]);
    assert!(!app.dialog_open(cx));
    assert_eq!(settings_now(cx).skipped_update, None);
    assert_eq!(curl_args(&fake).len(), 1);
    app.type_text(cx, " as one");
    let tab = app.tab(cx);
    assert_eq!(cx.update(|cx| tab.read(cx).sql(cx)), "select 1 as one");

    // An indented line typed into it at the next run (Tab, a word, Space): Tab reaches Later
    // first, so Space closes it as Later; nothing is skipped, opened or installed.
    allow_updates(
        &app,
        cx,
        fake.clone(),
        "0.1.0",
        Install::App(bundle.clone()),
        temp.path(),
    );
    check(&app, cx, false);
    assert!(update_dialog(&app, cx));
    app.press(cx, &["tab", "a", "n", "d", "space", "tab", "space"]);
    assert!(!app.dialog_open(cx));
    assert_eq!(curl_args(&fake).len(), 2, "only the two checks");
    assert_eq!(cx.opened_url(), None);
    assert_eq!(settings_now(cx).skipped_update, None);
    assert_eq!(phase(cx), Phase::Available(v("0.2.0")));
    assert_eq!(bundle_says(&bundle), "old");
}

/// K42: the About window's "Check for updates" says its answer there: checks switched off by
/// the environment or in Settings (no request), the latest version, a newer one with Update,
/// a failure.
#[gpui_kit::test]
fn the_about_window_checks_for_updates_and_says_the_answer(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    let temp = tempfile::tempdir().unwrap();
    app.dispatch(cx, actions::About);
    let ask = |app: &Fixture, cx: &mut TestAppContext| {
        app.tap(cx, "about-check-updates");
        app.wait_until(cx, "the answer", |app, cx| {
            app.has(cx, "about-update-state") && !paints(app, cx, "Checking…")
        });
    };
    // KERNO_NO_UPDATE_CHECK: no updater installed.
    ask(&app, cx);
    assert!(paints(
        &app,
        cx,
        "Update checks are off for this run: KERNO_NO_UPDATE_CHECK is set."
    ));

    let fake = Fake::new(|_, _| Ok(redirect_to("v0.2.0")));
    allow_updates(&app, cx, fake.clone(), "0.1.0", Install::Page, temp.path());
    cx.update(|cx| AppState::update_settings(cx, |settings| settings.check_updates = false));
    ask(&app, cx);
    assert!(paints(
        &app,
        cx,
        "Update checks are turned off in Settings ▸ Updates."
    ));
    assert!(curl_args(&fake).is_empty(), "nothing asked");

    cx.update(|cx| AppState::update_settings(cx, |settings| settings.check_updates = true));
    ask(&app, cx);
    assert!(paints(&app, cx, "Kerno 0.2.0 is available."));
    assert!(app.has(cx, "about-update"));
    assert_eq!(phase(cx), Phase::Available(v("0.2.0")));
    assert_eq!(checks(&fake), 1);

    let fake = Fake::new(|_, _| Ok(redirect_to("v0.1.0")));
    allow_updates(&app, cx, fake, "0.1.0", Install::Page, temp.path());
    ask(&app, cx);
    assert!(paints(&app, cx, "Kerno 0.1.0 is the latest version."));
    assert!(!app.has(cx, "about-update"));

    let fake = Fake::new(|_, _| Err("curl: (6) Could not resolve host: github.com".into()));
    allow_updates(&app, cx, fake, "0.1.0", Install::Page, temp.path());
    ask(&app, cx);
    assert!(paints(
        &app,
        cx,
        "Kerno could not reach GitHub to check (offline?): curl: (6) Could not resolve host"
    ));
}

/// K43: the dialog says what the newer version brings once its notes arrive; a failure leaves
/// it as it was; with checks off, the notes are never asked for.
#[gpui_kit::test]
fn the_dialog_shows_what_the_release_brings_once_its_notes_arrive(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    let temp = tempfile::tempdir().unwrap();
    let notes = std::fs::read(format!(
        "{}/../../docs/release-notes/v0.3.1.md",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let fake = Fake::new(move |_, args| {
        if args.iter().any(|arg| arg == "--head") {
            Ok(redirect_to("v0.2.0"))
        } else {
            Ok(notes.clone())
        }
    });
    allow_updates(&app, cx, fake.clone(), "0.1.0", Install::Page, temp.path());
    check(&app, cx, false);
    assert!(update_dialog(&app, cx));
    app.wait_until(cx, "the notes", |app, cx| app.has(cx, "update-whats-new"));
    assert!(paints(&app, cx, "Kerno 0.3.1 answers the first reports"));
    assert!(paints(&app, cx, "• A table's rows load as you scroll."));
    assert!(paints(&app, cx, "You have 0.1.0."));
    let asked = fake.calls();
    let (_, request) = asked
        .iter()
        .find(|(_, args)| is_notes(args))
        .expect("the notes asked for");
    assert_eq!(
        request.last().map(String::as_str),
        Some(
            "https://raw.githubusercontent.com/EvanPluchart/kerno/v0.2.0/docs/release-notes/v0.2.0.md"
        )
    );
    app.click(cx, "update-dialog-later");

    // A failure: the dialog as before, nothing said.
    let fake = Fake::new(|_, args| {
        if args.iter().any(|arg| arg == "--head") {
            Ok(redirect_to("v0.2.0"))
        } else {
            Err("curl: (28) Operation timed out".into())
        }
    });
    next_run(&app, cx, fake.clone(), Install::Page, temp.path());
    let before = notification_count(&app, cx);
    check(&app, cx, false);
    assert!(update_dialog(&app, cx));
    app.wait_until(cx, "the notes asked", |_, _| notes_requests(&fake) == 1);
    cx.run_until_parked();
    assert!(!app.has(cx, "update-whats-new"));
    assert!(paints(&app, cx, "You have 0.1.0."));
    assert_eq!(notification_count(&app, cx), before);
    app.click(cx, "update-dialog-later");

    // Checks off: Help ▸ Check for Updates… still asks, but no notes are read.
    let fake = Fake::new(|_, _| Ok(redirect_to("v0.2.0")));
    next_run(&app, cx, fake.clone(), Install::Page, temp.path());
    cx.update(|cx| AppState::update_settings(cx, |settings| settings.check_updates = false));
    check(&app, cx, true);
    cx.run_until_parked();
    assert_eq!(checks(&fake), 1);
    assert_eq!(notes_requests(&fake), 0);
}

/// The window becoming active again, as when the user comes back to it.
fn come_back(app: &Fixture, cx: &mut TestAppContext) {
    gpui_kit::VisualTestContext::from_window(app.window.into(), cx).deactivate_window();
    app.step(cx, |window, _| window.activate_window());
}

/// K57 (D-123): the timed checks follow the wall clock. A computer that slept six hours checks
/// within two minutes of waking (not at once: the network may not be back), the window becoming
/// active checks when a check is due, and never inside the interval; with checks off, nothing.
#[gpui_kit::test]
fn a_kerno_left_open_checks_after_six_hours_of_wall_time_even_through_a_sleep(
    cx: &mut TestAppContext,
) {
    let app = open(cx, memory_storage());
    let temp = tempfile::tempdir().unwrap();
    let fake = Fake::new(|_, _| Ok(redirect_to("v0.1.0")));
    allow_updates(&app, cx, fake.clone(), "0.1.0", Install::Page, temp.path());
    let slept = wall_clock(cx);
    let sleep = |by: u64| *slept.lock().unwrap() += Duration::from_secs(by);
    let workspace = app.workspace.clone();
    app.step(cx, |window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.start_update_checks(window, cx)
        })
    });
    let advance = |app: &Fixture, cx: &mut TestAppContext, by: Duration| {
        cx.executor().advance_clock(by);
        app.wait_until(cx, "the checks", |_, cx| {
            cx.update(|cx| !cx.global::<Updates>().checking)
        });
        checks(&fake)
    };
    assert_eq!(advance(&app, cx, crate::update::FIRST), 1);

    // Six hours asleep: the timer counted none of them. Not at the wake, a minute after.
    sleep(6 * 3600);
    assert_eq!(advance(&app, cx, crate::update::TICK), 1);
    assert_eq!(advance(&app, cx, crate::update::TICK), 2);
    // Five hours asleep, then the window comes back: inside the interval, nothing.
    sleep(5 * 3600);
    for _ in 0..3 {
        assert_eq!(advance(&app, cx, crate::update::TICK), 2);
    }
    come_back(&app, cx);
    assert_eq!(advance(&app, cx, Duration::ZERO), 2);
    // An hour more asleep, the window back at the wake: due, but a minute later.
    sleep(3600);
    come_back(&app, cx);
    assert_eq!(advance(&app, cx, Duration::ZERO), 2);
    assert_eq!(advance(&app, cx, crate::update::TICK), 3);

    // Awake six hours less half a minute: not yet. Half a minute later the window comes back
    // before the next look: it checks at once, and once.
    assert_eq!(
        advance(&app, cx, crate::update::EVERY - Duration::from_secs(30)),
        3
    );
    sleep(30);
    come_back(&app, cx);
    assert_eq!(advance(&app, cx, Duration::ZERO), 4);
    come_back(&app, cx);
    assert_eq!(advance(&app, cx, crate::update::TICK), 4);

    // Checks off in Settings: a day passes, nothing is asked. Turned on again: the next look.
    cx.update(|cx| AppState::update_settings(cx, |settings| settings.check_updates = false));
    sleep(24 * 3600);
    assert_eq!(advance(&app, cx, crate::update::TICK), 4);
    come_back(&app, cx);
    assert_eq!(advance(&app, cx, crate::update::TICK), 4);
    cx.update(|cx| AppState::update_settings(cx, |settings| settings.check_updates = true));
    assert_eq!(advance(&app, cx, crate::update::TICK), 5);
    assert_eq!(fake.calls().len(), 5, "only the checks");
}

/// K57: a newer version found after an older one was downloaded but not installed (the quit
/// question declined) replaces it, and its download is removed; the same version, or a
/// download under way, keeps its state.
#[gpui_kit::test]
fn a_newer_version_replaces_one_downloaded_but_not_installed(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    let temp = tempfile::tempdir().unwrap();
    let latest = |tag: &'static str| Fake::new(move |_, _| Ok(redirect_to(tag)));
    let folder = temp.path().join("kerno-update-1");
    std::fs::create_dir(&folder).unwrap();
    let file = folder.join("Kerno-0.2.0.dmg");
    std::fs::write(&file, "checked").unwrap();

    // The same version: the checked download stays for the next click.
    allow_updates(
        &app,
        cx,
        latest("v0.2.0"),
        "0.1.0",
        Install::Page,
        temp.path(),
    );
    set_phase(&app, cx, Phase::Ready(v("0.2.0"), file.clone()));
    check(&app, cx, false);
    assert_eq!(phase(cx), Phase::Ready(v("0.2.0"), file.clone()));
    assert!(file.is_file());
    app.press(cx, &["escape"]);

    // A newer one: offered, the old download gone.
    cx.update(|cx| cx.global_mut::<Updates>().runner = latest("v0.3.0"));
    check(&app, cx, false);
    assert_eq!(phase(cx), Phase::Available(v("0.3.0")));
    assert!(!folder.exists(), "the old download is removed");
    assert!(update_dialog(&app, cx));
    assert!(paints(&app, cx, "Kerno 0.3.0 is available"));
    assert_eq!(
        label(cx).as_deref(),
        Some("Kerno 0.3.0 is available · Update")
    );
    app.press(cx, &["escape"]);

    // A download under way keeps going.
    allow_updates(
        &app,
        cx,
        latest("v0.3.0"),
        "0.1.0",
        Install::Page,
        temp.path(),
    );
    set_phase(&app, cx, Phase::Downloading(v("0.2.0")));
    check(&app, cx, false);
    assert_eq!(phase(cx), Phase::Downloading(v("0.2.0")));
}

/// K57: the quit question of an install open, a newer version found meanwhile: answering it
/// installs nothing, and the item offers the newer one.
#[gpui_kit::test]
fn a_newer_version_found_during_the_quit_question_installs_nothing(cx: &mut TestAppContext) {
    if !testing::available(Engine::Postgres) {
        return;
    }
    let app = connected(cx, Engine::Postgres);
    let temp = tempfile::tempdir().unwrap();
    let (_apps, bundle) = applications();
    let fake = release_runner(release(&sha256_hex(b"the release")));
    allow_updates(
        &app,
        cx,
        fake.clone(),
        "0.1.0",
        Install::App(bundle.clone()),
        temp.path(),
    );
    app.press(cx, &["secondary-t"]);
    app.open_transaction(cx, "select 1 as kerno_ui_update_newer");
    set_phase(&app, cx, Phase::Available(v("0.2.0")));
    app.click(cx, "status-update-button");
    app.wait_until(cx, "the quit question", |app, cx| {
        app.has(cx, "end-question")
    });
    let Phase::Ready(_, file) = phase(cx) else {
        panic!("not ready: {:?}", phase(cx))
    };
    cx.update(|cx| cx.global_mut::<Updates>().runner = Fake::new(|_, _| Ok(redirect_to("v0.3.0"))));
    check(&app, cx, false);
    assert_eq!(phase(cx), Phase::Available(v("0.3.0")));
    assert!(!file.exists());
    app.click(cx, "end-rollback");
    app.wait_until(cx, "the rollback", |app, cx| {
        app.tx_state(cx) == kerno_core::TxState::Idle
    });
    cx.run_until_parked();
    assert_eq!(phase(cx), Phase::Available(v("0.3.0")));
    assert_eq!(bundle_says(&bundle), "old");
    assert!(
        !fake
            .calls()
            .iter()
            .any(|(program, _)| { program.ends_with("open") || program.ends_with("hdiutil") })
    );
}

/// A runner serving `files` to `curl` and recording the installer it is asked to start (Windows,
/// K70): everything but `curl` is that installer.
fn setup_runner(files: Vec<(String, Vec<u8>)>) -> Arc<Fake> {
    Fake::new(move |program, args| {
        if !is_curl(program) {
            return Ok(Vec::new());
        }
        let Some(output) = value_of(args, "--output") else {
            return Ok(redirect_to("v0.2.0"));
        };
        let name = args.last().unwrap().rsplit('/').next().unwrap();
        let (_, body) = files
            .iter()
            .find(|(file, _)| file == name)
            .ok_or("curl: (22) The requested URL returned error: 404")?;
        std::fs::write(output, body).unwrap();
        Ok(Vec::new())
    })
}

/// K70 (D-139), the Windows twin of `update_installs_the_checked_release_and_starts_it`: the
/// checked installer is started for this user and Kerno quits; the download stays for it, a note
/// stays for the next start, which says when the installer did not finish.
#[gpui_kit::test]
fn on_windows_update_starts_the_checked_installer_and_the_next_start_says_if_it_failed(
    cx: &mut TestAppContext,
) {
    let app = open(cx, memory_storage());
    let temp = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let exe = std::path::PathBuf::from(r"C:\Users\x\AppData\Local\Programs\Kerno\kerno.exe");
    let target = Install::Setup(exe.clone());
    let does = what_update_does(&target, v("0.2.0"));
    assert!(does.contains("installer for your account"), "{does}");
    let data_dir = |cx: &mut TestAppContext| {
        cx.update(|cx| cx.global_mut::<Updates>().data_dir = Some(data.path().to_path_buf()))
    };

    // A wrong checksum: nothing is started.
    let refused = setup_runner(release(&sha256_hex(b"another file")));
    allow_updates(
        &app,
        cx,
        refused.clone(),
        "0.1.0",
        target.clone(),
        temp.path(),
    );
    data_dir(cx);
    set_phase(&app, cx, Phase::Available(v("0.2.0")));
    app.click(cx, "status-update-button");
    app.wait_until(cx, "the refusal", |_, cx| {
        matches!(phase(cx), Phase::Failed(..))
    });
    assert!(refused.calls().iter().all(|(program, _)| is_curl(program)));
    assert!(!data.path().join(update::PENDING).exists());
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);

    let fake = setup_runner(release(&sha256_hex(b"the release")));
    allow_updates(&app, cx, fake.clone(), "0.1.0", target, temp.path());
    data_dir(cx);
    set_phase(&app, cx, Phase::Available(v("0.2.0")));
    app.click(cx, "status-update-button");
    app.wait_until(cx, "the installer started", |_, _| {
        fake.calls().iter().any(|(program, _)| !is_curl(program))
    });
    assert_eq!(phase(cx), Phase::Installing(v("0.2.0")));
    let (setup, args) = fake.calls().last().unwrap().clone();
    assert!(args.contains(&"/relaunch=1".to_string()), "{args:?}");
    assert!(
        args.contains(&format!("/kerno={}", exe.display())),
        "{args:?}"
    );
    // The installer reads its files from the download: it stays.
    cx.run_until_parked();
    assert!(setup.is_file(), "{setup:?}");
    assert!(data.path().join(update::PENDING).is_file());

    // The next start, still 0.1.0: it says so, once, and the download goes.
    let (version, why) = update::after_install(data.path(), "0.1.0", temp.path()).unwrap();
    assert!(!setup.exists());
    use gpui_kit::component::WindowExt as _;
    let before = app.step(cx, |window, cx| window.notifications(cx).len());
    app.step(cx, |window, cx| {
        crate::workspace::updates::not_installed(version, why, window, cx)
    });
    // One at a time (it replaces the refusal's above), and it stays until closed, a minute
    // later too.
    assert!(before >= 1);
    cx.executor().advance_clock(Duration::from_secs(60));
    cx.run_until_parked();
    assert_eq!(
        app.step(cx, |window, cx| window.notifications(cx).len()),
        before
    );
    assert_eq!(
        label(cx).as_deref(),
        Some("Kerno 0.2.0 was not installed · Release page")
    );
    assert_eq!(
        update::after_install(data.path(), "0.1.0", temp.path()),
        None
    );
}

/// K70 (V5): a failed install said at a start is not offered again by the dialog in that run; the
/// status bar item stays, and a newer version found later is offered as before.
#[gpui_kit::test]
fn a_failed_install_said_at_start_is_not_offered_again_by_the_dialog(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    let temp = tempfile::tempdir().unwrap();
    let latest = Arc::new(std::sync::Mutex::new("v0.2.0"));
    let tag = latest.clone();
    let fake = Fake::new(move |_, _| Ok(redirect_to(&tag.lock().unwrap())));
    allow_updates(&app, cx, fake, "0.1.0", Install::Page, temp.path());
    let before = notification_count(&app, cx);
    app.step(cx, |window, cx| {
        crate::workspace::updates::not_installed(v("0.2.0"), "it stopped".into(), window, cx)
    });
    // X1: the timed check finds the very version that failed: the item stays on "Release
    // page", as the notice says, and the notice stays.
    check(&app, cx, false);
    assert!(!update_dialog(&app, cx));
    assert_eq!(
        label(cx).as_deref(),
        Some("Kerno 0.2.0 was not installed · Release page")
    );
    assert_eq!(notification_count(&app, cx), before + 1);
    // A check asked for (the About window's) offers it again, and the failure notice goes.
    let workspace = app.workspace.clone();
    app.step(cx, |window, cx| {
        workspace.update(cx, |workspace, cx| workspace.check_from_about(window, cx))
    });
    app.wait_until(cx, "the check", |_, cx| {
        cx.update(|cx| !cx.try_global::<Updates>().is_some_and(|u| u.checking))
    });
    assert_eq!(
        label(cx).as_deref(),
        Some("Kerno 0.2.0 is available · Update")
    );
    // Once it has slid out.
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    assert_eq!(notification_count(&app, cx), before);
    // A newer version found by a timed check is offered by the dialog as before.
    *latest.lock().unwrap() = "v0.3.0";
    check(&app, cx, false);
    assert!(update_dialog(&app, cx));
}

/// K70 (V5): a zip copy whose profile has Kerno installed at the version found offers to start
/// that one, from the timed check and from the status bar item, and installs nothing.
#[gpui_kit::test]
fn a_zip_copy_offers_to_start_the_kerno_installed_in_the_profile(cx: &mut TestAppContext) {
    let app = open(cx, memory_storage());
    let temp = tempfile::tempdir().unwrap();
    let installed = std::path::PathBuf::from(r"C:\Users\x\AppData\Local\Programs\Kerno\kerno.exe");
    let fake = Fake::new(|_, _| Ok(redirect_to("v0.2.0")));
    allow_updates(
        &app,
        cx,
        fake.clone(),
        "0.1.0",
        Install::Setup("kerno.exe".into()),
        temp.path(),
    );
    let at = installed.clone();
    cx.update(|cx| cx.global_mut::<Updates>().installed = Some((v("0.2.0"), at)));
    let before = notification_count(&app, cx);
    check(&app, cx, false);
    assert!(!update_dialog(&app, cx));
    assert_eq!(notification_count(&app, cx), before + 1);
    // Said again, in place of the first.
    app.click(cx, "status-update-button");
    assert_eq!(notification_count(&app, cx), before + 1);
    // Only the check was sent: nothing downloaded.
    assert_eq!(curl_args(&fake).len(), 1);
    // The notification's button, once it has slid in.
    cx.executor().advance_clock(Duration::from_secs(1));
    app.click(cx, "update-start-installed");
    let (program, args) = fake.calls().last().unwrap().clone();
    assert_eq!((program, args), (installed, Vec::<String>::new()));
    assert_eq!(phase(cx), Phase::Available(v("0.2.0")));
}
