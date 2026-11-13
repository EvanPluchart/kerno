//! "Filter by distinct values" (DBeaver's `core.resultset.filterMenu.distinct`,
//! `valuefilter/`): the values of a column with how many rows hold each, the
//! most frequent first, searchable; the ones picked become a filter.

use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, TestSupportExt as _, Window, div, px,
};
use kerno_core::exec::Collected;

/// How many values are listed, the most frequent first.
pub const LIMIT: u64 = 200;

/// The values of a listing (`None` is NULL) and how many rows hold each.
pub fn values(listed: &Collected) -> Vec<(Option<String>, u64)> {
    // At most `LIMIT` rows: owned strings are fine here.
    listed.results.first().map_or_else(Vec::new, |result| {
        result
            .rows()
            .into_iter()
            .map(|mut row| {
                let count = row
                    .get(1)
                    .cloned()
                    .flatten()
                    .and_then(|count| count.parse().ok())
                    .unwrap_or(0);
                (row.swap_remove(0), count)
            })
            .collect()
    })
}

/// Receives the values picked.
type OnPick = dyn Fn(Vec<Option<String>>, &mut Window, &mut App);

pub struct DistinctDialog {
    values: Vec<(Option<String>, u64)>,
    picked: Vec<bool>,
    search: Entity<InputState>,
    on_pick: Rc<OnPick>,
    _search: Subscription,
}

/// Lists the `values` of `column` to pick from.
pub fn open(
    column: &str,
    values: Vec<(Option<String>, u64)>,
    window: &mut Window,
    cx: &mut App,
    on_pick: impl Fn(Vec<Option<String>>, &mut Window, &mut App) + 'static,
) -> Entity<DistinctDialog> {
    let view = cx.new(|cx| {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search the values"));
        let typing = cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        DistinctDialog {
            picked: vec![false; values.len()],
            values,
            search,
            on_pick: Rc::new(on_pick),
            _search: typing,
        }
    });
    let capped = view.read(cx).values.len() as u64 >= LIMIT;
    let title = SharedString::from(format!(
        "Values of {column}{}",
        if capped {
            format!(" (the {LIMIT} most frequent)")
        } else {
            String::new()
        }
    ));
    let (shown, focus) = (view.clone(), view.read(cx).search.clone());
    window.open_dialog(cx, move |dialog, _, _| {
        let view = shown.clone();
        dialog
            .title(title.clone())
            .w(px(440.))
            .on_ok(|_, _, _| false)
            .content(move |content, _, _| content.child(view.clone()))
    });
    window.defer(cx, move |window, cx| {
        focus.read(cx).focus_handle(cx).focus(window, cx)
    });
    view
}

impl DistinctDialog {
    /// The values the search keeps, by index.
    fn shown(&self, cx: &App) -> Vec<usize> {
        let query = self.search.read(cx).value().trim().to_lowercase();
        (0..self.values.len())
            .filter(|&index| {
                query.is_empty()
                    || self.values[index]
                        .0
                        .as_deref()
                        .unwrap_or("NULL")
                        .to_lowercase()
                        .contains(&query)
            })
            .collect()
    }

    /// Picks the value at `index`, or lets it go.
    pub fn toggle(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(picked) = self.picked.get_mut(index) {
            *picked = !*picked;
            cx.notify();
        }
    }

    /// Filters on the values picked.
    pub fn apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked: Vec<Option<String>> = self
            .values
            .iter()
            .zip(&self.picked)
            .filter(|(_, picked)| **picked)
            .map(|((value, _), _)| value.clone())
            .collect();
        if picked.is_empty() {
            return;
        }
        window.close_dialog(cx);
        (self.on_pick)(picked, window, cx);
    }
}

impl Render for DistinctDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let shown = self.shown(cx);
        let any = self.picked.iter().any(|picked| *picked);
        let rows = shown.iter().map(|&index| {
            let (value, count) = &self.values[index];
            let label = format!("{}  ({count})", value.as_deref().unwrap_or("NULL"));
            div().id(("distinct-value", index)).test_support().child(
                Checkbox::new(("distinct-check", index))
                    .w_full()
                    .label(SharedString::from(label))
                    .checked(self.picked[index])
                    .on_click(cx.listener(move |this, _: &bool, _, cx| this.toggle(index, cx))),
            )
        });
        v_flex()
            .id("distinct-dialog")
            .test_support()
            .gap_3()
            .pt_1()
            .child(Input::new(&self.search).id("distinct-search"))
            .child(
                v_flex()
                    .id("distinct-values")
                    .gap_1()
                    .max_h(px(320.))
                    .overflow_y_scroll()
                    .children(rows)
                    .when(shown.is_empty(), |list| {
                        list.child(
                            div()
                                .text_sm()
                                .text_color(muted)
                                .child("No value matches the search."),
                        )
                    }),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("distinct-cancel")
                            .ghost()
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("distinct-apply")
                            .primary()
                            .label("Filter")
                            .disabled(!any)
                            .on_click(cx.listener(|this, _, window, cx| this.apply(window, cx))),
                    ),
            )
    }
}
