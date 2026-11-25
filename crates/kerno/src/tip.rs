//! The tooltips Kerno gives to plain elements through GPUI's `.tooltip(…)`
//! (rows of the tree, tabs, cells), outside dialogs only (K76).
//!
//! GPUI shows such a tooltip once the pointer has rested on its element, and
//! a click while it waits does not stop it: when the click opened a dialog,
//! the tooltip appeared over that dialog and stayed until the pointer left
//! the element's place, under the dialog. A tip from [`tip`] draws nothing
//! while a dialog is open, and once hidden that way it stays hidden. The
//! kit's own components (buttons, switches) manage their tooltips themselves.

use gpui_kit::base::TestTextExt as _;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::{
    AnyView, App, AppContext as _, Context, IntoElement, ParentElement as _, Render, SharedString,
    Window, div,
};

/// The builder `.tooltip(…)` takes, for an element outside dialogs.
pub fn tip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |window, cx| {
        let tooltip = Tooltip::new(text.clone()).build(window, cx);
        let text = text.clone();
        cx.new(|_| Tip {
            text,
            tooltip,
            hidden: false,
        })
        .into()
    }
}

struct Tip {
    text: SharedString,
    tooltip: AnyView,
    /// A dialog was open over the element: the tip is done.
    hidden: bool,
}

impl Render for Tip {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.hidden |= window.has_active_dialog(cx);
        if self.hidden {
            return div().into_any_element();
        }
        div()
            .child(self.tooltip.clone())
            .test_text(self.text.clone())
            .into_any_element()
    }
}
