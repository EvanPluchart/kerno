//! "Configure…" of the tree's Filter ▸ menu: the masks of the objects of one
//! kind the tree shows and hides (DBeaver's "Configure filter" dialog,
//! `EditObjectFilterDialog`), one mask per line.

use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, TestSupportExt as _, Window, div, px,
};
use kerno_core::config::ObjectFilter;

/// Receives the filter saved; `None` when it holds no mask.
type OnSave = dyn Fn(Option<ObjectFilter>, &mut Window, &mut App);

pub struct FilterDialog {
    include: Entity<TextareaState>,
    exclude: Entity<TextareaState>,
    enabled: bool,
    on_save: Rc<OnSave>,
}

/// Opens the masks of the objects of a kind (`kind` reads "tables").
pub fn open(
    kind: &str,
    filter: ObjectFilter,
    window: &mut Window,
    cx: &mut App,
    on_save: impl Fn(Option<ObjectFilter>, &mut Window, &mut App) + 'static,
) -> Entity<FilterDialog> {
    let view = cx.new(|cx| {
        let lines = |masks: &[String]| masks.join("\n");
        let mut area = |text: String, placeholder: &'static str, cx: &mut Context<FilterDialog>| {
            cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(3, 8)
                    .placeholder(placeholder)
                    .default_value(text)
            })
        };
        FilterDialog {
            include: area(lines(&filter.include), "every one: leave empty", cx),
            exclude: area(lines(&filter.exclude), "tmp_*", cx),
            enabled: filter.enabled,
            on_save: Rc::new(on_save),
        }
    });
    let title = SharedString::from(format!("Filter of the {kind}"));
    let shown = view.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let view = shown.clone();
        dialog
            .title(title.clone())
            .w(px(480.))
            .on_ok(|_, _, _| false)
            .content(move |content, _, _| content.child(view.clone()))
    });
    view
}

impl FilterDialog {
    /// The filter as typed: one mask per line, blank lines left out.
    pub fn filter(&self, cx: &App) -> ObjectFilter {
        let masks = |area: &Entity<TextareaState>| {
            area.read(cx)
                .value()
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect()
        };
        ObjectFilter {
            include: masks(&self.include),
            exclude: masks(&self.exclude),
            enabled: self.enabled,
        }
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let filter = self.filter(cx);
        let empty = filter.include.is_empty() && filter.exclude.is_empty();
        window.close_dialog(cx);
        (self.on_save)((!empty).then_some(filter), window, cx);
    }
}

impl Render for FilterDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let labelled = |label: &'static str, area: &Entity<TextareaState>, id: &'static str| {
            v_flex()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
                .child(div().id(id).test_support().child(Textarea::new(area)))
        };
        v_flex()
            .id("filter-dialog")
            .test_support()
            .gap_3()
            .pt_1()
            .child(div().text_sm().child(
                "One name or mask per line, without regard to case: * stands for any \
                 characters, ? for one. An object is shown when it matches a line to show \
                 (or there is none) and no line to hide.",
            ))
            .child(labelled("Show only", &self.include, "filter-include"))
            .child(labelled("Hide", &self.exclude, "filter-exclude"))
            .child(
                Checkbox::new("filter-enabled")
                    .label("Apply this filter")
                    .checked(self.enabled)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.enabled = *checked;
                        cx.notify();
                    })),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("filter-cancel")
                            .ghost()
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("filter-save")
                            .primary()
                            .label("Save")
                            .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                    ),
            )
    }
}
