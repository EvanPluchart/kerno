//! The progress of Execute script (CLAUDE.md §4 J9, Beekeeper #702,
//! #2325): the file is fed to the client through its standard input, the
//! wizard shows how much of it was sent, and Cancel stops it at once.

use std::time::Instant;

use kerno_core::tools::ClientFamily;

use super::tool_wizards::{fake_client, paints};
use super::*;
use crate::backup_dialog::Mode;

#[gpui_kit::test]
fn a_script_shows_how_much_was_sent_and_cancels_at_once(cx: &mut TestAppContext) {
    if !super::tool_wizards::fake_tools_run() {
        return;
    }
    if !testing::available(Engine::MySql) {
        return;
    }
    let app = connected(cx, Engine::MySql);
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    // A client that reads its script slowly, a line every 10 ms.
    let client = fake_client(
        &root,
        "client",
        ClientFamily::MySql,
        "mysql  Ver 9.4.0 for macos15 on arm64 (Homebrew)",
        "echo started; while IFS= read -r line; do sleep 0.01; done",
    );
    app.use_client(cx, ClientFamily::MySql, &client);
    let script = root.join("big.sql");
    let line = format!("INSERT INTO t VALUES ('{}');\n", "x".repeat(990));
    std::fs::write(&script, line.repeat(4000)).unwrap();

    let dialog = app
        .wizard(cx, "kerno", Mode::ExecuteScript)
        .expect("the script wizard");
    app.wait_backup(cx, &dialog, &["choose a file"]);
    app.tap(cx, "restore-choose-file");
    app.answer_open_prompt(cx, &script);
    let state = app.wait_backup(cx, &dialog, &["ready", "no tool", "refused"]);
    assert!(state.starts_with("ready: "), "{state}");
    app.tap(cx, "restore-start");
    app.wait_until(cx, "the confirmation", |app, cx| app.has(cx, "sql-preview"));
    app.tap(cx, "sql-confirm");

    // Part of it sent, and the page says how much.
    app.wait_until(cx, "part of the script sent", |_, cx| {
        cx.update(|cx| dialog.read(cx).fed())
            .is_some_and(|(sent, total)| sent > 0 && sent < total)
    });
    let total = line.len() * 4000;
    assert_eq!(
        cx.update(|cx| dialog.read(cx).fed())
            .map(|(_, total)| total),
        Some(total as u64)
    );
    let of = format!(" of {} sent (", kerno_core::format::byte_size(total as u64));
    let progress = cx
        .update(|cx| dialog.read(cx).progress())
        .expect("a progress");
    assert!(
        progress.ends_with(" %)") && progress.contains(&of),
        "{progress}"
    );
    assert!(app.has(cx, "backup-progress"));
    assert!(paints(&app, cx, &of));

    // The bench times the stop (`scripts/bench.sh --only import`); here it
    // only has to come, well before the 40 s the client would take.
    let cancelled = Instant::now();
    app.tap(cx, "backup-cancel");
    let state = app.wait_backup(cx, &dialog, &["done", "failed"]);
    assert!(state.contains("Cancelled"), "{state}");
    assert!(
        cancelled.elapsed() < Duration::from_secs(10),
        "{:?}",
        cancelled.elapsed()
    );
    assert!(!app.has(cx, "backup-progress"));
}
