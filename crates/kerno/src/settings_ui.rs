//! The pieces Settings is drawn with (K25, D-098): the frame of a page, its
//! sections and panels of rows, the side list of sections, and option
//! cards, the choice among a few visual options with a drawn preview each.
//!
//! The scale, used everywhere in Settings: 4 px inside a control, 8 px
//! between the lines of a row or a section's head and its content, 12 px
//! between cards and the columns of the dialog, 20 px between sections. Text:
//! a page's name at 1.25× the interface size, semibold; a section's name at
//! the row size, semibold; rows at the small size; descriptions, hints and
//! override lines extra small, muted. Motion: 120 ms (the kit's fast
//! duration), colours and opacity only, nothing moves on its own; with the
//! system's "reduce motion" every change lands at once (GPUI's animations
//! and Base's transitions read it).

use std::rc::Rc;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::base::{
    Radio as BaseRadio, RadioGroup as BaseRadioGroup, TestTextExt as _, Transition, transition,
};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, ThemeStyled as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Animation, AnimationExt as _, AnyElement, App, Axis, ClickEvent, Div, FocusHandle, FontWeight,
    Hsla, InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, ParentElement as _,
    Pixels, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _,
    TestSupportExt as _, Window, div, ease_out_quint, px, relative, rgb,
};

use crate::settings::{TabLayout, TableDoubleClick};
use crate::theme::{self, Appearance};

/// The width of the Settings dialog; the kit narrows it to a small window.
pub const DIALOG_WIDTH: f32 = 860.;
/// The side list of sections.
pub const NAV_WIDTH: f32 = 184.;
/// The tallest Settings gets: the search, the pages and the footer.
const MAX_HEIGHT: f32 = 560.;
/// What the dialog draws around Settings (its title, paddings and the kit's
/// margin to the window's edges), with room to spare: a short window
/// shortens the pages, which scroll, and never cuts the footer.
const DIALOG_CHROME: f32 = 140.;

/// The height of Settings in this window: the same for every page.
pub fn height(window: &Window) -> Pixels {
    let available = window.viewport_size().height - px(DIALOG_CHROME);
    available.min(px(MAX_HEIGHT)).max(px(240.))
}

/// The kit's fast duration (120 ms): a card's colours, its check, a page.
fn fast(cx: &App) -> Duration {
    cx.theme().motion_tokens().duration_fast
}

/// A page's head: its name, and one line on what it holds.
pub fn page_head(title: &'static str, intro: &'static str, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    v_flex()
        .gap_1()
        .child(
            div()
                .text_size(theme.font_size * 1.25)
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .when(!intro.is_empty(), |head| {
            head.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(intro),
            )
        })
}

/// A section's name, the one heading style of every page.
pub fn section_title(text: impl Into<SharedString>) -> Div {
    div().font_weight(FontWeight::SEMIBOLD).child(text.into())
}

/// One bordered panel holding rows, a line between two of them.
pub fn panel(rows: Vec<AnyElement>, cx: &App) -> Div {
    let theme = cx.theme();
    v_flex()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.border)
        .bg(theme.background)
        .children(rows.into_iter().enumerate().map(|(index, row)| {
            div()
                .when(index > 0, |line| {
                    line.border_t_1().border_color(theme.border)
                })
                .child(row)
        }))
}

/// One section of the side list (its index, glyph and name), the selected
/// one on the accent wash; a focusable element that Enter and Space click.
pub fn nav_item(
    (index, icon, label): (usize, IconName, &'static str),
    selected: bool,
    focus: &FocusHandle,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    window: &Window,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let (bg, hover) = (theme.list_active, theme.list_hover);
    div()
        .id(("settings-section", index))
        .test_support()
        .track_focus(focus)
        .relative()
        .flex()
        .items_center()
        .gap_2()
        .px_2()
        .py_1p5()
        .rounded(theme.radius)
        .cursor_pointer()
        .map(|item| {
            if selected {
                item.bg(bg).font_weight(FontWeight::MEDIUM)
            } else {
                item.hover(move |item| item.bg(hover))
            }
        })
        .child(Icon::new(icon).size_4().text_color(if selected {
            theme.primary
        } else {
            theme.muted_foreground
        }))
        .child(div().flex_1().min_w_0().truncate().child(label))
        .when(focus.is_focused(window), |item| {
            item.focus_ring_style(window, cx)
        })
        // A click does not take the keyboard from the search, nor draw a
        // focus ring: Tab does.
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .on_click(on_click)
}

type OnChange = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// A choice among a few options drawn as cards side by side, each with a
/// preview, its name and a check when it is the one chosen (K25). A radio
/// group to the keyboard and to assistive technology: one Tab stop (the
/// chosen card), the arrows move the choice, Space and Enter choose. The
/// group's id and each card's index are the element ids, as the kit's radio
/// group gives them.
#[derive(IntoElement)]
pub struct OptionCards {
    id: &'static str,
    selected: Option<usize>,
    choices: Vec<(SharedString, AnyElement)>,
    on_change: OnChange,
}

impl OptionCards {
    pub fn new(
        id: &'static str,
        selected: Option<usize>,
        on_change: impl Fn(usize, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id,
            selected,
            choices: Vec::new(),
            on_change: Rc::new(on_change),
        }
    }

    /// One more card.
    pub fn option(mut self, label: impl Into<SharedString>, preview: impl IntoElement) -> Self {
        self.choices
            .push((label.into(), preview.into_any_element()));
        self
    }
}

/// The id of a card's check, present while the card is the one chosen.
pub fn check_id(group: &str, index: usize) -> SharedString {
    SharedString::from(format!("{group}-check-{index}"))
}

impl RenderOnce for OptionCards {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self {
            id,
            selected,
            choices,
            on_change,
        } = self;
        let count = choices.len();
        let keyed = |what: &str, index: usize| SharedString::from(format!("{id}-{what}-{index}"));
        let handles: Rc<Vec<FocusHandle>> = Rc::new(
            (0..count)
                .map(|index| {
                    window
                        .use_keyed_state(keyed("focus", index), cx, |_, cx| cx.focus_handle())
                        .read(cx)
                        .clone()
                })
                .collect(),
        );
        // One Tab stop: the chosen card, else the first.
        let stop = selected.unwrap_or(0);
        let cards: Vec<AnyElement> = choices
            .into_iter()
            .enumerate()
            .map(|(index, (label, preview))| {
                let hovered = window.use_keyed_state(keyed("hover", index), cx, |_, _| false);
                let state = CardState {
                    checked: selected == Some(index),
                    hovered: *hovered.read(cx),
                    focused: handles[index].is_focused(window),
                };
                let (border, bg) = state.colors(cx);
                let policy = Transition::new(fast(cx));
                let border = transition(keyed("border", index), border, policy.clone(), window, cx);
                let bg = transition(keyed("bg", index), bg, policy, window, cx);
                let on_change = on_change.clone();
                let theme = cx.theme();
                BaseRadio::new(index)
                    .checked(state.checked)
                    .track_focus(&handles[index])
                    .tab_stop(index == stop)
                    .set_position(index + 1, count)
                    .accessibility_label(label.clone())
                    .on_change(move |_, _, window, cx| on_change(index, window, cx))
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .on_hover(move |inside, window, cx| {
                        hovered.update(cx, |hovered, _| *hovered = *inside);
                        window.refresh();
                    })
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_2()
                    .rounded(theme.radius_lg)
                    .border_2()
                    .border_color(border)
                    .bg(bg)
                    .cursor_pointer()
                    .child(
                        div()
                            .h(px(64.))
                            .w_full()
                            .rounded(theme.radius)
                            .overflow_hidden()
                            .child(preview),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(check(id, index, state.checked, cx))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(label.clone())
                                    .test_text(label),
                            ),
                    )
                    .when(state.focused, |card| card.focus_ring_style(window, cx))
                    .into_any_element()
            })
            .collect();
        BaseRadioGroup::new(id)
            .axis(Axis::Horizontal)
            .on_key_down(move |event: &KeyDownEvent, window, cx| {
                let step: isize = match event.keystroke.key.as_str() {
                    "left" | "up" => -1,
                    "right" | "down" => 1,
                    _ => return,
                };
                let modifiers = event.keystroke.modifiers;
                if modifiers.platform || modifiers.control || modifiers.alt || modifiers.shift {
                    return;
                }
                cx.stop_propagation();
                let current = (handles.iter())
                    .position(|handle| handle.is_focused(window))
                    .or(selected)
                    .unwrap_or(0);
                let next = (current as isize + step).rem_euclid(count as isize) as usize;
                handles[next].focus(window, cx);
                on_change(next, window, cx);
            })
            .flex()
            .w_full()
            .gap_3()
            // Room for the focus ring, which is drawn outside the card.
            .p_1()
            .children(cards)
    }
}

/// What a card shows of its state.
struct CardState {
    checked: bool,
    hovered: bool,
    focused: bool,
}

impl CardState {
    /// The frame and the fill: the accent's frame on its wash when chosen
    /// (with the check, never colour alone), a stronger frame and a tint
    /// under the pointer.
    fn colors(&self, cx: &App) -> (Hsla, Hsla) {
        let theme = cx.theme();
        if self.checked {
            (theme.primary, theme.accent)
        } else if self.hovered || self.focused {
            (theme.input, theme.list_hover)
        } else {
            (theme.border, theme.background)
        }
    }
}

/// The round mark of a card: empty, or the accent with a check that fades
/// in when the card is chosen.
fn check(group: &str, index: usize, checked: bool, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let mark = div()
        .size(px(16.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1();
    if !checked {
        return mark.border_color(theme.input).into_any_element();
    }
    mark.id(check_id(group, index))
        .test_support()
        .border_color(theme.primary)
        .bg(theme.primary)
        .child(
            Icon::new(IconName::Check)
                .xsmall()
                .text_color(theme.primary_foreground),
        )
        .with_animation(
            check_id(group, index),
            Animation::new(fast(cx)).with_easing(ease_out_quint()),
            |mark, delta| mark.opacity(delta),
        )
        .into_any_element()
}

/// A short line, as text is drawn in a preview.
fn line(width: f32, color: impl Into<Hsla>) -> Div {
    div()
        .h(px(3.))
        .w(relative(width))
        .rounded_full()
        .bg(color.into())
}

/// A window of the light or the dark theme in miniature: its title bar, its
/// side list and a page with lines and the accent, in that theme's colours.
fn mini_window(dark: bool) -> Div {
    let colors = theme::preview(dark);
    v_flex()
        .size_full()
        .bg(rgb(colors.window))
        .child(
            div()
                .h(px(9.))
                .flex_none()
                .bg(rgb(colors.bar))
                .border_b_1()
                .border_color(rgb(colors.border)),
        )
        .child(
            h_flex()
                .flex_1()
                .items_stretch()
                .child(
                    v_flex()
                        .w(relative(0.3))
                        .flex_none()
                        .gap(px(4.))
                        .p(px(5.))
                        .bg(rgb(colors.sidebar))
                        .border_r_1()
                        .border_color(rgb(colors.border))
                        .child(line(0.8, rgb(colors.muted)))
                        .child(div().h(px(5.)).rounded_sm().bg(rgb(colors.selection)))
                        .child(line(0.6, rgb(colors.muted))),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .gap(px(4.))
                        .p(px(6.))
                        .child(line(0.7, rgb(colors.text)))
                        .child(line(0.5, rgb(colors.muted)))
                        .child(line(0.6, rgb(colors.muted)))
                        .child(
                            h_flex().justify_end().child(
                                div()
                                    .w(px(18.))
                                    .h(px(6.))
                                    .rounded_sm()
                                    .bg(rgb(colors.accent)),
                            ),
                        ),
                ),
        )
}

/// The preview of a theme choice: a light window, a dark one, or for
/// "Follow system" the light window's left half beside the dark one's right
/// half.
pub fn theme_preview(appearance: Appearance, cx: &App) -> AnyElement {
    let frame = div()
        .size_full()
        .rounded(cx.theme().radius)
        .overflow_hidden()
        .border_1()
        .border_color(cx.theme().border);
    match appearance {
        Appearance::Light => frame.child(mini_window(false)),
        Appearance::Dark => frame.child(mini_window(true)),
        Appearance::System => frame.child(
            h_flex()
                .size_full()
                .child(
                    div()
                        .w(relative(0.5))
                        .h_full()
                        .overflow_hidden()
                        .child(mini_window(false).w(relative(2.))),
                )
                .child(
                    div()
                        .w(relative(0.5))
                        .h_full()
                        .overflow_hidden()
                        .child(mini_window(true).w(relative(2.)).ml(relative(-1.))),
                ),
        ),
    }
    .into_any_element()
}

/// A drawing surface for the diagrams below, in the current theme.
fn sketch(cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .size_full()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .bg(theme.background)
        .overflow_hidden()
}

/// A tab in a diagram: the open one in the accent.
fn tab_pill(active: bool, cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .w(px(20.))
        .h(px(6.))
        .flex_none()
        .rounded_sm()
        .bg(if active { theme.primary } else { theme.border })
}

/// The page under the tabs of a diagram.
fn page_lines(cx: &App) -> Div {
    let muted = cx.theme().muted_foreground.opacity(0.5);
    v_flex()
        .flex_1()
        .gap(px(4.))
        .p(px(6.))
        .child(line(0.7, muted))
        .child(line(0.5, muted))
        .child(line(0.6, muted))
}

/// The preview of a layout of the tabs: one strip, two strips, or a list at
/// the side.
pub fn tabs_preview(layout: TabLayout, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let strip = |first: usize, cx: &App| {
        h_flex()
            .gap(px(3.))
            .px(px(5.))
            .py(px(3.))
            .bg(theme.muted)
            .border_b_1()
            .border_color(theme.border)
            .children((0..4).map(move |index| tab_pill(first + index == 0, cx)))
    };
    match layout {
        TabLayout::SingleRow => sketch(cx).child(
            v_flex()
                .size_full()
                .child(strip(0, cx))
                .child(page_lines(cx)),
        ),
        TabLayout::Rows => sketch(cx).child(
            v_flex()
                .size_full()
                .child(strip(0, cx))
                .child(strip(4, cx))
                .child(page_lines(cx)),
        ),
        TabLayout::Vertical => sketch(cx).child(
            h_flex()
                .size_full()
                .items_stretch()
                .child(
                    v_flex()
                        .w(relative(0.32))
                        .flex_none()
                        .gap(px(3.))
                        .p(px(5.))
                        .bg(theme.muted)
                        .border_r_1()
                        .border_color(theme.border)
                        .children((0..4).map(|index| tab_pill(index == 0, cx).w_full())),
                )
                .child(page_lines(cx)),
        ),
    }
    .into_any_element()
}

/// The preview of what a double click on a table opens: its rows (a grid),
/// its structure (its columns, the key first), or its node in the tree.
pub fn double_click_preview(choice: TableDoubleClick, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let (border, muted, accent) = (
        theme.border,
        theme.muted_foreground.opacity(0.5),
        theme.primary,
    );
    match choice {
        TableDoubleClick::Data => {
            sketch(cx).child(v_flex().size_full().children((0..5).map(move |row| {
                h_flex()
                    .flex_1()
                    .when(row == 0, |header| header.bg(theme.muted))
                    .when(row > 0, |cells| cells.border_t_1().border_color(border))
                    .children((0..3).map(move |column| {
                        div()
                            .flex_1()
                            .h_full()
                            .flex()
                            .items_center()
                            .px(px(4.))
                            .when(column > 0, |cell| cell.border_l_1().border_color(border))
                            .child(line(if column == 0 { 0.5 } else { 0.8 }, muted))
                    }))
            })))
        }
        TableDoubleClick::Structure => sketch(cx).child(
            v_flex()
                .size_full()
                .gap(px(5.))
                .p(px(6.))
                .children((0..4).map(move |row| {
                    h_flex()
                        .gap(px(5.))
                        .child(div().size(px(6.)).flex_none().rounded_sm().bg(if row == 0 {
                            accent
                        } else {
                            border
                        }))
                        .child(line(0.35, muted))
                        .child(line(0.25, muted))
                })),
        ),
        TableDoubleClick::Expand => {
            sketch(cx).child(v_flex().size_full().gap(px(5.)).p(px(6.)).children(
                [(0, true), (1, false), (1, false), (1, false)].map(move |(depth, open)| {
                    h_flex()
                        .gap(px(4.))
                        .pl(px(depth as f32 * 12.))
                        .child(div().size(px(6.)).flex_none().rounded_sm().bg(if open {
                            accent
                        } else {
                            border
                        }))
                        .child(line(if open { 0.4 } else { 0.3 }, muted))
                }),
            ))
        }
    }
    .into_any_element()
}
