//! `kerno <url>` in the window (CLAUDE.md §4 J4): the URL of the command
//! line, and those later `kerno <url>` runs hand over through the
//! instance socket ([`kerno_core::instance`]), open here. A URL may hold a
//! password: only its redacted form is ever logged.

use gpui_kit::{App, Entity, Window};
use kerno_core::config::{ParsedUrl, parse_connection_url};
use kerno_core::instance;

use crate::connections::manager::Connections;
use crate::workspace::Workspace;

/// Opens what `url` names (the saved connection to the same place, or an
/// unsaved one), shows it in the tree, and brings the window to the front.
pub fn open(
    workspace: &Entity<Workspace>,
    connections: &Entity<Connections>,
    url: &ParsedUrl,
    window: &mut Window,
    cx: &mut App,
) {
    let id = connections.update(cx, |connections, cx| connections.open_url(url, cx));
    let navigator = workspace.read(cx).navigator(cx);
    navigator.update(cx, |navigator, cx| {
        navigator.open_connection(&id, cx);
        navigator.select_connection(&id, cx);
    });
    window.activate_window();
    cx.activate(true);
}

/// Opens, in this window, each URL another `kerno <url>` hands over on
/// `listener`. The socket is accepted on a thread of its own; the window
/// opens the URLs one after the other.
pub fn listen(
    listener: instance::Listener,
    workspace: &Entity<Workspace>,
    connections: &Entity<Connections>,
    window: &mut Window,
    cx: &mut App,
) {
    let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
    let accepting = std::thread::Builder::new()
        .name("kerno-instance".into())
        .spawn(move || instance::serve(listener, |line| sent.send(line).is_ok()));
    if let Err(error) = accepting {
        tracing::warn!(%error, "URLs from other kerno runs cannot be received");
        return;
    }
    let (workspace, connections) = (workspace.downgrade(), connections.downgrade());
    window
        .spawn(cx, async move |cx| {
            while let Some(line) = received.recv().await {
                let (Some(workspace), Some(connections)) =
                    (workspace.upgrade(), connections.upgrade())
                else {
                    return;
                };
                let opened = cx.update(|window, cx| match parse_connection_url(&line) {
                    Ok(url) => {
                        tracing::info!(url = %instance::redact_url(&line), "opening a URL handed over");
                        open(&workspace, &connections, &url, window, cx);
                    }
                    Err(_) => tracing::warn!("a URL handed over by another kerno is not valid"),
                });
                if opened.is_err() {
                    return;
                }
            }
        })
        .detach();
}
