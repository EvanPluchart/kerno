//! The value viewer: one cell in full, as text, as indented JSON or as a
//! hex dump.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, ClipboardItem, Context, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    TestSupportExt as _, Window, div, px,
};
use kerno_core::format::{byte_size, hex_dump, pretty_json};
use kerno_core::{Column, TypeKind};

/// The most a view draws: beyond it the text is cut and says so (the
/// clipboard gets everything).
const MAX_SHOWN_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Text,
    Json,
    Hex,
}

struct ValueViewer {
    /// `None` for NULL.
    value: Option<Vec<u8>>,
    /// The value indented, when it is JSON.
    json: Option<String>,
    view: View,
    views: Vec<View>,
}

/// Opens the viewer on a cell of `column`.
pub fn open(column: &Column, value: Option<&[u8]>, window: &mut Window, cx: &mut App) {
    let title = SharedString::from(format!("{} · {}", column.name, column.type_name));
    let text = value.and_then(|bytes| std::str::from_utf8(bytes).ok());
    let json = text.and_then(pretty_json);
    let binary = column.kind == TypeKind::Binary || (value.is_some() && text.is_none());
    let mut views = Vec::new();
    if json.is_some() {
        views.push(View::Json);
    }
    if !binary {
        views.push(View::Text);
    }
    if value.is_some() {
        views.push(View::Hex);
    }
    let viewer = cx.new(|_| ValueViewer {
        value: value.map(<[u8]>::to_vec),
        json,
        view: views.first().copied().unwrap_or(View::Text),
        views,
    });
    window.open_dialog(cx, move |dialog, _, _| {
        let viewer = viewer.clone();
        dialog
            .title(title.clone())
            .w(px(720.))
            .content(move |content, _, _| content.child(viewer.clone()))
    });
}

impl ValueViewer {
    /// What the current view shows, and what "Copy" copies.
    fn shown(&self) -> String {
        match (&self.value, self.view) {
            (None, _) => String::new(),
            (Some(_), View::Json) => self.json.clone().unwrap_or_default(),
            (Some(bytes), View::Hex) => hex_dump(&bytes[..bytes.len().min(MAX_SHOWN_BYTES / 4)]),
            (Some(bytes), View::Text) => String::from_utf8_lossy(bytes).into_owned(),
        }
    }
}

impl Render for ValueViewer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let selected = self
            .views
            .iter()
            .position(|view| *view == self.view)
            .unwrap_or(0);
        let text = self.shown();
        // Cut on a character boundary.
        let cut = (text.len() > MAX_SHOWN_BYTES).then(|| {
            let mut end = MAX_SHOWN_BYTES;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            end
        });
        let body = match (&self.value, cut) {
            (None, _) => div()
                .italic()
                .text_color(theme.muted_foreground)
                .child("NULL")
                .into_any_element(),
            (Some(_), Some(end)) => div()
                .child(SharedString::from(text[..end].to_string()))
                .into_any_element(),
            (Some(_), None) => div()
                .child(SharedString::from(text.clone()))
                .into_any_element(),
        };
        let size = self.value.as_ref().map(|bytes| bytes.len() as u64);
        v_flex()
            .gap_3()
            .pt_1()
            .when(self.views.len() > 1, |this| {
                this.child(
                    TabBar::new("value-views")
                        .underline()
                        .selected_index(selected)
                        .on_click(cx.listener(|this, index: &usize, _, cx| {
                            if let Some(view) = this.views.get(*index) {
                                this.view = *view;
                                cx.notify();
                            }
                        }))
                        .children(self.views.iter().map(|view| {
                            Tab::new().label(match view {
                                View::Text => "Text",
                                View::Json => "JSON",
                                View::Hex => "Hex",
                            })
                        })),
                )
            })
            .child(
                div()
                    .id("value-text")
                    .test_support()
                    .h(px(360.))
                    .p_2()
                    .overflow_y_scroll()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.muted)
                    .font_family(theme.mono_font_family.clone())
                    .text_sm()
                    .child(body),
            )
            .child(
                h_flex()
                    .justify_between()
                    .child(div().text_xs().text_color(theme.muted_foreground).child(
                        SharedString::from(match (size, cut) {
                            (None, _) => "No value".to_string(),
                            (Some(size), None) => byte_size(size),
                            (Some(size), Some(_)) => format!(
                                "{}; the first {} are shown, Copy takes all of it",
                                byte_size(size),
                                byte_size(MAX_SHOWN_BYTES as u64)
                            ),
                        }),
                    ))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(Button::new("value-copy").outline().label("Copy").on_click(
                                cx.listener(|this, _, _, cx| {
                                    let all = match (&this.value, this.view) {
                                        (Some(bytes), View::Hex) => hex_dump(bytes),
                                        _ => this.shown(),
                                    };
                                    cx.write_to_clipboard(ClipboardItem::new_string(all));
                                }),
                            ))
                            .child(
                                Button::new("value-close")
                                    .primary()
                                    .label("Close")
                                    .on_click(|_, window, cx| window.close_dialog(cx)),
                            ),
                    ),
            )
    }
}
