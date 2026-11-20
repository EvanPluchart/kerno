//! Settings ▸ Shortcuts (K20): every command of `actions::COMMANDS` with
//! its shortcuts, to change (click a shortcut, then press the new one), add,
//! remove or give back their defaults. Each change applies at once
//! (`actions::apply_keymap`) and is written to the keymap file in the
//! background, the rest of that file kept; a file Kerno cannot read is said,
//! never overwritten.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use gpui_kit::base::TestTextExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, Global, InteractiveElement as _,
    IntoElement, Keystroke, ParentElement as _, Render, SharedString, Styled as _, Subscription,
    TestSupportExt as _, Window, div,
};

use crate::actions::{self, COMMANDS, Command, HOST_OS, KeymapChange, Overrides};
use crate::bridge;
use crate::state::AppState;

/// What the page knows of the keymap file.
enum File {
    /// Being read.
    Loading,
    /// Read: its text (`None`: no file yet) and its entries.
    Read {
        text: Option<String>,
        overrides: Overrides,
    },
    /// It cannot be read or is not a keymap file: said, never written.
    Unreadable(String),
}

/// Where the next keystroke goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    /// In place of the command's shortcut at this index.
    Replace(usize),
    /// After its shortcuts.
    Add,
}

/// The shortcut being recorded.
struct Recording {
    command: &'static Command,
    slot: Slot,
    /// Why the last keystroke was not taken.
    refused: Option<String>,
}

/// A shortcut recorded that other commands answer where this one would.
struct Question {
    command: &'static Command,
    slot: Slot,
    key: String,
    owners: Vec<&'static Command>,
}

pub struct ShortcutsView {
    filter: Entity<InputState>,
    file: File,
    recording: Option<Recording>,
    question: Option<Question>,
    /// "Reset all" was clicked and waits for its confirmation.
    confirm_reset_all: bool,
    /// What was wrong with the file when it was applied last.
    problems: Vec<String>,
    /// Takes the next keystroke while a shortcut is recorded; dropped with
    /// the view, so a closed Settings never keeps the keyboard.
    interceptor: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl ShortcutsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Filter by name, group or shortcut (⌘K, cmd k)")
        });
        let subscriptions = vec![cx.subscribe(&filter, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.stop_recording(cx);
            }
        })];
        let mut view = Self {
            filter,
            file: File::Loading,
            recording: None,
            question: None,
            confirm_reset_all: false,
            problems: Vec::new(),
            interceptor: None,
            _subscriptions: subscriptions,
        };
        view.load(cx);
        view
    }

    /// Reads the keymap file off the UI thread and applies it: the page
    /// shows what is bound, a change typed in the file by hand included.
    fn load(&mut self, cx: &mut Context<Self>) {
        let path = AppState::get(cx).paths.keymap();
        let writer = cx.default_global::<KeymapWriter>().0.clone();
        let reading = bridge::spawn_blocking(cx, move || {
            // Changes still on their way to the file (Settings closed and
            // opened again at once): the file will hold the page's last text.
            match writer.pending_text() {
                Some(text) => Ok(Some(text)),
                None => read(&path),
            }
        });
        cx.spawn(async move |this, cx| {
            let read = reading.await;
            let _ = this.update(cx, |this, cx| {
                this.file = match read {
                    Ok(text) => match actions::overrides(text.as_deref().unwrap_or_default()) {
                        Ok(overrides) => {
                            this.problems =
                                actions::apply_keymap(text.as_deref().unwrap_or_default(), cx);
                            File::Read { text, overrides }
                        }
                        Err(error) => File::Unreadable(error),
                    },
                    Err(error) => File::Unreadable(error),
                };
                cx.notify();
            });
        })
        .detach();
    }

    /// The entries of the file, when the page can change it.
    fn overrides(&self) -> Option<&Overrides> {
        match &self.file {
            File::Read { overrides, .. } => Some(overrides),
            _ => None,
        }
    }

    /// Makes `changes` in order: applied at once, then written in the
    /// background to the file as it is then (a file Kerno cannot read is
    /// left alone and said).
    fn change(&mut self, changes: Vec<KeymapChange>, window: &mut Window, cx: &mut Context<Self>) {
        // A question asked before this change is about keys that are no more.
        self.stop_recording(cx);
        let File::Read { text, .. } = &self.file else {
            return;
        };
        let mut text = text.clone();
        for change in &changes {
            match actions::changed_keymap(text.as_deref(), change) {
                Ok(changed) => text = Some(changed),
                Err(error) => {
                    self.file = File::Unreadable(error);
                    cx.notify();
                    return;
                }
            }
        }
        let text = text.unwrap_or_default();
        self.problems = actions::apply_keymap(&text, cx);
        write(changes, text.clone(), window, cx);
        self.file = File::Read {
            overrides: actions::overrides(&text).unwrap_or_default(),
            text: Some(text),
        };
        cx.notify();
    }

    /// Starts recording the shortcut of `command` that goes in `slot`.
    fn record(&mut self, command: &'static Command, slot: Slot, cx: &mut Context<Self>) {
        self.recording = Some(Recording {
            command,
            slot,
            refused: None,
        });
        self.question = None;
        self.confirm_reset_all = false;
        let view = cx.entity().downgrade();
        // Before any binding or field sees it: the keystroke is the new
        // shortcut, not a command (Escape included, which cancels).
        self.interceptor = Some(cx.intercept_keystrokes(move |event, window, cx| {
            let Some(view) = view.upgrade() else {
                return;
            };
            if event.keystroke.is_modifier_key() {
                return;
            }
            cx.stop_propagation();
            let stroke = event.keystroke.clone();
            view.update(cx, |view, cx| view.pressed(stroke, window, cx));
        }));
        cx.notify();
    }

    /// Ends a recording, if one runs, and the question it raised: Settings
    /// shows another section or searches, the filter changes, a shortcut is
    /// changed another way (Remove, Reset, Reset all).
    pub(crate) fn stop_recording(&mut self, cx: &mut Context<Self>) {
        self.recording = None;
        self.question = None;
        self.interceptor = None;
        cx.notify();
    }

    /// A keystroke while recording.
    fn pressed(&mut self, stroke: Keystroke, window: &mut Window, cx: &mut Context<Self>) {
        let Some(recording) = self.recording.as_mut() else {
            self.interceptor = None;
            return;
        };
        let modifiers = stroke.modifiers;
        let bare = !(modifiers.control || modifiers.alt || modifiers.shift || modifiers.platform);
        if stroke.key == "escape" && bare {
            self.stop_recording(cx);
            return;
        }
        let (command, slot) = (recording.command, recording.slot);
        let key = match actions::recorded_key(&stroke, command.context, HOST_OS) {
            Ok(key) => key,
            Err(why) => {
                recording.refused = Some(why);
                cx.notify();
                return;
            }
        };
        // A command with no key context would win over the text fields'
        // own key in every field (a command of the tree or a grid does not).
        if command.context.is_none()
            && let Some(action) = actions::kept_for_the_kit(&key, cx)
        {
            recording.refused = Some(format!(
                "{} is the text fields' {action}: a command that works everywhere would take it \
                 from them.",
                actions::shown(&key)
            ));
            cx.notify();
            return;
        }
        self.stop_recording(cx);
        let Some(overrides) = self.overrides() else {
            return;
        };
        let keys = actions::keys_in_effect(command, overrides);
        if keys
            .iter()
            .any(|each| actions::same_keys(each, &key, HOST_OS))
        {
            return;
        }
        let owners = actions::conflicts(command.id, &key, overrides, HOST_OS);
        if owners.is_empty() {
            self.assign(command, slot, key, Vec::new(), window, cx);
        } else {
            self.question = Some(Question {
                command,
                slot,
                key,
                owners,
            });
        }
    }

    /// Gives `key` to `command` in `slot`, after taking it from `owners`.
    fn assign(
        &mut self,
        command: &'static Command,
        slot: Slot,
        key: String,
        owners: Vec<&'static Command>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(overrides) = self.overrides() else {
            return;
        };
        let mut changes: Vec<KeymapChange> = owners
            .iter()
            .map(|owner| {
                let kept = actions::keys_in_effect(owner, overrides)
                    .into_iter()
                    .filter(|each| !actions::same_keys(each, &key, HOST_OS))
                    .collect();
                KeymapChange::Set(owner.id, kept)
            })
            .collect();
        let mut keys = actions::keys_in_effect(command, overrides);
        match slot {
            Slot::Replace(index) if index < keys.len() => keys[index] = key,
            _ => keys.push(key),
        }
        changes.push(KeymapChange::Set(command.id, keys));
        self.change(changes, window, cx);
    }

    fn remove(
        &mut self,
        command: &'static Command,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(overrides) = self.overrides() else {
            return;
        };
        let mut keys = actions::keys_in_effect(command, overrides);
        if index < keys.len() {
            keys.remove(index);
        }
        self.change(vec![KeymapChange::Set(command.id, keys)], window, cx);
    }

    /// One command: its name, its shortcuts as buttons that record a new
    /// one in their place, Add, Reset when changed, and what recording says.
    fn render_command(
        &self,
        command: &'static Command,
        overrides: &Overrides,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let id = command.id;
        let keys = actions::keys_in_effect(command, overrides);
        let changed = overrides.contains_key(id);
        let recording = self
            .recording
            .as_ref()
            .filter(|recording| recording.command.id == id);
        let question = self
            .question
            .as_ref()
            .filter(|question| question.command.id == id);
        let element = |what: &str| SharedString::from(format!("shortcut-{id}-{what}"));
        let mut shortcuts = h_flex().flex_none().flex_wrap().gap_1().justify_end();
        for (index, key) in keys.iter().enumerate() {
            let waiting = recording.is_some_and(|r| r.slot == Slot::Replace(index));
            let shown = actions::shown(key);
            shortcuts = shortcuts.child(
                h_flex()
                    .gap_0p5()
                    .child(
                        Button::new(element(&format!("key-{index}")))
                            .outline()
                            .xsmall()
                            .when(waiting, |button| button.selected(true))
                            .label(if waiting {
                                SharedString::from("Press a shortcut…")
                            } else {
                                SharedString::from(shown.clone())
                            })
                            .tooltip(format!("Change {shown}"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.record(command, Slot::Replace(index), cx)
                            })),
                    )
                    .child(
                        Button::new(element(&format!("remove-{index}")))
                            .ghost()
                            .xsmall()
                            .label("×")
                            .tooltip(format!("Remove {shown}"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.remove(command, index, window, cx)
                            })),
                    ),
            );
        }
        let adding = recording.is_some_and(|r| r.slot == Slot::Add);
        shortcuts = shortcuts.child(
            Button::new(element("add"))
                .ghost()
                .xsmall()
                .when(adding, |button| button.selected(true))
                .label(if adding {
                    "Press a shortcut…"
                } else if keys.is_empty() {
                    "Add a shortcut"
                } else {
                    "Add"
                })
                .on_click(cx.listener(move |this, _, _, cx| this.record(command, Slot::Add, cx))),
        );
        if changed {
            shortcuts = shortcuts.child(
                Button::new(element("reset"))
                    .ghost()
                    .xsmall()
                    .label("Reset to default")
                    .tooltip(if command.keys.is_empty() {
                        "No shortcut by default".to_string()
                    } else {
                        format!(
                            "Default: {}",
                            command
                                .keys
                                .iter()
                                .map(|key| actions::shown(key))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.change(vec![KeymapChange::Reset(id)], window, cx)
                    })),
            );
        }
        v_flex()
            .id(element("row"))
            .test_support()
            .py_1()
            .gap_1()
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_2()
                            .child(command.label)
                            .when(changed, |this| {
                                this.child(
                                    div()
                                        .id(element("changed"))
                                        .test_support()
                                        .text_xs()
                                        .text_color(muted)
                                        .child("Changed"),
                                )
                            }),
                    )
                    .child(shortcuts),
            )
            .when_some(recording, |this, recording| {
                let line = recording.refused.clone().unwrap_or_else(|| {
                    "Press the new shortcut with its modifiers; Escape cancels.".to_string()
                });
                // A refusal in the text colour, the hint muted (both AA).
                let refused = recording.refused.is_some();
                this.child(
                    div()
                        .id("shortcut-recording")
                        .test_support()
                        .text_xs()
                        .when(!refused, |this| this.text_color(muted))
                        .when(refused, |this| this.font_weight(FontWeight::MEDIUM))
                        .child(SharedString::from(line.clone()))
                        .test_text(line),
                )
            })
            .when_some(question, |this, question| {
                let names = question
                    .owners
                    .iter()
                    .map(|owner| format!("“{}” ({})", owner.label, owner.group))
                    .collect::<Vec<_>>()
                    .join(", ");
                let line = format!(
                    "{} is already the shortcut of {names}. Replace takes it from {} and gives \
                     it to “{}”.",
                    actions::shown(&question.key),
                    if question.owners.len() == 1 {
                        "it"
                    } else {
                        "them"
                    },
                    question.command.label,
                );
                this.child(
                    h_flex()
                        .id("shortcut-question")
                        .test_support()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .child(SharedString::from(line.clone()))
                                .test_text(line),
                        )
                        .child(
                            Button::new("shortcut-replace")
                                .primary()
                                .xsmall()
                                .label("Replace")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    if let Some(question) = this.question.take() {
                                        this.assign(
                                            question.command,
                                            question.slot,
                                            question.key,
                                            question.owners,
                                            window,
                                            cx,
                                        );
                                    }
                                })),
                        )
                        .child(
                            Button::new("shortcut-keep")
                                .outline()
                                .xsmall()
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.question = None;
                                    cx.notify();
                                })),
                        ),
                )
            })
            .into_any_element()
    }

    fn render_list(&self, overrides: &Overrides, cx: &mut Context<Self>) -> AnyElement {
        let query = self.filter.read(cx).value().to_string();
        let mut groups: Vec<(&'static str, Vec<&'static Command>)> = Vec::new();
        for command in COMMANDS {
            let keys = actions::keys_in_effect(command, overrides);
            if !actions::command_matches(command, &keys, &query, HOST_OS) {
                continue;
            }
            match groups.iter_mut().find(|(group, _)| *group == command.group) {
                Some((_, commands)) => commands.push(command),
                None => groups.push((command.group, vec![command])),
            }
        }
        if groups.is_empty() {
            use gpui_kit::component::empty::{
                Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyTitle,
            };
            return div()
                .id("shortcuts-no-match")
                .test_support()
                .flex()
                .child(
                    Empty::new()
                        .header(
                            EmptyHeader::new()
                                .title(EmptyTitle::new().child(SharedString::from(format!(
                                    "No command matches “{}”",
                                    query.trim()
                                ))))
                                .description(EmptyDescription::new().child(
                                    "Type a word of a command's name or group, or a shortcut \
                                     such as ⌘K or “cmd shift d”.",
                                )),
                        )
                        .content(
                            EmptyContent::new().child(
                                Button::new("shortcuts-filter-clear")
                                    .outline()
                                    .small()
                                    .label("Clear filter")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.filter.update(cx, |filter, cx| {
                                            filter.set_value("", window, cx)
                                        });
                                        cx.notify();
                                    })),
                            ),
                        ),
                )
                .into_any_element();
        }
        let muted = cx.theme().muted_foreground;
        v_flex()
            .gap_3()
            .children(groups.into_iter().map(|(group, commands)| {
                v_flex()
                    .id(SharedString::from(format!("shortcuts-group-{group}")))
                    .test_support()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(muted)
                            .child(group),
                    )
                    .children(
                        commands
                            .into_iter()
                            .map(|command| self.render_command(command, overrides, cx)),
                    )
            }))
            .into_any_element()
    }
}

impl Render for ShortcutsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let page = v_flex().id("shortcuts").test_support().gap_3();
        let overrides = match &self.file {
            File::Loading => {
                return page.child(
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child("Reading the keymap file…"),
                );
            }
            File::Unreadable(error) => {
                let line = format!(
                    "The keymap file cannot be used ({error}). Kerno keeps the shortcuts it \
                     started with and does not change the file: fix it, then open Settings again."
                );
                return page.child(
                    v_flex()
                        .id("shortcuts-unreadable")
                        .test_support()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child(SharedString::from(line.clone()))
                                .test_text(line),
                        )
                        .child(
                            Button::new("shortcuts-open-file")
                                .outline()
                                .small()
                                .label("Open keymap file")
                                .on_click(|_, window, cx| {
                                    crate::settings_view::open_keymap_file(window, cx)
                                }),
                        ),
                );
            }
            File::Read { overrides, .. } => overrides.clone(),
        };
        let changed = overrides.len();
        page.child(
            div()
                .text_xs()
                .text_color(muted)
                .child(SharedString::from(format!(
                    "Click a shortcut to change it, or Add for one more. {} Changes apply at \
                     once and go to the keymap file, which stays yours to edit by hand.",
                    actions::RECORD_RULE
                ))),
        )
        .child(
            h_flex()
                .gap_2()
                .child(
                    div()
                        .id("shortcuts-filter")
                        .test_support()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(&self.filter).small().cleanable(true)),
                )
                .child(
                    Button::new("shortcuts-reset-all")
                        .outline()
                        .small()
                        .label("Reset all…")
                        .disabled(changed == 0)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.stop_recording(cx);
                            this.confirm_reset_all = true;
                            cx.notify();
                        })),
                ),
        )
        .when(self.confirm_reset_all, |this| {
            let line = format!(
                "Every command gets its default shortcuts back: {changed} changed {}.",
                if changed == 1 {
                    "command loses its changes"
                } else {
                    "commands lose their changes"
                }
            );
            this.child(
                h_flex()
                    .id("shortcuts-reset-all-question")
                    .test_support()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .child(SharedString::from(line.clone()))
                            .test_text(line),
                    )
                    .child(
                        Button::new("shortcuts-reset-all-confirm")
                            .danger()
                            .xsmall()
                            .label("Reset all")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.confirm_reset_all = false;
                                this.change(vec![KeymapChange::ResetAll], window, cx);
                            })),
                    )
                    .child(
                        Button::new("shortcuts-reset-all-cancel")
                            .outline()
                            .xsmall()
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.confirm_reset_all = false;
                                cx.notify();
                            })),
                    ),
            )
        })
        .when(!self.problems.is_empty(), |this| {
            let line = format!(
                "The keymap file has entries Kerno does not use: {}.",
                self.problems.join("; ")
            );
            this.child(
                div()
                    .id("shortcuts-problems")
                    .test_support()
                    .text_xs()
                    .child(SharedString::from(line.clone()))
                    .test_text(line),
            )
        })
        .child(self.render_list(&overrides, cx))
    }
}

/// The keymap file's text; `None` when there is none yet.
fn read(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

/// Makes `change` in the file at `path` as it is now, through a file beside
/// it renamed over it: a failed write leaves the old file whole.
fn write_change(path: &Path, change: &KeymapChange) -> Result<(), String> {
    let text = read(path)?;
    let changed = actions::changed_keymap(text.as_deref(), change)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    }
    let temporary = path.with_extension("json.writing");
    std::fs::write(&temporary, changed).map_err(|error| error.to_string())?;
    std::fs::rename(&temporary, path).map_err(|error| error.to_string())
}

/// The changes waiting to reach the keymap file, written in the order made
/// by one writer at a time.
#[derive(Default)]
struct Writer {
    queue: Mutex<Queue>,
}

#[derive(Default)]
struct Queue {
    /// The changes not written yet.
    changes: VecDeque<KeymapChange>,
    /// A writer runs.
    writing: bool,
    /// The page's text after its last change.
    latest: Option<String>,
}

impl Writer {
    fn queue(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// The page's last text while changes have still to reach the file.
    fn pending_text(&self) -> Option<String> {
        let queue = self.queue();
        (queue.writing || !queue.changes.is_empty())
            .then(|| queue.latest.clone())
            .flatten()
    }
}

#[derive(Default)]
struct KeymapWriter(Arc<Writer>);

impl Global for KeymapWriter {}

/// Queues `changes` (which made `text`) for the keymap file; a failure is
/// said in a notification (the shortcuts apply until Kerno quits).
fn write(changes: Vec<KeymapChange>, text: String, window: &mut Window, cx: &mut App) {
    let writer = cx.default_global::<KeymapWriter>().0.clone();
    let start = {
        let mut queue = writer.queue();
        queue.changes.extend(changes);
        queue.latest = Some(text);
        !std::mem::replace(&mut queue.writing, true)
    };
    if !start {
        return;
    }
    let path: PathBuf = AppState::get(cx).paths.keymap();
    let writing = bridge::spawn_blocking(cx, move || {
        let mut outcome = Ok(());
        loop {
            let change = {
                let mut queue = writer.queue();
                match queue.changes.pop_front() {
                    Some(change) => change,
                    None => {
                        queue.writing = false;
                        break;
                    }
                }
            };
            if let Err(error) = write_change(&path, &change)
                && outcome.is_ok()
            {
                outcome = Err(error);
            }
        }
        outcome
    });
    window
        .spawn(cx, async move |cx| {
            if let Err(error) = writing.await {
                let _ = cx.update(|window, cx| {
                    window.push_notification(
                        Notification::error(SharedString::from(format!(
                            "The keymap file could not be written: {error}. The change works \
                             until Kerno quits or Settings reads the file again."
                        )))
                        .autohide(false),
                        cx,
                    )
                });
            }
        })
        .detach();
}
