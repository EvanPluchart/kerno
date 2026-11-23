//! Ask the assistant in the editor, and fix an error in one click (K52, D-121; CLAUDE.md §4 J17,
//! §1 "Nothing leaves the machine"). "AI: Ask Here…" opens a small field under the statement at
//! the cursor (or the selection, or the cursor's empty line); "Fix with AI" on a failed statement
//! sends it with the server's error at once. The answer's SQL comes back as a diff against what
//! it would replace: the lines it removes tinted in the text, and the whole proposal in a card
//! under them, each line signed − or +. Nothing of the text changes until Accept (one undo step);
//! Reject leaves the text, the cursor, the selection and the undo history as they were; nothing
//! it proposes runs. Requests go through `kerno_core::ai` as the panel's do, with the schema
//! context only (never sample rows), and join the session's "What was sent".
//!
//! The kit draws the tint (a range decoration); the card is Kerno's, placed over the editor
//! from its layout of the frame (as K37's `run_marks` reads it): the kit's own popovers at the
//! caret are internal to it, and its editor has no rows that are not text.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use gpui_kit::base::TestTextExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{
    EditorState, Escape, Input, InputEvent, InputState, RangeDecoration, RangeDecorationCollection,
    RangeDecorationStyle, RopeExt as _,
};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, AvailableSpace, Bounds, Context, Element, ElementId, Entity,
    EventEmitter, Focusable as _, FontWeight, GlobalElementId, InspectorElementId,
    InteractiveElement as _, IntoElement, LayoutId, ParentElement as _, Pixels, Render,
    SharedString, StatefulInteractiveElement as _, Style, Styled as _, Subscription, Task,
    TestSupportExt as _, WeakEntity, Window, deferred, div, point, px, size,
};
use kerno_core::Secret;
use kerno_core::ai::inline::{self, Change, Inline, Line};
use kerno_core::ai::{self, Context as AiContext, Detail, Turn};
use kerno_core::schema::TableRef;
use kerno_core::storage::now_ms;

use crate::actions::{AiAskHere, CancelQuery, RunStatement};
use crate::ai_view::{self, Facts, OFF, OFF_NOTICE, Sent};
use crate::bridge;
use crate::query::tab::QueryTab;
use crate::state::AppState;

/// The widest the card is drawn.
const MAX_WIDTH: Pixels = px(760.);
/// How much of the theme's danger and success colours tint a removed and an added line of the
/// card, and of danger the removed lines in the text (text stays at 4.5:1 on them, `theme.rs`).
pub const REMOVED_TINT: f32 = 0.14;
pub const ADDED_TINT: f32 = 0.16;
pub const MARK_TINT: f32 = 0.25;

/// What the field was opened for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    /// A sentence about the SQL at the place (or about nothing, on an empty line).
    Ask,
    /// A statement the server refused: what the error view shows, and where it pointed.
    Fix {
        error: String,
        position: Option<u32>,
    },
}

/// What the field tells its tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineEvent {
    /// Accepted, rejected, closed or moved to the panel: the tab lets it go and gives the
    /// keyboard back to the editor.
    Closed,
}

/// A request on its way.
struct Sending {
    /// The endpoint's host; `None` while the tables' structures are still being read.
    host: Option<String>,
    stop: ai::Stopper,
    _task: Option<Task<()>>,
}

/// An answer, as the card shows it.
struct Shown {
    answer: String,
    /// Its SQL fitted to the place, with the diff against what it replaces; `None` for an
    /// answer without SQL (its text is shown, nothing to accept).
    proposal: Option<Proposal>,
}

struct Proposal {
    replacement: String,
    lines: Vec<Line>,
}

/// The field and the proposal of one request at a place of the editor (K52).
pub struct AiInline {
    tab: WeakEntity<QueryTab>,
    editor: Entity<EditorState>,
    input: Entity<InputState>,
    /// The place asked about, and its text then.
    range: Range<usize>,
    original: String,
    subject: Subject,
    facts: Facts,
    context: AiContext,
    /// The structures read for the context (K50), the tables asked for, the reads going.
    details: HashMap<TableRef, Detail>,
    details_asked: HashSet<TableRef>,
    details_reading: usize,
    /// The turns so far (each message as sent, each answer), and what the user typed for each.
    turns: Vec<Turn>,
    typed: Vec<String>,
    sending: Option<Sending>,
    /// A message waiting for the structures before it goes.
    waiting: Option<(String, String)>,
    shown: Option<Shown>,
    /// What happened last, in plain words; `true` for a failure.
    note: Option<(String, bool)>,
    /// The text moved under the place: nothing more can be asked or accepted here.
    stale: bool,
    /// Bumped by Stop, an edit, a tab switch and each request: a late answer is dropped.
    generation: u64,
    show_payload: bool,
    /// The field is emptied at the next frame (an answer came: it waits for a refinement).
    clear_field: bool,
    /// The body of the last request sent, as "What was sent" shows it.
    last_sent: Option<String>,
    has_key: bool,
    /// The lines the proposal removes, tinted in the text; the place framed while sending.
    marks: RangeDecorationCollection,
    tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<InlineEvent> for AiInline {}

impl Drop for AiInline {
    /// A closed tab takes its request with it: the connection is closed.
    fn drop(&mut self) {
        if let Some(sending) = &self.sending {
            sending.stop.stop();
        }
    }
}

impl AiInline {
    /// A field on `range` of `editor` (its text now is what is asked about). A fix is sent at
    /// once; an ask waits for the sentence.
    pub fn new(
        tab: WeakEntity<QueryTab>,
        editor: Entity<EditorState>,
        facts: Facts,
        range: Range<usize>,
        subject: Subject,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let original = editor.read(cx).text().to_string()[range.clone()].to_string();
        let placeholder = match (&subject, original.trim().is_empty()) {
            (Subject::Fix { .. }, _) => "Say what else to change (Enter)",
            (Subject::Ask, true) => "Describe the SQL to write here (Enter)",
            (Subject::Ask, false) => "Say how to change this SQL (Enter)",
        };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let marks = editor.update(cx, |editor, cx| {
            editor.create_range_decorations_collection(Vec::new(), cx)
        });
        let subscriptions = vec![
            cx.subscribe_in(
                &input,
                window,
                |this: &mut Self, _, event: &InputEvent, window, cx| match event {
                    InputEvent::PressEnter {
                        secondary: true, ..
                    } => this.accept(window, cx),
                    InputEvent::PressEnter { .. } => this.send(window, cx),
                    InputEvent::Change => this.refresh(cx),
                    _ => {}
                },
            ),
            // The text moved under the place: a request is dropped, a proposal goes.
            cx.subscribe(&editor, |this: &mut Self, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.text_changed(cx);
                }
            }),
        ];
        let mut this = Self {
            tab,
            editor,
            input,
            range,
            original,
            subject,
            facts,
            context: AiContext::default(),
            details: HashMap::new(),
            details_asked: HashSet::new(),
            details_reading: 0,
            turns: Vec::new(),
            typed: Vec::new(),
            sending: None,
            waiting: None,
            shown: None,
            note: None,
            stale: false,
            generation: 0,
            show_payload: false,
            clear_field: false,
            last_sent: None,
            has_key: false,
            marks,
            tasks: Vec::new(),
            _subscriptions: subscriptions,
        };
        this.read_key(cx);
        this.refresh(cx);
        this.focus(window, cx);
        if matches!(this.subject, Subject::Fix { .. }) {
            this.send(window, cx);
        }
        this
    }

    /// The place asked about.
    pub fn range(&self) -> Range<usize> {
        self.range.clone()
    }

    /// Whether the text moved under the place (a new ask starts over).
    pub fn stale(&self) -> bool {
        self.stale
    }

    /// A request on its way (or waiting for the structures); for tests.
    #[cfg(test)]
    pub fn sending(&self) -> bool {
        self.sending.is_some()
    }

    /// The structures of the context still being read; for tests.
    #[cfg(test)]
    pub fn reading(&self) -> bool {
        self.details_reading > 0
    }

    /// The keyboard in the field.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Whether a key is stored for the endpoint, for "What is sent" and the log.
    fn read_key(&mut self, cx: &mut Context<Self>) {
        let Ok(endpoint) = ai::endpoint(&AppState::get(cx).settings.ai.endpoint) else {
            return;
        };
        let secrets = AppState::get(cx).secrets.clone();
        let reading = bridge::spawn_blocking(cx, move || {
            secrets
                .get(&ai::key_name(&endpoint.base))
                .ok()
                .flatten()
                .is_some_and(|key| !key.is_empty())
        });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let has_key = reading.await;
            let _ = this.update(cx, |this, cx| {
                this.has_key = has_key;
                cx.notify();
            });
        }));
    }

    /// The message the next send would send, and what the user typed for it; `None` while
    /// there is nothing to send.
    fn message(&self, cx: &App) -> Option<(String, String)> {
        let typed = self.input.read(cx).value().trim().to_string();
        let engine = &self.facts.engine;
        if !self.turns.is_empty() {
            return (!typed.is_empty()).then(|| (inline::refine(&typed), typed));
        }
        match &self.subject {
            Subject::Ask if typed.is_empty() => None,
            Subject::Ask => {
                let sql = (!self.original.trim().is_empty()).then(|| self.original.clone());
                let ask = Inline::Ask {
                    sentence: typed.clone(),
                    sql,
                };
                Some((ask.message(engine), typed))
            }
            Subject::Fix { error, position } => {
                let fix = Inline::Fix {
                    sql: self.original.clone(),
                    error: error.clone(),
                    position: *position,
                };
                Some((fix.message(engine), format!("Fix: {}", first_line(error))))
            }
        }
    }

    /// The text whose table names choose the tables described.
    fn naming_text(&self, cx: &App) -> String {
        let mut text = match &self.subject {
            Subject::Ask => self.original.clone(),
            Subject::Fix { error, .. } => format!("{}\n{error}", self.original),
        };
        text.push('\n');
        text.push_str(self.input.read(cx).value().as_ref());
        for turn in &self.turns {
            text.push('\n');
            text.push_str(&turn.text);
        }
        text
    }

    /// Puts the context together again and reads the structures it lacks (K50).
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let mut context = AiContext::from_catalog(
            &self.facts.catalog,
            &self.facts.engine,
            self.facts.server_version.clone(),
            self.facts.database.clone(),
            self.facts.schema.as_deref(),
            &self.naming_text(cx),
        );
        context.add_details(&self.details);
        self.context = context;
        self.load_details(cx);
        cx.notify();
    }

    fn load_details(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.facts.session.clone() else {
            return;
        };
        let tables: Vec<TableRef> = (self.context.detail_tables().into_iter())
            .filter(|table| !self.details_asked.contains(table))
            .collect();
        if tables.is_empty() {
            return;
        }
        self.details_asked.extend(tables.iter().cloned());
        self.details_reading += 1;
        let reading = bridge::spawn(cx, ai::read_details(session, tables));
        self.tasks.push(cx.spawn(async move |this, cx| {
            let read = reading.await;
            let _ = this.update(cx, |this, cx| {
                this.details_reading -= 1;
                for (table, detail) in read {
                    match detail {
                        Ok(detail) => {
                            this.details.insert(table, detail);
                        }
                        Err(why) => tracing::debug!(
                            "AI context: the structure of {}.{} was not read: {why}",
                            table.schema,
                            table.name
                        ),
                    }
                }
                this.refresh(cx);
                if this.details_reading == 0
                    && let Some((message, typed)) = this.waiting.take()
                {
                    this.dispatch(message, typed, cx);
                }
            });
        }));
    }

    /// The exact body the next send would send; `None` while there is nothing to send or the
    /// structures are being read.
    pub fn payload(&self, cx: &App) -> Option<serde_json::Value> {
        if self.details_reading > 0 {
            return None;
        }
        let (message, _) = self.message(cx)?;
        Some(ai::build_chat(
            &self.context,
            &self.turns,
            &message,
            &AppState::get(cx).settings.ai.model,
        ))
    }

    fn fail(&mut self, why: impl Into<String>, cx: &mut Context<Self>) {
        self.note = Some((why.into(), true));
        cx.notify();
    }

    /// Sends the sentence (or the fix), with the turns so far; the answer replaces the diff.
    pub fn send(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if self.sending.is_some() || self.stale {
            return;
        }
        let settings = AppState::get(cx).settings.ai.clone();
        if let Err(error) = ai::gate(&settings) {
            return self.fail(error.to_string(), cx);
        }
        if settings.model.trim().is_empty() {
            return self.fail(
                "Choose a model in Settings → AI (List models asks the endpoint). Nothing was sent.",
                cx,
            );
        }
        if self.original.chars().count() > ai::MAX_QUERY {
            return self.fail(
                format!(
                    "This SQL is longer than a request carries ({} characters): select the part \
                     to change. Nothing was sent.",
                    ai::MAX_QUERY
                ),
                cx,
            );
        }
        let Some((message, typed)) = self.message(cx) else {
            return;
        };
        self.note = None;
        self.generation += 1;
        self.sending = Some(Sending {
            host: None,
            stop: ai::Stopper::default(),
            _task: None,
        });
        self.mark_place(cx);
        if self.details_reading > 0 {
            self.waiting = Some((message, typed));
            cx.notify();
            return;
        }
        self.dispatch(message, typed, cx);
    }

    /// Sends `message`: logged in "What was sent" as it goes, off the UI thread.
    fn dispatch(&mut self, message: String, typed: String, cx: &mut Context<Self>) {
        let Some(sending) = &mut self.sending else {
            return;
        };
        let settings = AppState::get(cx).settings.ai.clone();
        let endpoint = match ai::gate(&settings) {
            Ok(endpoint) => endpoint,
            Err(error) => {
                self.sending = None;
                self.clear_marks(cx);
                return self.fail(error.to_string(), cx);
            }
        };
        let payload = ai::build_chat(&self.context, &self.turns, &message, &settings.model);
        let body = ai::preview(&payload);
        ai_view::record_sent(
            Sent {
                at_ms: now_ms(),
                endpoint: format!("{}/chat/completions", endpoint.base),
                key: self.has_key,
                body: body.clone(),
            },
            cx,
        );
        self.last_sent = Some(body);
        let generation = self.generation;
        let secrets = AppState::get(cx).secrets.clone();
        let key_name = ai::key_name(&endpoint.base);
        let stopper = sending.stop.clone();
        let asking = bridge::spawn_blocking(cx, move || {
            let key: Option<Secret> = secrets.get(&key_name).ok().flatten();
            ai::chat_until(&settings, key.as_ref(), &payload, &stopper)
        });
        sending.host = Some(endpoint.host);
        sending._task = Some(cx.spawn(async move |this, cx| {
            let answer = asking.await;
            let _ = this.update(cx, |this, cx| {
                // Stopped, edited, left: this answer is for another moment.
                if this.generation != generation {
                    return;
                }
                this.sending = None;
                this.clear_marks(cx);
                match answer {
                    Ok(answer) => this.take(message, typed, answer, cx),
                    Err(error) => this.fail(error.to_string(), cx),
                }
            });
        }));
        cx.notify();
    }

    /// The answer of the last request: the turn kept, its SQL fitted to the place and shown as
    /// a diff, the field emptied for a refinement.
    fn take(&mut self, message: String, typed: String, answer: String, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).text().to_string();
        if !inline::still_there(&text, &self.range, &self.original) {
            return self.went_stale(cx);
        }
        self.turns.push(Turn::user(message));
        self.turns.push(Turn::assistant(answer.clone()));
        self.typed.push(typed);
        let proposal = inline::proposed_sql(&answer).and_then(|sql| {
            let replacement = inline::replacement(&text, self.range.clone(), &sql);
            (replacement != self.original).then(|| Proposal {
                lines: inline::diff(&self.original, &replacement),
                replacement,
            })
        });
        if proposal.is_none() && inline::proposed_sql(&answer).is_some() {
            self.note = Some(("The assistant proposes no change.".into(), false));
        }
        self.shown = Some(Shown { answer, proposal });
        self.mark_removed(cx);
        // The window is not at hand here: the field is emptied on its next frame.
        self.clear_field = true;
        cx.notify();
    }

    /// Frames the place while its request is on its way: the quiet mark of a request.
    fn mark_place(&mut self, cx: &mut Context<Self>) {
        let color = cx.theme().muted_foreground;
        let range = self.range.clone();
        self.marks.set(
            vec![
                RangeDecoration::new(range)
                    .with_style(RangeDecorationStyle::Frame)
                    .with_color(color),
            ],
            cx,
        );
    }

    /// Tints the lines the proposal removes.
    fn mark_removed(&mut self, cx: &mut Context<Self>) {
        let Some(Proposal { lines, .. }) = self.shown.as_ref().and_then(|s| s.proposal.as_ref())
        else {
            return self.clear_marks(cx);
        };
        let color = cx.theme().danger.opacity(MARK_TINT);
        let start = self.range.start;
        let marks = inline::removed_ranges(&self.original, lines)
            .into_iter()
            .map(|range| {
                RangeDecoration::new(start + range.start..start + range.end)
                    .with_style(RangeDecorationStyle::Fill)
                    .with_color(color)
            })
            .collect();
        self.marks.set(marks, cx);
    }

    fn clear_marks(&mut self, cx: &mut Context<Self>) {
        self.marks.clear(cx);
    }

    /// The text changed: when the place no longer holds what was asked about, a request is
    /// dropped and a proposal goes; the field says so.
    fn text_changed(&mut self, cx: &mut Context<Self>) {
        if self.stale {
            return;
        }
        let text = self.editor.read(cx).text().to_string();
        if !inline::still_there(&text, &self.range, &self.original) {
            self.went_stale(cx);
        }
    }

    fn went_stale(&mut self, cx: &mut Context<Self>) {
        let was = if self.sending.is_some() {
            "its answer will not be shown"
        } else {
            "its proposal was dropped"
        };
        self.drop_request(cx);
        self.stale = true;
        self.shown = None;
        self.clear_marks(cx);
        self.note = Some((
            format!(
                "The text changed under this request: {was}. Ask again where you are ({}).",
                key("secondary-shift-h")
            ),
            false,
        ));
        cx.notify();
    }

    /// Closes the request in flight, if one is, and forgets what it would bring.
    fn drop_request(&mut self, cx: &mut Context<Self>) -> bool {
        self.generation += 1;
        self.waiting = None;
        let Some(sending) = self.sending.take() else {
            return false;
        };
        sending.stop.stop();
        self.mark_removed(cx);
        true
    }

    /// Stop (⌘., the field's button): the request is closed at once; what was shown stays.
    pub fn stop(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.drop_request(cx) {
            return false;
        }
        self.note = Some((
            "Stopped: the request was closed, nothing changed.".into(),
            false,
        ));
        cx.notify();
        true
    }

    /// The tab is no longer on screen: a request is closed; a proposal stays for its return.
    pub fn left(&mut self, cx: &mut Context<Self>) {
        if self.drop_request(cx) {
            self.note = Some((
                "Stopped when the tab was left: the request was closed, nothing changed.".into(),
                false,
            ));
            cx.notify();
        }
    }

    /// Accept (⌘↩, the button): the proposal in place of what it replaces, one undo step,
    /// selected; nothing runs. Then the field closes.
    pub fn accept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(replacement) = (self.shown.as_ref())
            .and_then(|shown| shown.proposal.as_ref())
            .map(|proposal| proposal.replacement.clone())
        else {
            return;
        };
        if self.sending.is_some() || self.stale {
            return;
        }
        // Closed first: this edit is not one that drops the proposal.
        self.stale = true;
        let range = self.range.clone();
        self.editor.update(cx, |editor, cx| {
            editor.set_selected_range(range.clone(), cx);
            editor.replace(replacement.clone(), window, cx);
            editor.set_selected_range(range.start..range.start + replacement.len(), cx);
        });
        self.close(cx);
    }

    /// Reject, Escape, Close: nothing of the text changes.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.discard(cx);
        cx.emit(InlineEvent::Closed);
    }

    /// Ends this field without a word to the tab (another one takes its place): its request
    /// closed, its marks gone from the text.
    pub fn discard(&mut self, cx: &mut Context<Self>) {
        self.drop_request(cx);
        self.stale = true;
        self.marks.dispose(cx);
    }

    /// "Open in the panel": the exchange so far goes to the tab's conversation, the field closes.
    pub fn open_in_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tab.upgrade() else {
            return;
        };
        let exchange: Vec<(String, String, String)> = (self.turns.chunks(2))
            .zip(&self.typed)
            .filter_map(|(pair, typed)| match pair {
                [asked, answer] => Some((typed.clone(), asked.text.clone(), answer.text.clone())),
                _ => None,
            })
            .collect();
        self.close(cx);
        window.defer(cx, move |window, cx| {
            let panel = tab.update(cx, |tab, cx| tab.ai_chat(window, cx));
            panel.update(cx, |panel, cx| panel.adopt(exchange, cx));
            ai_view::show(true, cx);
        });
    }

    /// The text of "What is sent" (before a request) or "What was sent" (after one).
    pub fn payload_text(&self, cx: &App) -> String {
        let address = ai::endpoint(&AppState::get(cx).settings.ai.endpoint)
            .map(|endpoint| format!("{}/chat/completions", endpoint.base))
            .unwrap_or_else(|_| "(no endpoint set)".into());
        let body = match (self.payload(cx), &self.last_sent) {
            (Some(payload), _) => ai::preview(&payload),
            (None, Some(sent)) => sent.clone(),
            (None, None) if self.details_reading > 0 => {
                "(reading the tables' keys and indexes…)".into()
            }
            (None, None) => "(nothing to send yet: write what you want first)".into(),
        };
        let key = if self.has_key {
            "\nAuthorization: Bearer ***"
        } else {
            ""
        };
        format!("POST {address}\nContent-Type: application/json{key}\n\n{body}")
    }

    /// The diff shown, as signed lines; for tests.
    #[cfg(test)]
    pub fn diff_lines(&self) -> Option<Vec<String>> {
        let proposal = self.shown.as_ref()?.proposal.as_ref()?;
        Some(
            (proposal.lines.iter())
                .map(|line| format!("{}{}", line.change.sign(), line.text))
                .collect(),
        )
    }

    /// The answer shown, whole; for tests.
    #[cfg(test)]
    pub fn answer(&self) -> Option<&str> {
        self.shown.as_ref().map(|shown| shown.answer.as_str())
    }

    /// What the field says last; for tests.
    #[cfg(test)]
    pub fn note(&self) -> Option<&str> {
        self.note.as_ref().map(|(text, _)| text.as_str())
    }

    #[cfg(test)]
    pub fn turns(&self) -> usize {
        self.turns.len()
    }

    /// What is typed in the field; for tests.
    #[cfg(test)]
    pub fn typed(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    /// The ranges framed or tinted in the text; for tests.
    #[cfg(test)]
    pub fn marked(&self, cx: &App) -> Vec<Range<usize>> {
        self.marks.get_ranges(cx)
    }
}

/// A key as this system writes it (⌘↩ on macOS, Ctrl+Enter elsewhere).
fn key(stroke: &str) -> String {
    crate::actions::shown(stroke)
}

/// A statement's first line, short.
fn first_line(text: &str) -> String {
    let line = text.trim().lines().next().unwrap_or_default();
    if line.chars().count() > 80 {
        format!("{}…", line.chars().take(80).collect::<String>())
    } else {
        line.to_string()
    }
}

/// `text` with its tabs as `width` spaces, as the editor draws them.
fn expanded(text: &str, width: usize) -> String {
    let width = width.max(1);
    let mut out = String::with_capacity(text.len());
    let mut column = 0;
    for c in text.chars() {
        if c == '\t' {
            let spaces = width - column % width;
            out.extend(std::iter::repeat_n(' ', spaces));
            column += spaces;
        } else {
            out.push(c);
            column += 1;
        }
    }
    out
}

impl AiInline {
    fn has_proposal(&self) -> bool {
        self.shown
            .as_ref()
            .is_some_and(|shown| shown.proposal.is_some())
    }

    /// The proposal, line by line, each signed and tinted: removed lines, then the lines added
    /// in their place, between the lines kept.
    fn render_diff(&self, lines: &[Line], cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let settings = &AppState::get(cx).settings;
        let (wrap, tab) = (settings.soft_wrap, usize::from(settings.tab_size));
        let ligatures = settings.ligatures;
        let editor = self.editor.read(cx);
        let line_height = editor.line_height().unwrap_or(px(20.));
        let room = (editor.input_bounds().size.height * 0.45).max(px(120.));
        let rows = lines.iter().map(|line| {
            let tint = match line.change {
                Change::Kept => None,
                Change::Removed => Some(theme.danger.opacity(REMOVED_TINT)),
                Change::Added => Some(theme.success.opacity(ADDED_TINT)),
            };
            h_flex()
                .items_start()
                .min_h(line_height)
                .when_some(tint, |row, tint| row.bg(tint))
                .child(
                    div()
                        .flex_none()
                        .w(px(20.))
                        .text_center()
                        .font_weight(FontWeight::BOLD)
                        .child(line.change.sign()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .when(!wrap, |text| text.whitespace_nowrap())
                        .child(SharedString::from(expanded(&line.text, tab))),
                )
        });
        let shown: Vec<String> = (lines.iter())
            .map(|line| format!("{}{}", line.change.sign(), line.text))
            .collect();
        v_flex()
            .id("ai-inline-diff")
            .test_support()
            .test_text(shown.join("\n"))
            .max_h(room)
            .overflow_y_scroll()
            .when(!wrap, |block| block.overflow_x_scroll())
            .py_1()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .text_color(theme.foreground)
            .font_family(theme.mono_font_family.clone())
            .text_size(theme.mono_font_size)
            .line_height(line_height)
            // `>=` stays two signs unless ligatures are chosen, as in the editor.
            .when(!ligatures, |block| {
                block.font_features(gpui_kit::FontFeatures::disable_ligatures())
            })
            .children(rows)
            .into_any_element()
    }

    /// The text of an answer without SQL, or the words around a proposal's SQL.
    fn render_words(&self, answer: &str, whole: bool, cx: &Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme();
        let words = if whole {
            ai::unfenced(answer)
        } else {
            (ai::segments(answer).into_iter())
                .filter_map(|part| match part {
                    ai::Segment::Text(text) => Some(text),
                    ai::Segment::Code { .. } => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        if words.trim().is_empty() {
            return None;
        }
        let room = (self.editor.read(cx).input_bounds().size.height * 0.3).max(px(80.));
        Some(
            div()
                .id(if whole {
                    "ai-inline-text"
                } else {
                    "ai-inline-words"
                })
                .test_support()
                .max_h(room)
                .overflow_y_scroll()
                .text_xs()
                .when(!whole, |words| words.text_color(theme.muted_foreground))
                .child(SharedString::from(words))
                .into_any_element(),
        )
    }
}

impl Render for AiInline {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The field went with a stale request: the keyboard goes back to the editor.
        if self.stale && self.input.read(cx).focus_handle(cx).is_focused(window) {
            let focus = self.editor.read(cx).focus_handle(cx);
            focus.focus(window, cx);
        }
        if std::mem::take(&mut self.clear_field) {
            self.input.update(cx, |input, cx| {
                input.set_value("", window, cx);
                input.set_placeholder("Refine: say what to change (Enter)", window, cx);
            });
        }
        let theme = cx.theme();
        let (muted, danger) = (theme.muted_foreground, theme.danger);
        let on = AppState::get(cx).settings.ai.enabled;
        let width = (self.editor.read(cx).input_bounds().size.width - px(16.))
            .min(MAX_WIDTH)
            .max(px(320.));
        let proposal = self.has_proposal();
        let sending = self.sending.as_ref().map(|sending| match &sending.host {
            Some(host) => format!("Asking {host}…"),
            None => "Reading the tables' keys and indexes…".to_string(),
        });
        let shown = self.shown.as_ref();
        let diff = (shown.and_then(|shown| shown.proposal.as_ref()))
            .map(|proposal| self.render_diff(&proposal.lines, cx));
        let words = shown.and_then(|shown| self.render_words(&shown.answer, !proposal, cx));
        let hint = if proposal {
            let run = key("secondary-enter");
            format!(
                "{run} accepts (one undo step), Esc rejects, Enter refines. Nothing runs: {run} in \
                 the editor runs the editor's own text."
            )
        } else if self.turns.is_empty() && self.subject == Subject::Ask {
            let what = if self.original.trim().is_empty() {
                "your sentence"
            } else {
                "this SQL and your sentence"
            };
            format!(
                "Enter asks, Esc closes. Sent: the schema's names, types and keys, {what}; never \
                 rows. Nothing runs by itself."
            )
        } else if self.turns.is_empty() {
            "Sent: the statement and the server's error, with the schema's names, types and \
             keys; never rows. Esc closes."
                .to_string()
        } else {
            "Enter asks again, Esc closes.".to_string()
        };
        v_flex()
            .id("ai-inline")
            .key_context("AiInline")
            .test_support()
            .occlude()
            .w(width)
            .p_2()
            .gap_1p5()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .shadow_md()
            .text_sm()
            // Escape in the field (nothing of the field's own to dismiss): rejected, closed.
            .on_action(cx.listener(|this, _: &Escape, _, cx| this.close(cx)))
            // ⌘↩ in the field accepts a proposal; with none it runs the editor's statement.
            .on_action(cx.listener(|this, _: &RunStatement, window, cx| {
                if this.has_proposal() && this.sending.is_none() && !this.stale {
                    this.accept(window, cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &CancelQuery, _, cx| {
                if !this.stop(cx) {
                    cx.propagate();
                }
            }))
            .children(diff)
            .children(words)
            .when(!on, |card| {
                card.child(
                    div()
                        .id("ai-inline-off")
                        .test_support()
                        .text_xs()
                        .text_color(danger)
                        .child(SharedString::from(format!(
                            "{OFF_NOTICE} Nothing is sent while it is off."
                        ))),
                )
            })
            .children(sending.map(|sending| {
                h_flex()
                    .id("ai-inline-sending")
                    .test_support()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(muted)
                            .child(SharedString::from(sending)),
                    )
                    .child(
                        Button::new("ai-inline-stop")
                            .outline()
                            .xsmall()
                            .label("Stop")
                            .tooltip(format!(
                                "{} — closes the request at once",
                                key("secondary-.")
                            ))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.stop(cx);
                            })),
                    )
            }))
            .children(self.note.as_ref().map(|(text, failed)| {
                div()
                    .id("ai-inline-note")
                    .test_support()
                    .text_xs()
                    .text_color(if *failed { danger } else { muted })
                    .child(SharedString::from(text.clone()))
            }))
            .child(
                h_flex()
                    .gap_1()
                    .when(!self.stale, |row| {
                        row.child(
                            div()
                                .id("ai-inline-field")
                                .test_support()
                                .flex_1()
                                .min_w_0()
                                .child(Input::new(&self.input).small()),
                        )
                    })
                    .when(self.stale, |row| row.child(div().flex_1()))
                    .when(proposal && !self.stale, |row| {
                        row.child(
                            Button::new("ai-inline-accept")
                                .primary()
                                .small()
                                .label("Accept")
                                .tooltip(format!(
                                    "{} — puts the lines marked + in place of those marked −, as \
                                     one undo step; runs nothing",
                                    key("secondary-enter")
                                ))
                                .disabled(self.sending.is_some())
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.accept(window, cx)),
                                ),
                        )
                        .child(
                            Button::new("ai-inline-reject")
                                .outline()
                                .small()
                                .label("Reject")
                                .tooltip("Esc — leaves the text as it is")
                                .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                        )
                    })
                    .when(!proposal || self.stale, |row| {
                        row.child(
                            Button::new("ai-inline-close")
                                .ghost()
                                .small()
                                .label("Close")
                                .tooltip("Esc — nothing changes")
                                .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                        )
                    }),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(muted)
                            .child(SharedString::from(hint)),
                    )
                    .when(!self.turns.is_empty(), |row| {
                        row.child(
                            Button::new("ai-inline-panel")
                                .ghost()
                                .xsmall()
                                .label("Open in the panel")
                                .tooltip("Goes on as a conversation in the tab's AI panel")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_in_panel(window, cx)
                                })),
                        )
                    })
                    .child(
                        Button::new("ai-inline-what")
                            .ghost()
                            .xsmall()
                            .label(match (self.show_payload, self.last_sent.is_some()) {
                                (true, _) => "Hide",
                                (false, false) => "What is sent…",
                                (false, true) => "What was sent…",
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_payload = !this.show_payload;
                                cx.notify();
                            })),
                    ),
            )
            .when(self.show_payload, |card| {
                card.child(
                    div()
                        .id("ai-inline-payload")
                        .test_support()
                        .max_h(px(200.))
                        .overflow_y_scroll()
                        .p_2()
                        .rounded(theme.radius)
                        .bg(theme.muted)
                        .font_family(theme.mono_font_family.clone())
                        .text_xs()
                        .child(SharedString::from(self.payload_text(cx))),
                )
            })
    }
}

/// Where something is drawn over the editor.
#[derive(Debug, Clone)]
enum Place {
    /// Under the rows of this range, else above them, always within the editor's view.
    Card(Range<usize>),
    /// At the right of the row of this offset, while that row is on screen.
    RowEnd(usize),
}

/// The top and bottom of the rows of `range`, in window coordinates, as the editor last laid
/// them out (soft-wrapped rows and folds counted); and its line height.
fn rows_of(editor: &EditorState, range: &Range<usize>) -> Option<(Pixels, Pixels, Pixels)> {
    let text = editor.text();
    let (start, end) = (range.start.min(text.len()), range.end.min(text.len()));
    let first = text.offset_to_point(start).row;
    let mut last = text.offset_to_point(end).row;
    // A range ending at a line's start does not reach that line.
    if last > first && text.line_start_offset(last) == end {
        last -= 1;
    }
    let origin = editor.text_bounds()?.origin;
    let line_height = editor.line_height()?;
    let map = editor.display_map();
    let top = map.buffer_line_to_display_row(first);
    let end = map
        .buffer_line_to_display_row_range(last)
        .map_or_else(|| map.buffer_line_to_display_row(last) + 1, |rows| rows.end)
        .max(top + 1);
    Some((
        origin.y + line_height * top as f32,
        origin.y + line_height * end as f32,
        line_height,
    ))
}

/// An element placed over the editor from its layout of this frame: drawn deferred, so that
/// its place is read after the editor's own prepaint (no frame behind when it scrolls).
struct OverEditor {
    editor: Entity<EditorState>,
    place: Place,
    child: Option<AnyElement>,
}

impl IntoElement for OverEditor {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for OverEditor {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        (window.request_layout(Style::default(), [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let editor = self.editor.read(cx);
        let view = editor.input_bounds();
        let range = match &self.place {
            Place::Card(range) => range.clone(),
            Place::RowEnd(at) => *at..*at,
        };
        let rows = rows_of(editor, &range);
        let Some(((top, bottom, line_height), mut child)) = rows.zip(self.child.take()) else {
            return;
        };
        if view.size.width <= px(0.) || view.size.height <= px(0.) {
            return;
        }
        // The card has its own width; the chip takes what its label needs.
        let space = match &self.place {
            Place::Card(_) => size(
                AvailableSpace::Definite(view.size.width),
                AvailableSpace::Definite(view.size.height),
            ),
            Place::RowEnd(_) => size(AvailableSpace::MinContent, AvailableSpace::MinContent),
        };
        let laid = child.layout_as_root(space, window, cx);
        let origin = match &self.place {
            Place::Card(_) => {
                let (view_top, view_bottom) = (view.top() + px(4.), view.bottom() - px(4.));
                let below = bottom + px(2.);
                let above = top - laid.height - px(2.);
                let y = if below + laid.height <= view_bottom {
                    below
                } else if above >= view_top {
                    above
                } else {
                    (view_bottom - laid.height).max(view_top)
                };
                point(view.left() + px(8.), y)
            }
            Place::RowEnd(_) => {
                // Only beside a row on screen.
                if top < view.top() || bottom > view.bottom() {
                    return;
                }
                point(
                    view.right() - laid.width - px(18.),
                    top + (line_height - laid.height) / 2.,
                )
            }
        };
        child.prepaint_at(origin, window, cx);
        self.child = Some(child);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(child) = self.child.as_mut() {
            child.paint(window, cx);
        }
    }
}

/// The field of `inline`, drawn over `editor` under the place it asks about.
pub fn overlay(editor: &Entity<EditorState>, inline: &Entity<AiInline>, cx: &App) -> AnyElement {
    deferred(OverEditor {
        editor: editor.clone(),
        place: Place::Card(inline.read(cx).range()),
        child: Some(inline.clone().into_any_element()),
    })
    .with_priority(1)
    .into_any_element()
}

/// "Fix with AI" beside the row where the server's error is marked in the editor.
pub fn fix_chip(
    editor: &Entity<EditorState>,
    at: usize,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    let chip = Button::new("ai-fix-chip")
        .outline()
        .xsmall()
        .label("Fix with AI")
        .tooltip("Proposes a corrected statement as a diff here; runs nothing")
        .on_click(move |_, window, cx| on_click(window, cx));
    deferred(OverEditor {
        editor: editor.clone(),
        place: Place::RowEnd(at),
        child: Some(div().occlude().child(chip).into_any_element()),
    })
    .with_priority(1)
    .into_any_element()
}

/// "AI: Ask Here…" or "AI: Fix Error" with no query tab active: says what is needed.
pub fn without_tab(window: &mut Window, cx: &mut App) {
    let text = match AppState::get(cx).settings.ai.enabled {
        true => "Open a query tab first: the AI assistant works on its editor.",
        false => OFF_NOTICE,
    };
    window.push_notification(Notification::warning(SharedString::from(text)), cx);
}

/// The editor menu's entries of this item: "AI: Ask Here…" and "AI: Fix Error", disabled with
/// the reason while the assistant is off.
pub fn editor_entries(
    mut menu: gpui_kit::component::menu::PopupMenu,
    cx: &App,
) -> gpui_kit::component::menu::PopupMenu {
    use gpui_kit::component::menu::PopupMenuItem;
    let on = AppState::get(cx).settings.ai.enabled;
    let entries: [(&str, ai_view::MakeAction); 2] = [
        ("AI: Ask Here…", || Box::new(AiAskHere)),
        ("AI: Fix Error", || Box::new(crate::actions::AiFixError)),
    ];
    for (label, action) in entries {
        let label = if on {
            label.to_string()
        } else {
            format!("{label} ({OFF})")
        };
        menu = menu.item(
            PopupMenuItem::new(SharedString::from(label))
                .disabled(!on)
                .on_click(move |_, window, cx| {
                    window.defer(cx, move |window, cx| window.dispatch_action(action(), cx));
                }),
        );
    }
    menu
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_are_drawn_as_the_editor_draws_them() {
        assert_eq!(expanded("\tSELECT", 4), "    SELECT");
        assert_eq!(expanded("ab\tc", 4), "ab  c");
        assert_eq!(expanded("x", 0), "x");
    }

    /// ⌘⇧H (Ctrl+Shift+H) is Ask Here's, and no other command answers it on either system.
    #[test]
    fn ask_here_has_a_key_of_its_own() {
        use gpui_kit::OperatingSystem;
        let command = (crate::actions::COMMANDS.iter())
            .find(|command| command.id == "ai_ask_here")
            .unwrap();
        assert_eq!(command.keys, ["secondary-shift-h"]);
        for os in [OperatingSystem::Mac, OperatingSystem::Linux] {
            let taken = crate::actions::conflicts(
                "ai_ask_here",
                "secondary-shift-h",
                &Default::default(),
                os,
            );
            assert!(taken.is_empty(), "{os:?}: {:?}", taken[0].id);
        }
    }

    /// The editor menu's entries run commands of the palette.
    #[test]
    fn the_entries_are_commands_of_the_palette() {
        for id in ["ai_ask_here", "ai_fix_error"] {
            assert!(
                crate::actions::COMMANDS
                    .iter()
                    .any(|command| command.id == id),
                "{id}"
            );
        }
    }
}
