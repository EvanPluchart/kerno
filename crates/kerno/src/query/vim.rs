//! Vim mode in the SQL editor (CLAUDE.md §4 J19; DBeaver #37897, #8219,
//! Beekeeper #317), when Settings ▸ Editor turns it on: the keys go to
//! [`kerno_core::vim::Vim`] first in Normal and Visual mode, and its
//! effects are applied through the editor's own API. Off, none of this
//! exists: the query tab holds no [`VimEditor`] and installs no handler.
//!
//! What is left to the editor in every mode: the arrows, Home/End, the Page
//! keys, the mouse, and every key with ⌘ (Ctrl elsewhere) — Beekeeper's vim
//! mode broke the arrows (#1621, #1636). In Insert mode every key but `Esc`
//! is the editor's (completion, Tab, Enter included).

use gpui_kit::base::input::{self, RangeDecorationStyle};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::input::{EditorState, Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, Focusable as _,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, SharedString,
    Styled as _, Subscription, TestSupportExt as _, WeakEntity, Window, div, px,
};
use kerno_core::sql::LexRules;
use kerno_core::vim::{Buffer, Effect, Key, Mode, Vim};

use super::tab::QueryTab;
use crate::actions::{CloseTab, RefreshData, SaveFile};
use crate::state::AppState;

/// Vim's state in one query tab, and what it draws.
pub struct VimEditor {
    vim: Vim,
    /// The editor's text, read again after a change only: Normal mode's
    /// keys do not copy a large script each.
    text: Option<String>,
    /// The block over the character under Normal mode's cursor (the kit
    /// draws a one-pixel caret only).
    block: input::RangeDecorationCollection,
    shown_block: Option<std::ops::Range<usize>>,
    /// The one-line field of `:` and `/`.
    prompt: Option<Prompt>,
    /// `:wq` waits for the file to be written before closing the tab.
    close_after_save: bool,
}

struct Prompt {
    kind: char,
    input: Entity<InputState>,
    _subscription: Subscription,
}

impl VimEditor {
    fn new(editor: &Entity<EditorState>, cx: &mut App) -> Self {
        let block = editor.update(cx, |editor, cx| {
            editor.create_range_decorations_collection(Vec::new(), cx)
        });
        Self {
            vim: Vim::new(),
            text: None,
            block,
            shown_block: None,
            prompt: None,
            close_after_save: false,
        }
    }

    pub fn mode(&self) -> Mode {
        self.vim.mode()
    }

    /// The editor's text changed: read it again when it is needed.
    pub fn text_changed(&mut self) {
        self.text = None;
    }

    fn text(&mut self, editor: &Entity<EditorState>, cx: &App) -> &str {
        let rope = editor.read(cx).text();
        if self
            .text
            .as_ref()
            .is_none_or(|text| text.len() != rope.len())
        {
            self.text = Some(rope.to_string());
        }
        self.text.as_deref().unwrap_or_default()
    }

    /// Draws the block where Normal mode's cursor is (the editor notifies
    /// at every blink: nothing is set when it stays the same).
    pub fn refresh_block(&mut self, editor: &Entity<EditorState>, cx: &mut App) {
        let (cursor, selected) = {
            let state = editor.read(cx);
            (state.cursor(), !state.selected_range().is_empty())
        };
        let block = if self.vim.mode() == Mode::Normal && !selected {
            let text = self.text(editor, cx).to_owned();
            self.vim.block(&text, cursor)
        } else {
            None
        };
        if block == self.shown_block {
            return;
        }
        self.shown_block = block.clone();
        let color = cx.theme().caret.opacity(0.35);
        let marks = block
            .map(|range| {
                input::RangeDecoration::new(range)
                    .with_style(RangeDecorationStyle::Fill)
                    .with_color(color)
            })
            .into_iter()
            .collect();
        self.block.set(marks, cx);
    }

    /// Gives `key` to vim, with the editor's text, cursor and the
    /// clipboard (the unnamed register).
    fn handle(
        &mut self,
        key: Key,
        editor: &Entity<EditorState>,
        rules: LexRules,
        cx: &App,
    ) -> Vec<Effect> {
        let cursor = editor.read(cx).cursor();
        let clipboard = cx.read_from_clipboard().and_then(|item| item.text());
        let text = self.text(editor, cx).to_owned();
        let buffer = Buffer {
            text: &text,
            cursor,
            clipboard: clipboard.as_deref(),
            rules,
        };
        self.vim.key(key, &buffer)
    }
}

/// Turns vim on or off in `tab` as the setting says.
pub fn follow_setting(tab: &mut QueryTab, cx: &mut App) {
    let on = AppState::get(cx).settings.vim_mode;
    if on == tab.vim.is_some() {
        return;
    }
    if let Some(vim) = tab.vim.take() {
        vim.block.dispose(cx);
    } else {
        tab.vim = Some(VimEditor::new(tab.editor(), cx));
    }
}

/// The editor with vim's key handlers around it and the command field
/// under it; called only while vim is on.
pub fn render(
    tab: &mut QueryTab,
    editor: impl IntoElement,
    window: &mut Window,
    cx: &mut Context<QueryTab>,
) -> AnyElement {
    // `:wq`: the tab closes once its file is written.
    let written = tab.file().is_some() && !tab.unsaved();
    if let Some(vim) = tab.vim.as_mut()
        && vim.close_after_save
        && written
    {
        vim.close_after_save = false;
        window.dispatch_action(Box::new(CloseTab), cx);
    }
    let prompt = tab.vim.as_ref().and_then(|vim| {
        let prompt = vim.prompt.as_ref()?;
        Some((prompt.kind, prompt.input.clone()))
    });
    let keys = div()
        .id("vim-editor")
        .test_support()
        .flex_1()
        .min_h_0()
        .capture_key_down(cx.listener(on_key_down))
        // The editor's bindings take these keys before a key handler sees
        // them: in Normal and Visual mode they are vim's.
        .capture_action(cx.listener(|tab, _: &input::Escape, window, cx| {
            // Not stopped: the kit closes its completion list too.
            if editor_focused(tab, window, cx) {
                run(tab, Key::Escape, window, cx);
            }
        }))
        .capture_action(cx.listener(|tab, action: &input::Enter, window, cx| {
            if !action.secondary && commands(tab, window, cx) {
                cx.stop_propagation();
                run(tab, Key::Enter, window, cx);
            }
        }))
        .capture_action(cx.listener(|tab, _: &input::Backspace, window, cx| {
            if commands(tab, window, cx) {
                cx.stop_propagation();
                run(tab, Key::Backspace, window, cx);
            }
        }))
        .capture_action(cx.listener(|tab, _: &input::Delete, window, cx| {
            if commands(tab, window, cx) {
                cx.stop_propagation();
                run(tab, Key::Char('x'), window, cx);
            }
        }))
        .capture_action(cx.listener(|tab, _: &input::IndentInline, window, cx| {
            if commands(tab, window, cx) {
                cx.stop_propagation();
            }
        }))
        .capture_action(cx.listener(|tab, _: &input::OutdentInline, window, cx| {
            if commands(tab, window, cx) {
                cx.stop_propagation();
            }
        }))
        // Ctrl+R is Refresh Rows where Ctrl is the command key (Linux): in
        // Normal mode it is vim's redo. On macOS it is ⌘R, and ⌃R reaches
        // the key handler.
        .capture_action(cx.listener(|tab, _: &RefreshData, window, cx| {
            if !cfg!(target_os = "macos")
                && tab
                    .vim
                    .as_ref()
                    .is_some_and(|vim| vim.mode() == Mode::Normal)
                && editor_focused(tab, window, cx)
            {
                cx.stop_propagation();
                run(tab, Key::Ctrl('r'), window, cx);
            }
        }))
        .child(editor);
    v_flex()
        .size_full()
        .child(keys)
        .when_some(prompt, |this, (kind, input)| {
            this.child(
                h_flex()
                    .id("vim-command-line")
                    .test_support()
                    .flex_none()
                    .px_2()
                    .gap_1()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .font_family(cx.theme().mono_font_family.clone())
                    .capture_action(cx.listener(|tab, _: &input::Escape, window, cx| {
                        cx.stop_propagation();
                        close_prompt(tab, window, cx);
                    }))
                    .child(div().w(px(10.)).child(SharedString::from(kind.to_string())))
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&input).small().appearance(false).bordered(false)),
                    ),
            )
        })
        .into_any_element()
}

fn editor_focused(tab: &QueryTab, window: &Window, cx: &App) -> bool {
    tab.editor().read(cx).focus_handle(cx).is_focused(window)
}

/// Whether keys are vim's commands now: Normal or Visual mode, in the
/// editor itself (not its find field).
fn commands(tab: &QueryTab, window: &Window, cx: &App) -> bool {
    tab.vim
        .as_ref()
        .is_some_and(|vim| vim.mode() != Mode::Insert)
        && editor_focused(tab, window, cx)
}

fn on_key_down(
    tab: &mut QueryTab,
    event: &KeyDownEvent,
    window: &mut Window,
    cx: &mut Context<QueryTab>,
) {
    if !commands(tab, window, cx) {
        return;
    }
    let keystroke = &event.keystroke;
    let modifiers = keystroke.modifiers;
    // ⌘ (and Fn) keys stay the app's and the editor's.
    if modifiers.platform || modifiers.function {
        return;
    }
    let key = if modifiers.control {
        match keystroke.key.as_str() {
            "r" => Key::Ctrl('r'),
            _ => return,
        }
    } else {
        // The character typed, Shift and Option included: `$ % { [` come
        // with Option on some layouts (French AZERTY).
        let mut chars = keystroke.key_char.as_deref().unwrap_or_default().chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) if !c.is_control() => Key::Char(c),
            _ => return,
        }
    };
    cx.stop_propagation();
    run(tab, key, window, cx);
}

/// Gives `key` to vim and applies what it says.
fn run(tab: &mut QueryTab, key: Key, window: &mut Window, cx: &mut Context<QueryTab>) {
    let editor = tab.editor().clone();
    let rules = tab.lex_rules(cx);
    let Some(vim) = tab.vim.as_mut() else {
        return;
    };
    let effects = vim.handle(key, &editor, rules, cx);
    apply(effects, cx.entity().downgrade(), window, cx);
}

/// Applies vim's effects once the key's handling is over (nothing of the
/// tab is borrowed then), then redraws the block and the status bar.
fn apply(effects: Vec<Effect>, tab: WeakEntity<QueryTab>, window: &mut Window, cx: &mut App) {
    window.defer(cx, move |window, cx| {
        let Some(tab) = tab.upgrade() else {
            return;
        };
        let editor = tab.read(cx).editor().clone();
        for effect in effects {
            match effect {
                Effect::Move(at) => editor.update(cx, |editor, cx| {
                    editor.set_selected_range(at..at, cx);
                }),
                Effect::Select { range, reversed } => editor.update(cx, |editor, cx| {
                    editor.set_selected_range_with_direction(range, reversed, cx);
                }),
                // One undoable edit of the kit: the selection replaced.
                Effect::Replace { range, text } => editor.update(cx, |editor, cx| {
                    editor.set_selected_range(range, cx);
                    editor.replace(text, window, cx);
                }),
                Effect::Undo | Effect::Redo => {
                    let focus = editor.read(cx).focus_handle(cx);
                    if effect == Effect::Undo {
                        focus.dispatch_action(&input::Undo, window, cx);
                    } else {
                        focus.dispatch_action(&input::Redo, window, cx);
                    }
                    // The kit gives back the selection the edit started
                    // from: Normal mode shows a cursor.
                    editor.update(cx, |editor, cx| {
                        let at = editor.selected_range().start;
                        editor.set_selected_range(at..at, cx);
                    });
                }
                Effect::Yank(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
                Effect::Prompt(kind) => {
                    tab.update(cx, |tab, cx| open_prompt(tab, kind, window, cx));
                }
                Effect::Write { then_close } => {
                    if then_close {
                        tab.update(cx, |tab, _| {
                            if let Some(vim) = tab.vim.as_mut() {
                                vim.close_after_save = true;
                            }
                        });
                    }
                    window.dispatch_action(Box::new(SaveFile), cx);
                }
                Effect::Quit { force } => {
                    // `:q!` forgets the unsaved text; an open transaction
                    // or pending changes still ask (CLAUDE.md §1).
                    if force {
                        tab.update(cx, |tab, cx| tab.forget_unsaved(cx));
                    }
                    window.dispatch_action(Box::new(CloseTab), cx);
                }
                Effect::Error(message) => {
                    window.push_notification(Notification::warning(SharedString::from(message)), cx)
                }
            }
        }
        tab.update(cx, |tab, cx| {
            if let Some(vim) = tab.vim.as_mut() {
                vim.refresh_block(&editor, cx);
            }
            cx.notify();
        });
        // The status bar shows the mode.
        window.refresh();
    });
}

fn open_prompt(tab: &mut QueryTab, kind: char, window: &mut Window, cx: &mut Context<QueryTab>) {
    let input = cx.new(|cx| InputState::new(window, cx));
    let subscription = cx.subscribe_in(
        &input,
        window,
        |tab, input, event: &InputEvent, window, cx| {
            match event {
                InputEvent::PressEnter { .. } => {
                    let line = input.read(cx).value().to_string();
                    let Some(kind) = tab
                        .vim
                        .as_ref()
                        .and_then(|vim| Some(vim.prompt.as_ref()?.kind))
                    else {
                        return;
                    };
                    close_prompt(tab, window, cx);
                    let editor = tab.editor().clone();
                    let rules = tab.lex_rules(cx);
                    let Some(vim) = tab.vim.as_mut() else {
                        return;
                    };
                    let cursor = editor.read(cx).cursor();
                    let text = vim.text(&editor, cx).to_owned();
                    let buffer = Buffer {
                        text: &text,
                        cursor,
                        clipboard: None,
                        rules,
                    };
                    let effects = vim.vim.command(kind, &line, &buffer);
                    apply(effects, cx.entity().downgrade(), window, cx);
                }
                // A click elsewhere closes it, as Escape does.
                InputEvent::Blur => {
                    if let Some(vim) = tab.vim.as_mut()
                        && vim.prompt.take().is_some()
                    {
                        cx.notify();
                    }
                }
                _ => {}
            }
        },
    );
    input.update(cx, |input, cx| input.focus(window, cx));
    if let Some(vim) = tab.vim.as_mut() {
        vim.prompt = Some(Prompt {
            kind,
            input,
            _subscription: subscription,
        });
    }
    cx.notify();
}

/// Closes the command field and gives the keyboard back to the editor.
fn close_prompt(tab: &mut QueryTab, window: &mut Window, cx: &mut Context<QueryTab>) {
    if let Some(vim) = tab.vim.as_mut() {
        vim.prompt = None;
    }
    let editor = tab.editor().clone();
    editor.update(cx, |editor, cx| editor.focus(window, cx));
    cx.notify();
}

#[cfg(test)]
impl QueryTab {
    /// The range Normal mode's block covers; for tests.
    pub fn vim_block(&self) -> Option<std::ops::Range<usize>> {
        self.vim.as_ref()?.shown_block.clone()
    }
}
