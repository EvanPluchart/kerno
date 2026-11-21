//! The query library: the history of what was run, and the queries saved
//! under a name. One dialog, two lists, a search box.

use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, TestSupportExt as _, Window, div, px,
};
use kerno_core::storage::{HistoryEntry, HistoryStatus, SavedQuery, now_ms};

use crate::bridge;
use crate::connections::manager::Connections;
use crate::state::AppState;
use crate::tabs::Target;

/// How many history entries the list shows.
const HISTORY_LIMIT: u32 = 300;

/// The list shown first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shelf {
    History,
    Saved,
}

/// Opens the chosen query in a tab: title, text, where it runs.
type Open = dyn Fn(String, String, Option<Target>, &mut Window, &mut App);

pub struct Library {
    connections: Entity<Connections>,
    shelf: Shelf,
    search: Entity<InputState>,
    /// The connection of the tab the library was opened from.
    connection: Option<String>,
    /// Only what belongs to that connection is listed.
    only_connection: bool,
    history: Vec<HistoryEntry>,
    saved: Vec<SavedQuery>,
    /// The entry whose removal waits for a second click: `(shelf, id)`;
    /// id 0 on the history shelf is "clear everything listed".
    removing: Option<(Shelf, i64)>,
    loading: Option<Task<()>>,
    open: Rc<Open>,
    _subscription: Subscription,
}

impl Library {
    /// Opens the library on `shelf`. `connection` is the connection of
    /// the active tab, to narrow the lists to it.
    pub fn open(
        shelf: Shelf,
        connection: Option<String>,
        connections: Entity<Connections>,
        window: &mut Window,
        cx: &mut App,
        open: impl Fn(String, String, Option<Target>, &mut Window, &mut App) + 'static,
    ) {
        let view = cx.new(|cx| {
            let search =
                cx.new(|cx| InputState::new(window, cx).placeholder("Search the statements"));
            let subscription =
                cx.subscribe(&search, |this: &mut Self, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.reload(cx);
                    }
                });
            let mut library = Self {
                connections,
                shelf,
                search,
                only_connection: connection.is_some(),
                connection,
                history: Vec::new(),
                saved: Vec::new(),
                removing: None,
                loading: None,
                open: Rc::new(open),
                _subscription: subscription,
            };
            library.reload(cx);
            library
        });
        let search = view.read(cx).search.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            let view = view.clone();
            dialog
                .title("Queries")
                .w(px(760.))
                .on_ok(|_, _, _| false)
                .content(move |content, _, _| content.child(view.clone()))
        });
        window.defer(cx, move |window, cx| {
            search.read(cx).focus_handle(cx).focus(window, cx)
        });
    }

    /// Reads both lists again from the store, off the UI thread.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let storage = AppState::get(cx).storage.clone();
        let search = self.search.read(cx).value().to_string();
        let connection = self.connection.clone().filter(|_| self.only_connection);
        let reading = bridge::spawn_blocking(cx, move || {
            let history = storage.history(connection.as_deref(), &search, HISTORY_LIMIT)?;
            let needle = search.to_lowercase();
            let saved = storage
                .saved_queries()?
                .into_iter()
                .filter(|query| {
                    connection.as_ref().is_none_or(|connection| {
                        query
                            .connection_id
                            .as_ref()
                            .is_none_or(|own| own == connection)
                    })
                })
                .filter(|query| {
                    needle.is_empty()
                        || query.name.to_lowercase().contains(&needle)
                        || query.folder.to_lowercase().contains(&needle)
                        || query.sql.to_lowercase().contains(&needle)
                })
                .collect::<Vec<_>>();
            Ok::<_, kerno_core::Error>((history, saved))
        });
        self.loading = Some(cx.spawn(async move |this, cx| {
            let outcome = reading.await;
            let _ = this.update(cx, |this, cx| {
                if let Ok((history, saved)) = outcome {
                    this.history = history;
                    this.saved = saved;
                }
                this.loading = None;
                cx.notify();
            });
        }));
    }

    /// Removes after a second click on the same button.
    fn remove(&mut self, shelf: Shelf, id: i64, cx: &mut Context<Self>) {
        if self.removing != Some((shelf, id)) {
            self.removing = Some((shelf, id));
            cx.notify();
            return;
        }
        self.removing = None;
        let storage = AppState::get(cx).storage.clone();
        let connection = self.connection.clone().filter(|_| self.only_connection);
        let removing = bridge::spawn_blocking(cx, move || match shelf {
            Shelf::Saved => storage.delete_query(id),
            Shelf::History => storage.clear_history(connection.as_deref()),
        });
        cx.spawn(async move |this, cx| {
            let _ = removing.await;
            let _ = this.update(cx, |this, cx| this.reload(cx));
        })
        .detach();
    }

    fn connection_name(&self, id: &str, cx: &App) -> String {
        self.connections.read(cx).get(id).map_or_else(
            || "a deleted connection".to_string(),
            |saved| saved.config.name.clone(),
        )
    }

    fn pick(
        &self,
        title: String,
        sql: String,
        target: Option<Target>,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.close_dialog(cx);
        (self.open)(title, sql, target, window, cx);
    }
}

/// A statement on one line, to tell it from the others in a list.
fn one_line(sql: &str) -> String {
    let mut line = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() > 160 {
        line = line.chars().take(159).collect::<String>() + "…";
    }
    line
}

/// How long ago, in words short enough for a list.
pub fn ago(now_ms: i64, then_ms: i64) -> String {
    let seconds = ((now_ms - then_ms) / 1000).max(0);
    match seconds {
        0..=9 => "just now".into(),
        10..=59 => format!("{seconds} s ago"),
        60..=3_599 => format!("{} min ago", seconds / 60),
        3_600..=86_399 => format!("{} h ago", seconds / 3_600),
        _ => format!("{} d ago", seconds / 86_400),
    }
}

impl Render for Library {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let now = now_ms();
        let mono = theme.mono_font_family.clone();
        let muted = theme.muted_foreground;
        let row = |id: (&'static str, usize)| {
            h_flex()
                .id(id)
                .test_support()
                .px_2()
                .py_1p5()
                .gap_3()
                .rounded(theme.radius)
                .cursor_pointer()
                .hover(|style| style.bg(theme.list_hover))
        };
        let list: Vec<gpui_kit::AnyElement> = match self.shelf {
            Shelf::History => self
                .history
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    let (color, word) = match entry.status {
                        HistoryStatus::Ok => (theme.success, "ok"),
                        HistoryStatus::Error => (theme.danger, "failed"),
                        HistoryStatus::Cancelled => (theme.warning, "cancelled"),
                    };
                    let target = Target {
                        connection: entry.connection_id.clone(),
                        database: entry.database.clone(),
                        schema: None,
                    };
                    let sql = entry.sql.clone();
                    row(("history", index))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.pick(
                                "History".into(),
                                sql.clone(),
                                Some(target.clone()),
                                window,
                                cx,
                            )
                        }))
                        .child(div().flex_none().size(px(8.)).rounded_full().bg(color))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .truncate()
                                        .text_sm()
                                        .font_family(mono.clone())
                                        .child(SharedString::from(one_line(&entry.sql))),
                                )
                                .child(div().truncate().text_xs().text_color(muted).child(
                                    SharedString::from(match (&entry.error, entry.row_count) {
                                        (Some(error), _) => format!("{word}: {}", one_line(error)),
                                        (None, Some(rows)) => format!("{word} · {rows} rows"),
                                        (None, None) => word.to_string(),
                                    }),
                                )),
                        )
                        .child(
                            v_flex()
                                .flex_none()
                                .items_end()
                                .text_xs()
                                .text_color(muted)
                                .child(SharedString::from(format!(
                                    "{} · {} ms",
                                    ago(now, entry.started_at_ms),
                                    entry.duration_ms
                                )))
                                .child(SharedString::from(
                                    self.connection_name(&entry.connection_id, cx),
                                )),
                        )
                        .into_any_element()
                })
                .collect(),
            Shelf::Saved => self
                .saved
                .iter()
                .enumerate()
                .map(|(index, query)| {
                    let target = query.connection_id.clone().map(|connection| Target {
                        connection,
                        database: None,
                        schema: None,
                    });
                    let (name, sql, id) = (query.name.clone(), query.sql.clone(), query.id);
                    let confirming = self.removing == Some((Shelf::Saved, id));
                    row(("saved", index))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.pick(name.clone(), sql.clone(), target.clone(), window, cx)
                        }))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .text_sm()
                                        .children((!query.folder.is_empty()).then(|| {
                                            div().flex_none().text_color(muted).child(
                                                SharedString::from(format!("{} /", query.folder)),
                                            )
                                        }))
                                        .child(
                                            div()
                                                .truncate()
                                                .child(SharedString::from(query.name.clone())),
                                        ),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_xs()
                                        .font_family(mono.clone())
                                        .text_color(muted)
                                        .child(SharedString::from(one_line(&query.sql))),
                                ),
                        )
                        .child(
                            Button::new(("saved-delete", index))
                                .ghost()
                                .xsmall()
                                .label(if confirming {
                                    "Click again to delete"
                                } else {
                                    "Delete"
                                })
                                .when(confirming, |button| button.danger())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.remove(Shelf::Saved, id, cx);
                                })),
                        )
                        .into_any_element()
                })
                .collect(),
        };
        let empty = list.is_empty();
        let searching = !self.search.read(cx).value().trim().is_empty();
        let clearing = self.removing == Some((Shelf::History, 0));
        v_flex()
            .gap_3()
            .pt_1()
            .child(
                TabBar::new("library-shelves")
                    .underline()
                    .selected_index(if self.shelf == Shelf::History { 0 } else { 1 })
                    .on_click(cx.listener(|this, index: &usize, _, cx| {
                        this.shelf = if *index == 0 { Shelf::History } else { Shelf::Saved };
                        this.removing = None;
                        cx.notify();
                    }))
                    .child(Tab::new().label("History"))
                    .child(Tab::new().label("Saved queries")),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(Input::new(&self.search).id("library-search").flex_1().cleanable(true))
                    .when(self.connection.is_some(), |this| {
                        this.child(
                            Checkbox::new("library-only-connection")
                                .label("This tab's connection only")
                                .checked(self.only_connection)
                                .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                    this.only_connection = *checked;
                                    this.reload(cx);
                                })),
                        )
                    }),
            )
            .child(
                v_flex()
                    .id("library-list")
                    .test_support()
                    .h(px(380.))
                    .overflow_y_scroll()
                    .children(list)
                    .when(empty, |this| {
                        this.child(div().p_3().text_sm().text_color(muted).child(
                            match (self.shelf, searching) {
                                (_, true) => "Nothing matches the search.",
                                (Shelf::History, false) => "Statements you run are listed here, newest first.",
                                (Shelf::Saved, false) => {
                                    "No saved query yet. “Save Query…” in the command palette keeps the \
                                     text of a tab under a name."
                                }
                            },
                        ))
                    }),
            )
            .child(
                h_flex()
                    .justify_between()
                    .child(div().when(self.shelf == Shelf::History && !self.history.is_empty(), |this| {
                        this.child(
                            Button::new("library-clear-history")
                                .ghost()
                                .small()
                                .label(if clearing {
                                    "Click again to clear the history"
                                } else {
                                    "Clear history"
                                })
                                .when(clearing, |button| button.danger())
                                .on_click(cx.listener(|this, _, _, cx| this.remove(Shelf::History, 0, cx))),
                        )
                    }))
                    .child(
                        Button::new("library-close")
                            .outline()
                            .label("Close")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    ),
            )
    }
}

/// Asks for the name and folder of a query to save, then calls `save`.
pub fn ask_name(
    suggested: String,
    window: &mut Window,
    cx: &mut App,
    save: impl Fn(String, String, &mut Window, &mut App) + 'static,
) {
    let name = cx.new(|cx| {
        InputState::new(window, cx)
            .placeholder("Monthly revenue")
            .default_value(suggested)
    });
    let folder = cx.new(|cx| {
        InputState::new(window, cx).placeholder("Folder (optional), like reports/finance")
    });
    let save = Rc::new(save);
    let focus = name.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let (name, folder, save) = (name.clone(), folder.clone(), save.clone());
        let submit = {
            let (name, folder, save) = (name.clone(), folder.clone(), save.clone());
            Rc::new(move |window: &mut Window, cx: &mut App| {
                let typed = name.read(cx).value().trim().to_string();
                if typed.is_empty() {
                    return false;
                }
                let folder = folder.read(cx).value().trim().trim_matches('/').to_string();
                save(typed, folder, window, cx);
                true
            })
        };
        let on_enter = submit.clone();
        dialog
            .title("Save query")
            .w(px(460.))
            .on_ok(move |_, window, cx| on_enter(window, cx))
            .content(move |content, _, _| {
                let submit = submit.clone();
                content.child(
                    v_flex()
                        .gap_3()
                        .pt_1()
                        .child(Input::new(&name).id("save-query-name"))
                        .child(Input::new(&folder).id("save-query-folder"))
                        .child(
                            h_flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    Button::new("save-query-cancel")
                                        .ghost()
                                        .label("Cancel")
                                        .on_click(|_, window, cx| window.close_dialog(cx)),
                                )
                                .child(
                                    Button::new("save-query-confirm")
                                        .primary()
                                        .label("Save")
                                        .on_click(move |_, window, cx| {
                                            if submit(window, cx) {
                                                window.close_dialog(cx);
                                            }
                                        }),
                                ),
                        ),
                )
            })
    });
    window.defer(cx, move |window, cx| {
        focus.read(cx).focus_handle(cx).focus(window, cx)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_told_in_the_largest_unit_that_fits() {
        let now = 10_000_000;
        for (seconds, text) in [
            (0, "just now"),
            (9, "just now"),
            (45, "45 s ago"),
            (60, "1 min ago"),
            (3_599, "59 min ago"),
            (7_200, "2 h ago"),
            (200_000, "2 d ago"),
        ] {
            assert_eq!(ago(now, now - seconds * 1000), text);
        }
        // A clock that went backwards is not "in the future".
        assert_eq!(ago(now, now + 5_000), "just now");
    }

    #[test]
    fn a_statement_is_shown_on_one_line() {
        assert_eq!(one_line("select 1,\n   2\n from t"), "select 1, 2 from t");
        assert_eq!(one_line(&"x".repeat(400)).chars().count(), 160);
    }
}
