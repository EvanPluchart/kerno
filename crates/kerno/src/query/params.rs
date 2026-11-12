//! The prompt for the parameters of a statement (`:name`, `$1`, `?`),
//! shown before it runs.

use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    App, AppContext as _, Context, Entity, Focusable as _, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Window, div, px,
};
use kerno_core::sql::ParamValue;

/// Runs with the values, in the order of the labels.
type Run = dyn Fn(Vec<ParamValue>, &mut Window, &mut App);

struct Field {
    label: String,
    input: Entity<InputState>,
    null: bool,
}

struct ParamsPrompt {
    fields: Vec<Field>,
    run: Rc<Run>,
}

/// Asks for a value per label (`remembered` holds what was typed last
/// time), then calls `run` with them.
pub fn open(
    labels: Vec<String>,
    remembered: Vec<String>,
    window: &mut Window,
    cx: &mut App,
    run: impl Fn(Vec<ParamValue>, &mut Window, &mut App) + 'static,
) {
    let view = cx.new(|cx| ParamsPrompt {
        fields: labels
            .into_iter()
            .zip(remembered)
            .map(|(label, value)| Field {
                label,
                input: cx.new(|cx| InputState::new(window, cx).default_value(value)),
                null: false,
            })
            .collect(),
        run: Rc::new(run),
    });
    let first = view
        .read(cx)
        .fields
        .first()
        .map(|field| field.input.clone());
    let confirm = view.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let (view, confirm) = (view.clone(), confirm.clone());
        dialog
            .title("Parameters")
            .w(px(520.))
            // Enter in a field runs with the values typed.
            .on_ok(move |_, window, cx| {
                confirm.update(cx, |prompt, cx| prompt.submit(window, cx));
                true
            })
            .content(move |content, _, _| content.child(view.clone()))
    });
    if let Some(first) = first {
        window.defer(cx, move |window, cx| {
            first.read(cx).focus_handle(cx).focus(window, cx)
        });
    }
}

impl ParamsPrompt {
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let values = self
            .fields
            .iter()
            .map(|field| {
                if field.null {
                    ParamValue::Null
                } else {
                    ParamValue::Auto(field.input.read(cx).value().to_string())
                }
            })
            .collect();
        (self.run)(values, window, cx);
    }
}

impl Render for ParamsPrompt {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let mono = theme.mono_font_family.clone();
        let muted = theme.muted_foreground;
        v_flex()
            .gap_3()
            .pt_1()
            .child(div().text_sm().text_color(muted).child(
                "A number or true/false is written as it is; anything else becomes a string. \
                 Tick NULL for no value.",
            ))
            .children(self.fields.iter().enumerate().map(|(index, field)| {
                h_flex()
                    .gap_3()
                    .child(
                        div()
                            .w(px(150.))
                            .flex_none()
                            .truncate()
                            .text_sm()
                            .font_family(mono.clone())
                            .child(SharedString::from(field.label.clone())),
                    )
                    .child(
                        Input::new(&field.input)
                            .id(("param", index))
                            .flex_1()
                            .disabled(field.null),
                    )
                    .child(
                        Checkbox::new(("param-null", index))
                            .label("NULL")
                            .checked(field.null)
                            .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                                this.fields[index].null = *checked;
                                cx.notify();
                            })),
                    )
            }))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("params-cancel")
                            .ghost()
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("params-run")
                            .primary()
                            .label("Run")
                            .on_click(cx.listener(|this, _, window, cx| {
                                window.close_dialog(cx);
                                this.submit(window, cx);
                            })),
                    ),
            )
    }
}
