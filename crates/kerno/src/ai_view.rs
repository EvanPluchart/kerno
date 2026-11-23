//! The AI assistant's screens (CLAUDE.md §4 J17, DBeaver #36951, #20072; D-068, D-120): the
//! conversation panel docked at the right of the window, one conversation per query tab, the AI
//! entries of the editor's menu, and the session's "What was sent" log (Settings → AI is
//! `ai_settings_view`). What is sent and the one function that sends are `kerno_core::ai`: this
//! file only gathers what the user chose and shows the answers, as text. An answer runs nothing:
//! its SQL goes in the editor when the user clicks Insert or Replace.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui_kit::base::TestTextExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{EditorState, InputEvent, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::resizable::{h_resizable, resizable_panel};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, FocusHandle, Focusable,
    FontWeight, Global, InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Render,
    ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task,
    TestSupportExt as _, WeakEntity, Window, div, px,
};
use kerno_core::ai::{
    self, Action, AiSettings, Context as AiContext, Detail, Sample, Segment, Turn,
};
use kerno_core::schema::TableRef;
use kerno_core::sql::Catalog;
use kerno_core::storage::now_ms;
use kerno_core::{Secret, exec};

use crate::actions;
use crate::bridge;
use crate::query::tab::QueryTab;
use crate::state::AppState;

pub mod ask;

/// What an entry says while the assistant is off.
pub const OFF: &str = "turn on in Settings → AI";

/// What an "AI: …" command says while the assistant is off, wherever it runs: what turning it
/// on gives, in the words of the top of Settings → AI (K23).
pub const OFF_NOTICE: &str = "The AI assistant is off. Turned on in Settings → AI, it writes SQL \
     from a sentence (never run by itself) and explains a query, a plan or an error, sending \
     only names and types to the endpoint you choose.";

/// The panel's width range and the editor area it leaves at least (K51).
pub const WIDTHS: std::ops::RangeInclusive<f32> = 320.0..=720.0;
const AREA_MIN: Pixels = px(360.);
/// Characters of a script beyond which the panel takes the editor's selection only.
const SPLIT_LIMIT: usize = 1_000_000;

/// An "AI: …" command that reached the window, not a query tab (the keyboard was in the
/// sidebar or a data tab): the active query tab takes it; with none, it says what is needed.
pub fn command(kind: Kind, tab: Option<Entity<QueryTab>>, window: &mut Window, cx: &mut App) {
    use gpui_kit::component::WindowExt as _;
    use gpui_kit::component::notification::Notification;
    match tab {
        Some(tab) => tab.update(cx, |tab, cx| tab.ai_request(kind, window, cx)),
        None => {
            let text = match AppState::get(cx).settings.ai.enabled {
                true => "Open a query tab first: the AI assistant works on its editor.",
                false => OFF_NOTICE,
            };
            window.push_notification(Notification::warning(SharedString::from(text)), cx);
        }
    }
}

/// What the user asks of the assistant, before the request is put together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    WriteSql,
    ExplainQuery,
    ExplainPlan,
    ExplainError,
}

/// Builds the command of a menu entry.
pub type MakeAction = fn() -> Box<dyn gpui_kit::Action>;

/// The AI entries of the editor's menu: label, command, enabled. Off, each says how to turn
/// the assistant on and does nothing.
pub fn menu_entries(on: bool) -> Vec<(String, MakeAction, bool)> {
    let entries: [(&str, MakeAction); 4] = [
        ("AI: Write SQL…", || Box::new(actions::AiWriteSql)),
        ("AI: Explain query", || Box::new(actions::AiExplainQuery)),
        ("AI: Explain plan", || Box::new(actions::AiExplainPlan)),
        ("AI: Explain error", || Box::new(actions::AiExplainError)),
    ];
    entries
        .into_iter()
        .map(|(label, action)| {
            let label = if on {
                label.to_string()
            } else {
                format!("{label} ({OFF})")
            };
            (label, action, on)
        })
        .collect()
}

/// Adds the AI entries to the editor's menu, under a separator.
pub fn editor_entries(
    mut menu: PopupMenu,
    _: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    menu = menu.separator();
    for (label, action, enabled) in menu_entries(AppState::get(cx).settings.ai.enabled) {
        menu = menu.item(
            PopupMenuItem::new(SharedString::from(label))
                .disabled(!enabled)
                .on_click(move |_, window, cx| {
                    // Once the menu has given the focus back to the editor.
                    window.defer(cx, move |window, cx| window.dispatch_action(action(), cx));
                }),
        );
    }
    menu
}

/// One request of the session, as Settings → AI ▸ What was sent lists it. In memory only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sent {
    pub at_ms: i64,
    /// `POST <address>`.
    pub endpoint: String,
    /// An API key went with it (shown as `Bearer ***`).
    pub key: bool,
    /// The body, as sent.
    pub body: String,
}

#[derive(Default)]
struct SentLog(Vec<Sent>);

impl Global for SentLog {}

/// Adds a request sent from elsewhere (K52: the editor's field) to the session's log.
pub fn record_sent(sent: Sent, cx: &mut App) {
    cx.default_global::<SentLog>().0.push(sent);
}

/// The requests the assistant sent during this run, the latest last.
pub fn sent_log(cx: &App) -> Vec<Sent> {
    cx.try_global::<SentLog>()
        .map(|log| log.0.clone())
        .unwrap_or_default()
}

/// The facts of the query tab a request is put together from.
pub struct Facts {
    pub catalog: Arc<Catalog>,
    pub engine: String,
    pub server_version: Option<String>,
    pub database: Option<String>,
    pub schema: Option<String>,
    /// The session in the catalog's database, for the sample rows and the structures.
    pub session: Option<Arc<dyn kerno_core::Session>>,
}

/// What the panel opens with (K51): the message made ready in its field, never sent by itself.
/// K53 opens it on a table or a result with a message of its own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Draft {
    /// What the message asks for (its instruction and subject go with it), or none for the
    /// user's words alone.
    pub action: Option<Action>,
    /// Words put in the message field.
    pub text: String,
}

/// Opens the AI panel at the right of the window on `tab`'s conversation, with `draft` ready in
/// its field and the keyboard there; nothing is sent until the user sends it. The entry point
/// for other places (K53). While the assistant is off it says so and opens nothing.
pub fn open(tab: &Entity<QueryTab>, draft: Draft, window: &mut Window, cx: &mut App) {
    if !AppState::get(cx).settings.ai.enabled {
        notify(OFF_NOTICE, window, cx);
        return;
    }
    let panel = tab.update(cx, |tab, cx| tab.ai_chat(window, cx));
    panel.update(cx, |panel, cx| panel.prepare(draft, window, cx));
    show(true, cx);
}

fn notify(text: &'static str, window: &mut Window, cx: &mut App) {
    use gpui_kit::component::WindowExt as _;
    use gpui_kit::component::notification::Notification;
    window.push_notification(Notification::warning(SharedString::from(text)), cx);
}

/// Shows or hides the panel, in every window, remembered.
pub fn show(shown: bool, cx: &mut App) {
    if AppState::get(cx).settings.ai_panel != shown {
        AppState::update_settings(cx, |settings| settings.ai_panel = shown);
    }
}

/// "AI: Show or Hide the Assistant": shown, the keyboard goes to the message field of the active
/// query tab's conversation; hidden, back to that tab.
pub fn toggle(tab: Option<Entity<QueryTab>>, window: &mut Window, cx: &mut App) {
    let shown = !AppState::get(cx).settings.ai_panel;
    show(shown, cx);
    let Some(tab) = tab else {
        return;
    };
    if shown {
        let panel = tab.update(cx, |tab, cx| tab.ai_chat(window, cx));
        panel.update(cx, |panel, cx| panel.sync_editor(cx));
        let focus = panel.read(cx).input.focus_handle(cx);
        focus.focus(window, cx);
    } else {
        let focus = tab.read(cx).editor().read(cx).focus_handle(cx);
        focus.focus(window, cx);
    }
}

/// "AI: New Conversation" on the active query tab: the panel shown, its thread cleared.
pub fn new_conversation(tab: Option<Entity<QueryTab>>, window: &mut Window, cx: &mut App) {
    let Some(tab) = tab else {
        notify(
            "Open a query tab first: a conversation belongs to its editor.",
            window,
            cx,
        );
        return;
    };
    show(true, cx);
    let panel = tab.update(cx, |tab, cx| tab.ai_chat(window, cx));
    panel.update(cx, |panel, cx| panel.clear(window, cx));
}

/// `area` (the window's tabs) with the panel at its right, while it is shown: the active query
/// tab's conversation, or what the panel is for when no query tab is active. Its width is the
/// user's, remembered.
pub fn beside(
    area: AnyElement,
    tab: Option<&Entity<QueryTab>>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let settings = &AppState::get(cx).settings;
    if !settings.ai_panel {
        return area;
    }
    let width = settings.ai_panel_width;
    let content = match tab {
        Some(tab) => {
            let panel = tab.update(cx, |tab, cx| tab.ai_chat(window, cx));
            // The editor's statement follows its cursor while the panel shows.
            panel.update(cx, |panel, cx| panel.sync_editor(cx));
            panel.into_any_element()
        }
        None => no_tab(cx).into_any_element(),
    };
    h_resizable("ai-dock-split")
        .child(
            resizable_panel()
                .size_range(AREA_MIN..Pixels::MAX)
                .child(area),
        )
        .child(
            resizable_panel()
                .size(px(width))
                .size_range(px(*WIDTHS.start())..px(*WIDTHS.end()))
                .fixed()
                .child(content),
        )
        .on_resize(|state, _, cx| {
            if let Some(width) = state.read(cx).sizes().get(1) {
                let width = f32::from(*width).clamp(*WIDTHS.start(), *WIDTHS.end());
                if (AppState::get(cx).settings.ai_panel_width - width).abs() >= 1.0 {
                    AppState::update_settings(cx, |settings| settings.ai_panel_width = width);
                }
            }
        })
        .into_any_element()
}

/// The panel when no query tab is active.
fn no_tab(cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    v_flex()
        .id("ai-panel")
        .test_support()
        .size_full()
        .p_3()
        .gap_2()
        .border_l_1()
        .border_color(theme.border)
        .bg(theme.background)
        .child(head(None, cx))
        .child(
            div()
                .id("ai-no-tab")
                .test_support()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(
                    "A conversation belongs to a query tab: open one (New SQL editor) and the \
                     assistant works on its editor, its database and its schema.",
                ),
        )
}

/// The panel's head: its name, New conversation, Close.
fn head(panel: Option<&Context<AiPanel>>, cx: &App) -> impl IntoElement {
    h_flex()
        .gap_1()
        .child(
            div()
                .flex_1()
                .font_weight(FontWeight::MEDIUM)
                .child("AI assistant"),
        )
        .children(panel.map(|panel| {
            Button::new("ai-new-conversation")
                .ghost()
                .xsmall()
                .label("New conversation")
                .tooltip("Clear this tab's conversation")
                .on_click(panel.listener(|this, _, window, cx| this.clear(window, cx)))
        }))
        .child(
            Button::new("ai-close")
                .ghost()
                .xsmall()
                .label("Close")
                .tooltip("Hide the panel (the conversation stays with its tab)")
                .on_click(|_, _, cx| show(false, cx)),
        )
        .text_color(cx.theme().foreground)
}

/// The status bar's entry, while the assistant is on: shows or hides the panel.
pub fn status_item(cx: &App) -> Option<AnyElement> {
    let settings = &AppState::get(cx).settings;
    if !settings.ai.enabled && !settings.ai_panel {
        return None;
    }
    let shown = settings.ai_panel;
    Some(
        div()
            .id("status-ai")
            .test_support()
            .cursor_pointer()
            .when(shown, |item| item.font_weight(FontWeight::SEMIBOLD))
            .child("AI")
            .tooltip(move |window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(if shown {
                    "Hide the AI assistant"
                } else {
                    "Show the AI assistant"
                })
                .build(window, cx)
            })
            .on_click(|_, window, cx| window.dispatch_action(Box::new(actions::ToggleAiPanel), cx))
            .into_any_element(),
    )
}

/// One message of the thread.
enum Entry {
    /// What the user sent: their words as shown, the chips of what went with them, and the
    /// message as sent (what later turns repeat).
    Asked {
        shown: String,
        chips: Vec<String>,
        sent: String,
    },
    /// The model's answer: while it arrives, the text so far (its reasoning still in it);
    /// once done, the answer without it.
    Answer { text: String, done: bool },
    /// A request that ended without an answer: stopped, or why it failed.
    Note { text: String, failed: bool },
}

enum Stage {
    Ready,
    Sending {
        host: String,
        /// Closes the request's connection (Stop, C3).
        stop: ai::Stopper,
        _task: Task<()>,
    },
}

/// The conversation of one query tab (K51): its thread, the message being written and what goes
/// with it, the request in flight. Kept while the tab lives, for the run only.
pub struct AiPanel {
    tab: WeakEntity<QueryTab>,
    editor: Entity<EditorState>,
    facts: Facts,
    input: Entity<TextareaState>,
    /// The action of the message being written (the subject chip), until it is sent or removed.
    action: Option<Action>,
    /// "Include the editor's query": on at each new message, removable for one.
    include_query: bool,
    /// The editor's statement at the cursor (or its selection), read when it may have changed.
    editor_query: Option<String>,
    /// The editor's selection and length when `editor_query` was read.
    editor_key: Option<(std::ops::Range<usize>, usize)>,
    /// "Include sample rows": off at every new message.
    include_samples: bool,
    /// The rows read for the tick, for these tables; `None` while reading.
    samples: Option<Result<Vec<Sample>, String>>,
    samples_for: Vec<TableRef>,
    /// The structures read for the context (K50), and the tables asked for so far.
    details: HashMap<TableRef, Detail>,
    details_asked: HashSet<TableRef>,
    details_reading: usize,
    /// What the model is told of the database for the message being written.
    context: AiContext,
    /// What a right-click asked (K53): the subject of the message being written.
    asked: Option<ask::Asked>,
    /// The answers' SQL is offered in a new query tab ("Generate test data…", K53).
    new_tab: bool,
    entries: Vec<Entry>,
    stage: Stage,
    /// Bumped by Stop, New conversation and each request: a late answer is dropped.
    generation: u64,
    show_preview: bool,
    /// An API key is stored for the endpoint (read off the UI thread, again when the active
    /// endpoint changes).
    has_key: bool,
    key_for: String,
    thread: ScrollHandle,
    tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Drop for AiPanel {
    /// A closed tab takes its conversation: a request in flight is closed with it.
    fn drop(&mut self) {
        if let Stage::Sending { stop, .. } = &self.stage {
            stop.stop();
        }
    }
}

impl AiPanel {
    pub fn new(
        tab: WeakEntity<QueryTab>,
        editor: Entity<EditorState>,
        facts: Facts,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 8)
                .submit_on_enter(true)
                .placeholder("Ask about this database, or describe a query to write")
        });
        let subscriptions = vec![
            cx.subscribe_in(
                &input,
                window,
                |this: &mut Self, _, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => {
                        this.sync_editor(cx);
                        this.refresh(cx)
                    }
                    InputEvent::PressEnter { shift: false, .. } => this.send(window, cx),
                    _ => {}
                },
            ),
            // Switched in Settings or in the picker: the key of the new endpoint, for the
            // preview's and the log's `Authorization` line.
            cx.observe_global::<AppState>(|this: &mut Self, cx| {
                if AppState::get(cx).settings.ai.endpoint != this.key_for {
                    this.read_key(cx);
                    cx.notify();
                }
            }),
        ];
        let mut panel = Self {
            tab,
            editor,
            facts,
            input,
            action: None,
            include_query: true,
            editor_query: None,
            editor_key: None,
            include_samples: false,
            samples: None,
            samples_for: Vec::new(),
            details: HashMap::new(),
            details_asked: HashSet::new(),
            details_reading: 0,
            context: AiContext::default(),
            asked: None,
            new_tab: false,
            entries: Vec::new(),
            stage: Stage::Ready,
            generation: 0,
            show_preview: false,
            has_key: false,
            key_for: String::new(),
            thread: ScrollHandle::new(),
            tasks: Vec::new(),
            _subscriptions: subscriptions,
        };
        // Made inside the tab's own update: its editor is read later, when the keyboard
        // comes to the panel or a message is sent.
        panel.read_key(cx);
        panel.refresh(cx);
        panel
    }

    /// Makes `draft` the message being written, the keyboard in its field: its action as the
    /// subject chip, its words in the field. Nothing is sent.
    pub fn prepare(&mut self, draft: Draft, window: &mut Window, cx: &mut Context<Self>) {
        self.facts_again(cx);
        self.action = draft.action;
        (self.asked, self.new_tab) = (None, false);
        self.include_query = true;
        self.include_samples = false;
        self.input.update(cx, |input, cx| {
            input.set_value(draft.text, window, cx);
            input.focus(window, cx);
        });
        self.read_editor(cx);
        self.refresh(cx);
    }

    /// The facts of the tab as they are now (a catalog loaded since, another database).
    /// What was read for the context (structures, rows) is kept while the tab's connection,
    /// database and schema stay.
    fn facts_again(&mut self, cx: &mut Context<Self>) {
        let Some(facts) = self.tab.upgrade().map(|tab| tab.read(cx).ai_facts(cx)) else {
            return;
        };
        let same_session = match (&facts.session, &self.facts.session) {
            (Some(new), Some(old)) => Arc::ptr_eq(new, old),
            (None, None) => true,
            _ => false,
        };
        if !same_session
            || facts.database != self.facts.database
            || facts.schema != self.facts.schema
        {
            self.details.clear();
            self.details_asked.clear();
            self.samples = None;
            self.samples_for.clear();
        }
        self.facts = facts;
    }

    /// "New conversation": the thread cleared, a request in flight stopped, the field emptied.
    pub fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Stage::Sending { stop, .. } = &self.stage {
            stop.stop();
        }
        self.generation += 1;
        self.stage = Stage::Ready;
        self.entries.clear();
        self.prepare(Draft::default(), window, cx);
    }

    /// K52: an exchange begun in the editor, moved here ("Open in the panel"): each question as
    /// the user typed it and as it was sent, with its whole answer.
    pub fn adopt(&mut self, exchange: Vec<(String, String, String)>, cx: &mut Context<Self>) {
        for (shown, sent, answer) in exchange {
            let chips = vec!["Asked in the editor".to_string()];
            self.entries.push(Entry::Asked { shown, chips, sent });
            self.entries.push(Entry::Answer {
                text: answer,
                done: true,
            });
        }
        self.thread.scroll_to_bottom();
        self.refresh(cx);
    }

    /// Whether a key is stored for the endpoint, for the preview's `Authorization` line.
    fn read_key(&mut self, cx: &mut Context<Self>) {
        self.key_for = AppState::get(cx).settings.ai.endpoint.clone();
        self.has_key = false;
        let Ok(endpoint) = ai::endpoint(&self.key_for) else {
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
        let base = self.key_for.clone();
        self.tasks.push(cx.spawn(async move |this, cx| {
            let has_key = reading.await;
            let _ = this.update(cx, |this, cx| {
                if this.key_for == base {
                    this.has_key = has_key;
                    cx.notify();
                }
            });
        }));
    }

    /// The editor's statement at the cursor, or its selection, as Run statement takes it. In a
    /// script over [`SPLIT_LIMIT`] only a selection is taken: it is not split at each move.
    fn read_editor(&mut self, cx: &mut Context<Self>) {
        let place = self.editor_place(cx);
        self.editor_query = if place.1 > SPLIT_LIMIT && place.0.is_empty() {
            None
        } else {
            self.tab
                .upgrade()
                .and_then(|tab| tab.read(cx).ai_statement(cx))
                .map(|(sql, _)| sql)
                .filter(|sql| !sql.trim().is_empty())
        };
        self.editor_key = Some(place);
    }

    /// Where the editor's selection is and how long its text: when it moved, its statement is
    /// read again (a comparison per frame; the split only when something changed).
    fn editor_place(&self, cx: &App) -> (std::ops::Range<usize>, usize) {
        let editor = self.editor.read(cx);
        (editor.selected_range(), editor.text().len())
    }

    /// Reads the editor's statement again if its selection or text moved since it was read.
    /// Not while the tab is being updated (the panel is made inside it).
    pub fn sync_editor(&mut self, cx: &mut Context<Self>) {
        // Made before the tab's catalog was read: read its facts again once it is.
        let unready = self.facts.catalog.schemas.is_empty() || self.facts.session.is_none();
        if unready {
            self.facts_again(cx);
        }
        let ready = !self.facts.catalog.schemas.is_empty() && unready;
        if ready || self.editor_key.as_ref() != Some(&self.editor_place(cx)) {
            self.read_editor(cx);
            self.refresh(cx);
        }
    }

    /// The editor's query goes with the message being written: it is ticked, there is one, and
    /// the message is not about a statement of its own.
    fn query_attached(&self) -> Option<&str> {
        let own = matches!(
            self.action,
            Some(
                Action::ExplainQuery { .. }
                    | Action::ExplainPlan { .. }
                    | Action::ExplainError { .. }
            )
        );
        (self.include_query && !own && self.asked.is_none())
            .then_some(self.editor_query.as_deref())
            .flatten()
    }

    /// The message being written, as sent; `None` while there is nothing to send.
    fn message(&self, cx: &App) -> Option<String> {
        let typed = self.input.read(cx).value().trim().to_string();
        let engine = &self.facts.engine;
        if let Some(asked) = &self.asked {
            return asked.message(&typed, engine);
        }
        let mut message = match &self.action {
            Some(Action::WriteSql { .. }) if typed.is_empty() => return None,
            Some(Action::WriteSql { .. }) => Action::WriteSql { sentence: typed }.message(engine),
            Some(action) if typed.is_empty() => action.message(engine),
            Some(action) => format!("{}\n\n{typed}", action.message(engine)),
            None if typed.is_empty() && self.query_attached().is_none() => return None,
            None => typed,
        };
        if let Some(sql) = self.query_attached() {
            message = ai::with_query(&message, sql);
        }
        Some(message)
    }

    /// The earlier turns of the thread: each question answered in full, with its answer.
    fn turns(&self) -> Vec<Turn> {
        let mut turns = Vec::new();
        for pair in self.entries.windows(2) {
            if let [
                Entry::Asked { sent, .. },
                Entry::Answer { text, done: true },
            ] = pair
            {
                turns.push(Turn::user(sent.clone()));
                turns.push(Turn::assistant(text.clone()));
            }
        }
        turns
    }

    /// The text whose table names choose the tables described: the message being written,
    /// then the earlier questions, the latest first.
    fn naming_text(&self, cx: &App) -> String {
        let mut text = self.message(cx).unwrap_or_default();
        if let Some(action) = &self.action {
            text = format!("{}\n{text}", action.text());
        }
        for entry in self.entries.iter().rev() {
            if let Entry::Asked { sent, .. } = entry {
                text.push('\n');
                text.push_str(sent);
            }
        }
        text
    }

    /// Puts the context together again (the message, the turns, the structures read changed)
    /// and reads what it lacks: the structures of new tables, the sample rows when ticked.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let mut context = AiContext::from_catalog(
            &self.facts.catalog,
            &self.facts.engine,
            self.facts.server_version.clone(),
            self.facts.database.clone(),
            self.facts.schema.as_deref(),
            &self.naming_text(cx),
        );
        if let Some(asked) = &self.asked {
            asked.ask.put_first(&mut context, &self.facts.catalog);
        }
        context.add_details(&self.details);
        self.context = context;
        self.load_details(cx);
        if self.include_samples {
            self.load_samples(cx);
        }
        cx.notify();
    }

    /// Whether structures are being read for the context.
    pub fn reading_details(&self) -> bool {
        self.details_reading > 0
    }

    /// Reads the structures of the tables the message is about that were not asked for yet
    /// (K50): off the UI thread, on the catalog's session, at most [`ai::MAX_DETAILED`]. A
    /// table that cannot be read stays described by the catalog's names and types.
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
            });
        }));
    }

    /// The exact payload Send would send now; `None` while there is nothing to send, or the
    /// sample rows or the structures are being read.
    pub fn payload(&self, cx: &App) -> Option<serde_json::Value> {
        if self.reading_details() || self.waiting_catalog() {
            return None;
        }
        let message = self.message(cx)?;
        let mut context = self.context.clone();
        if self.include_samples {
            context.sample_rows = Some(self.samples.clone()?.ok()?);
        }
        Some(ai::build_chat(
            &context,
            &self.turns(),
            &message,
            &AppState::get(cx).settings.ai.model,
        ))
    }

    /// Ticks or unticks "Include sample rows"; ticked, the rows are read now.
    pub fn set_include_samples(&mut self, on: bool, cx: &mut Context<Self>) {
        self.include_samples = on;
        if on {
            self.load_samples(cx);
        }
        cx.notify();
    }

    #[cfg(test)]
    pub fn include_samples(&self) -> bool {
        self.include_samples
    }

    /// Takes the editor's query off the message being written, or puts it back.
    pub fn set_include_query(&mut self, on: bool, cx: &mut Context<Self>) {
        self.include_query = on;
        self.refresh(cx);
    }

    /// Reads the first rows of the tables described, when they changed: on the connection's
    /// own session, never recorded in the history.
    fn load_samples(&mut self, cx: &mut Context<Self>) {
        let tables = self.context.sample_tables();
        if tables == self.samples_for && self.samples.is_some() {
            return;
        }
        self.samples_for = tables.clone();
        let Some(session) = self.facts.session.clone() else {
            self.samples = Some(Err(
                "Connect first: the rows are read from the database.".into()
            ));
            return;
        };
        self.samples = None;
        let reading = bridge::spawn(cx, async move {
            let mut samples = Vec::new();
            for table in tables {
                let sql = ai::sample_sql(session.dialect(), &table);
                let collected = exec::collect(session.clone(), sql).await.map_err(|error| {
                    let error = kerno_core::describe(&error);
                    format!("{}.{}: {error}", table.schema, table.name)
                })?;
                let result = collected.results.into_iter().next().unwrap_or_default();
                samples.push(Sample {
                    table,
                    columns: result.columns.iter().map(|c| c.name.clone()).collect(),
                    rows: result.rows(),
                });
            }
            Ok(samples)
        });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let samples = reading.await;
            let _ = this.update(cx, |this, cx| {
                this.samples = Some(samples);
                cx.notify();
            });
        }));
    }

    /// Sends the message being written with the earlier turns, through `kerno_core::ai`, off
    /// the UI thread; the answer is shown as it arrives. Then the field is emptied, the subject
    /// chip and the sample rows go, the editor's query is ticked again.
    pub fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sending() {
            return;
        }
        let settings: AiSettings = AppState::get(cx).settings.ai.clone();
        let failed = |this: &mut Self, why: String, cx: &mut Context<Self>| {
            this.entries.push(Entry::Note {
                text: why,
                failed: true,
            });
            this.thread.scroll_to_bottom();
            cx.notify();
        };
        let endpoint = match ai::gate(&settings) {
            Ok(endpoint) => endpoint,
            Err(error) => return failed(self, error.to_string(), cx),
        };
        if settings.model.trim().is_empty() {
            return failed(
                self,
                "Choose a model in Settings → AI (List models asks the endpoint). Nothing was sent."
                    .into(),
                cx,
            );
        }
        // The tab as it is now; when that needs structures not read yet, nothing goes until
        // they are (the preview says so), as for the sample rows.
        self.facts_again(cx);
        self.read_editor(cx);
        self.refresh(cx);
        let (Some(message), Some(payload)) = (self.message(cx), self.payload(cx)) else {
            return;
        };
        self.generation += 1;
        let generation = self.generation;
        cx.default_global::<SentLog>().0.push(Sent {
            at_ms: now_ms(),
            endpoint: format!("{}/chat/completions", endpoint.base),
            key: self.has_key,
            body: ai::preview(&payload),
        });
        let typed = self.input.read(cx).value().trim().to_string();
        let mut chips = Vec::new();
        if let Some(action) = &self.action {
            chips.push(subject(action));
        }
        if self.query_attached().is_some() {
            chips.push("The editor's query".to_string());
        }
        if self.include_samples {
            chips.push("Sample rows".to_string());
        }
        if let Some(asked) = &self.asked {
            chips.extend(asked.sent_chips());
        }
        self.entries.push(Entry::Asked {
            shown: typed,
            chips,
            sent: message,
        });
        self.entries.push(Entry::Answer {
            text: String::new(),
            done: false,
        });
        let secrets = AppState::get(cx).secrets.clone();
        let key_name = ai::key_name(&endpoint.base);
        let stop = ai::Stopper::default();
        let stopper = stop.clone();
        let (pieces, mut arriving) = tokio::sync::mpsc::unbounded_channel::<String>();
        let asking = bridge::spawn_blocking(cx, move || {
            let key: Option<Secret> = secrets.get(&key_name).ok().flatten();
            ai::chat_stream(&settings, key.as_ref(), &payload, &stopper, &mut |piece| {
                let _ = pieces.send(piece.to_string());
            })
        });
        let task = cx.spawn_in(window, async move |this, cx| {
            // The pieces as they come, then the whole answer.
            while let Some(piece) = arriving.recv().await {
                let mut piece = piece;
                while let Ok(more) = arriving.try_recv() {
                    piece.push_str(&more);
                }
                let live = this.update(cx, |this, cx| {
                    if this.generation != generation {
                        return false;
                    }
                    if let Some(Entry::Answer { text, done: false }) = this.entries.last_mut() {
                        text.push_str(&piece);
                    }
                    this.thread.scroll_to_bottom();
                    cx.notify();
                    true
                });
                if !matches!(live, Ok(true)) {
                    return;
                }
            }
            let answer = asking.await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.stage = Stage::Ready;
                match answer {
                    Ok(answer) => {
                        if let Some(Entry::Answer { text, done }) = this.entries.last_mut() {
                            *text = answer;
                            *done = true;
                        }
                    }
                    Err(error) => {
                        this.entries.pop();
                        this.entries.push(Entry::Note {
                            text: error.to_string(),
                            failed: true,
                        });
                    }
                }
                this.thread.scroll_to_bottom();
                this.refresh(cx);
            });
        });
        self.stage = Stage::Sending {
            host: endpoint.host,
            stop,
            _task: task,
        };
        self.action = None;
        self.new_tab |= self.asked.take().is_some_and(|asked| asked.test_data());
        self.include_samples = false;
        self.include_query = true;
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.thread.scroll_to_bottom();
        self.refresh(cx);
    }

    /// Stops the request: its connection is closed at once (C3), so the endpoint sees the
    /// client go and the call ends on its thread; what arrived of the answer is dropped (only
    /// whole answers are kept and sent again), and a new request goes on a connection of its own.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        if let Stage::Sending { stop, .. } = &self.stage {
            stop.stop();
            self.generation += 1;
            self.stage = Stage::Ready;
            if matches!(self.entries.last(), Some(Entry::Answer { done: false, .. })) {
                self.entries.pop();
            }
            self.entries.push(Entry::Note {
                text: "Stopped: the request was closed, nothing was inserted.".into(),
                failed: false,
            });
            cx.notify();
        }
    }

    /// The last answer, once it came whole; for tests.
    #[cfg(test)]
    pub fn answer(&self) -> Option<&str> {
        match self.entries.last() {
            Some(Entry::Answer { text, done: true }) => Some(text),
            _ => None,
        }
    }

    /// Why the last request failed or was not sent.
    #[cfg(test)]
    pub fn failure(&self) -> Option<&str> {
        match self.entries.last() {
            Some(Entry::Note { text, failed: true }) => Some(text),
            _ => None,
        }
    }

    /// The thread as the user reads it: (who, text) of each message; for tests.
    #[cfg(test)]
    pub fn thread(&self) -> Vec<(&'static str, String)> {
        (self.entries.iter())
            .map(|entry| match entry {
                Entry::Asked { shown, .. } => ("you", shown.clone()),
                Entry::Answer { text, .. } => ("assistant", ai::strip_reasoning(text)),
                Entry::Note { text, .. } => ("note", text.clone()),
            })
            .collect()
    }

    /// How many earlier turns the next send leaves out; for tests.
    #[cfg(test)]
    pub fn left_out(&self) -> usize {
        ai::left_out(&self.turns())
    }

    pub fn sending(&self) -> bool {
        matches!(self.stage, Stage::Sending { .. })
    }

    /// Puts the last answer's first SQL block in the editor ([`Self::insert_sql`]); for tests.
    #[cfg(test)]
    pub fn insert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sql) = self.answer().map(ai::sql_block) else {
            return;
        };
        self.insert_sql(&sql, window, cx);
    }

    /// Puts `sql` in the editor as one undoable edit, selected, in place of the selection or on
    /// its own line at the cursor ([`ai::placed`]), and runs nothing.
    pub fn insert_sql(&mut self, sql: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |editor, cx| {
            let range = editor.selected_range();
            let (text, start) = ai::placed(&editor.text().to_string(), range.clone(), sql);
            editor.set_selected_range(range.clone(), cx);
            editor.replace(text, window, cx);
            let start = range.start + start;
            editor.set_selected_range(start..start + sql.len(), cx);
        });
        self.focus_editor(window, cx);
    }

    /// Puts `sql` in place of the editor's selection, else of the statement at the cursor (as
    /// Run statement takes it), else at the cursor: one undoable edit, selected; runs nothing.
    pub fn replace_sql(&mut self, sql: &str, window: &mut Window, cx: &mut Context<Self>) {
        let target = self
            .tab
            .upgrade()
            .and_then(|tab| tab.read(cx).ai_statement(cx))
            .map(|(_, range)| range);
        let Some(range) = target else {
            return self.insert_sql(sql, window, cx);
        };
        self.editor.update(cx, |editor, cx| {
            editor.set_selected_range(range.clone(), cx);
            editor.replace(sql.to_string(), window, cx);
            editor.set_selected_range(range.start..range.start + sql.len(), cx);
        });
        self.focus_editor(window, cx);
    }

    fn focus_editor(&self, window: &mut Window, cx: &mut App) {
        let focus = self.editor.read(cx).focus_handle(cx);
        focus.focus(window, cx);
    }

    /// The preview's text: the request line, the headers that matter, then the body.
    pub fn preview_text(&self, cx: &App) -> String {
        let settings = &AppState::get(cx).settings.ai;
        let address = ai::endpoint(&settings.endpoint)
            .map(|endpoint| format!("{}/chat/completions", endpoint.base))
            .unwrap_or_else(|_| "(no endpoint set)".into());
        let body = match self.payload(cx) {
            Some(payload) => ai::preview(&payload),
            None if self.reading_details() => "(reading the tables' keys and indexes…)".into(),
            None if self.waiting_catalog() => "(reading the database's tables…)".into(),
            None if self.message(cx).is_none() => {
                "(nothing to send yet: write a message first)".into()
            }
            None => "(reading the sample rows…)".into(),
        };
        let key = if self.has_key {
            "\nAuthorization: Bearer ***"
        } else {
            ""
        };
        format!("POST {address}\nContent-Type: application/json{key}\n\n{body}")
    }

    /// The picker at the top of the panel: "endpoint · model", a menu of the saved endpoints
    /// and of the active one's models (K23). Not while a request is on its way.
    fn picker(&self, cx: &Context<Self>) -> impl IntoElement {
        let mut settings = AppState::get(cx).settings.ai.clone();
        settings.adopt_legacy();
        let shown = match settings.active() {
            Some(saved) if !settings.model.is_empty() => {
                format!("{} · {}", saved.name, settings.model)
            }
            Some(saved) => format!("{} · no model chosen", saved.name),
            None => "No endpoint: add one in Settings → AI".into(),
        };
        let menu = move |mut menu: PopupMenu, window: &mut Window, _: &mut Context<PopupMenu>| {
            for saved in &settings.endpoints {
                let base = saved.base.clone();
                let model = match saved.model.as_str() {
                    "" => "no model chosen",
                    model => model,
                };
                menu = menu.item(
                    PopupMenuItem::new(SharedString::from(format!("{} · {model}", saved.name)))
                        .checked(saved.base == settings.endpoint)
                        .on_click(move |_, _, cx| {
                            let base = base.clone();
                            AppState::update_settings(cx, |settings| {
                                settings.ai.adopt_legacy();
                                settings.ai.activate(&base)
                            });
                        }),
                );
            }
            if let Some(active) = settings.active()
                && !active.models.is_empty()
            {
                menu = menu.separator();
                for model in &active.models {
                    let id = model.id.clone();
                    menu = menu.item(
                        PopupMenuItem::new(SharedString::from(model.shown()))
                            .checked(model.id == settings.model)
                            .on_click(move |_, _, cx| {
                                let id = id.clone();
                                AppState::update_settings(cx, |settings| {
                                    settings.ai.adopt_legacy();
                                    settings.ai.set_model(&id)
                                });
                            }),
                    );
                }
            }
            crate::tabs::fit_window(menu, window)
        };
        div().id("ai-picker").test_support().child(
            Button::new("ai-endpoint-picker")
                .outline()
                .xsmall()
                .label(SharedString::from(shown))
                .dropdown_caret(true)
                .disabled(self.sending())
                .dropdown_menu(menu),
        )
    }

    /// The thread's messages, or what the panel does while it is empty.
    fn render_thread(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted, danger, border) = (theme.muted_foreground, theme.danger, theme.border);
        if self.entries.is_empty() {
            return div()
                .id("ai-empty")
                .test_support()
                .text_sm()
                .text_color(muted)
                .child(
                    "Ask a question about this tab's database, or describe a query to write. \
                     Follow-ups refine the answer. Each message goes with the names of the \
                     schemas, the tables it is about with their columns, keys and indexes, the \
                     earlier messages and, unless you remove it, the editor's query. Rows only \
                     when you tick Include sample rows. Nothing an answer says runs by itself.",
                )
                .into_any_element();
        }
        let left_out = ai::left_out(&self.turns());
        let last_answer =
            (self.entries.iter()).rposition(|entry| matches!(entry, Entry::Answer { .. }));
        let mut asked = 0;
        let mut items: Vec<AnyElement> = Vec::new();
        for (index, entry) in self.entries.iter().enumerate() {
            if let Entry::Asked { .. } = entry {
                // Each answered question is two turns; the marker goes before the first kept.
                if left_out > 0 && asked * 2 == left_out && self.answered(index) {
                    items.push(
                        div()
                            .id("ai-left-out")
                            .test_support()
                            .py_1()
                            .border_t_1()
                            .border_color(border)
                            .text_xs()
                            .text_color(muted)
                            .child("Earlier messages are no longer sent: the conversation is longer than what a request carries.")
                            .into_any_element(),
                    );
                }
                if self.answered(index) {
                    asked += 1;
                }
            }
            let last = Some(index) == last_answer;
            items.push(match entry {
                Entry::Asked { shown, chips, .. } => v_flex()
                    .id(("ai-asked", index))
                    .test_support()
                    .self_end()
                    .max_w(gpui_kit::relative(0.9))
                    .gap_1()
                    .p_2()
                    .rounded(theme.radius)
                    .bg(theme.muted)
                    .when(!shown.is_empty(), |message| {
                        message.child(div().text_sm().child(SharedString::from(shown.clone())))
                    })
                    .children(chips.iter().map(|chip| {
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(SharedString::from(chip.clone()))
                    }))
                    .into_any_element(),
                Entry::Answer { text, done } => self.render_answer(index, text, *done, last, cx),
                Entry::Note { text, failed } => div()
                    .id(if *failed { "ai-error" } else { "ai-stopped" })
                    .test_support()
                    .text_xs()
                    .text_color(if *failed { danger } else { muted })
                    .child(SharedString::from(text.clone()))
                    .into_any_element(),
            });
        }
        v_flex().gap_3().children(items).into_any_element()
    }

    /// Whether the question at `index` was answered in full (it then goes with later messages).
    fn answered(&self, index: usize) -> bool {
        matches!(
            self.entries.get(index + 1),
            Some(Entry::Answer { done: true, .. })
        )
    }

    /// An answer: prose as text, code blocks as code; under a whole answer's SQL blocks Insert,
    /// Replace and Copy. The last answer's controls keep plain ids, earlier ones are numbered.
    fn render_answer(
        &self,
        index: usize,
        text: &str,
        done: bool,
        last: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (muted, border, mono) = (
            theme.muted_foreground,
            theme.border,
            theme.mono_font_family.clone(),
        );
        let shown = ai::strip_reasoning(text);
        if !done && shown.is_empty() {
            return div()
                .id(("ai-waiting", index))
                .text_xs()
                .text_color(muted)
                .child("Waiting for the answer…")
                .into_any_element();
        }
        let id = |name: &'static str, n: usize| -> gpui_kit::ElementId {
            if last && n == 0 {
                name.into()
            } else {
                (name, index * 1000 + n).into()
            }
        };
        let mut sql_blocks = 0;
        let parts: Vec<AnyElement> = ai::segments(&shown)
            .into_iter()
            .map(|part| match &part {
                Segment::Text(prose) => div()
                    .text_sm()
                    .child(SharedString::from(prose.clone()))
                    .into_any_element(),
                Segment::Code { code, .. } => {
                    let sql = part.sql().filter(|_| done).map(str::to_string);
                    let n = sql_blocks;
                    if sql.is_some() {
                        sql_blocks += 1;
                    }
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .p_2()
                                .rounded(theme.radius)
                                .bg(theme.muted)
                                .font_family(mono.clone())
                                .text_xs()
                                .child(SharedString::from(code.clone())),
                        )
                        .children(sql.map(|sql| {
                            let new_tab = self.new_tab_button(id("ai-new-tab", n), sql.clone(), cx);
                            let (insert, replace, copy) = (sql.clone(), sql.clone(), sql);
                            let tip = replace_tip(self.editor_query.as_deref());
                            h_flex()
                                .gap_1()
                                .child(
                                    Button::new(id("ai-insert", n))
                                        .primary()
                                        .xsmall()
                                        .label("Insert")
                                        .tooltip(
                                            "In place of the editor's selection, or at the cursor",
                                        )
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.insert_sql(&insert, window, cx)
                                        })),
                                )
                                .child(
                                    Button::new(id("ai-replace", n))
                                        .outline()
                                        .xsmall()
                                        .label("Replace")
                                        .tooltip(tip)
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.replace_sql(&replace, window, cx)
                                        })),
                                )
                                .child(
                                    Button::new(id("ai-copy-sql", n))
                                        .ghost()
                                        .xsmall()
                                        .label("Copy")
                                        .on_click(move |_, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                copy.clone(),
                                            ))
                                        }),
                                )
                                .children(new_tab)
                        }))
                        .into_any_element()
                }
            })
            .collect();
        let copied = ai::unfenced(&shown);
        v_flex()
            .id(("ai-entry", index))
            .gap_2()
            .child(
                v_flex()
                    .id(id("ai-answer", 0))
                    .test_support()
                    .gap_2()
                    .p_2()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(border)
                    .test_text(ai::unfenced(&shown))
                    .children(parts),
            )
            .when(done, |answer| {
                answer.child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new(id("ai-copy", 0))
                                .ghost()
                                .xsmall()
                                .label("Copy answer")
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()))
                                }),
                        )
                        .when(last, |row| {
                            // Wraps in a narrow panel.
                            row.child(div().flex_1().min_w_0().text_xs().text_color(muted).child(
                                "The model's text: check it. SQL runs only when you run it.",
                            ))
                        }),
                )
            })
            .into_any_element()
    }
}

/// What Replace replaces, for its tooltip: the editor's selection or statement as last read
/// (when it moved), not split again at each frame.
fn replace_tip(statement: Option<&str>) -> SharedString {
    SharedString::from(match statement {
        Some(sql) => format!(
            "Replaces the editor's selection, else the statement at the cursor: {}",
            first_line(sql)
        ),
        None => "No statement at the cursor: inserts at the cursor".to_string(),
    })
}

/// A statement's first line, short.
fn first_line(sql: &str) -> String {
    let line = sql.trim().lines().next().unwrap_or_default();
    if line.chars().count() > 60 {
        format!("{}…", line.chars().take(60).collect::<String>())
    } else {
        line.to_string()
    }
}

/// The chip of an action's subject.
fn subject(action: &Action) -> String {
    match action {
        Action::WriteSql { .. } => "Write SQL".to_string(),
        Action::ExplainQuery { sql } => format!("Explain query: {}", first_line(sql)),
        Action::ExplainPlan { sql, .. } => format!("Explain the plan of: {}", first_line(sql)),
        Action::ExplainError { error, .. } => format!("Explain error: {}", first_line(error)),
    }
}

impl Focusable for AiPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Render for AiPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (muted, danger, border, mono) = (
            theme.muted_foreground,
            theme.danger,
            theme.border,
            theme.mono_font_family.clone(),
        );
        let on = AppState::get(cx).settings.ai.enabled;
        let described = (self.context.tables.iter())
            .filter(|t| !t.columns.is_empty())
            .count();
        let sending = self.sending();
        let nothing = self.message(cx).is_none();
        let reading = self.include_samples && self.samples.is_none()
            || self.reading_details()
            || self.waiting_catalog();
        let samples_note = match &self.samples {
            Some(Err(why)) if self.include_samples => Some(why.clone()),
            _ => None,
        };
        let mut chips: Vec<AnyElement> = Vec::new();
        if let Some(action) = &self.action {
            chips.push(
                chip("ai-subject", subject(action), cx, |this, cx| {
                    this.action = None;
                    this.refresh(cx);
                })
                .into_any_element(),
            );
        }
        if let Some(sql) = self.query_attached() {
            chips.push(
                chip(
                    "ai-editor-query",
                    format!("The editor's query: {}", first_line(sql)),
                    cx,
                    |this, cx| this.set_include_query(false, cx),
                )
                .into_any_element(),
            );
        }
        chips.extend(self.asked_chips(cx));
        let status: Option<AnyElement> = match &self.stage {
            Stage::Sending { host, .. } => Some(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(muted)
                            .child(SharedString::from(format!("Waiting for {host}…"))),
                    )
                    .child(
                        Button::new("ai-stop")
                            .outline()
                            .small()
                            .label("Stop")
                            .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
                    )
                    .into_any_element(),
            ),
            Stage::Ready => None,
        };
        v_flex()
            .id("ai-panel")
            .key_context("AiPanel")
            .test_support()
            .size_full()
            .p_3()
            .gap_2()
            .border_l_1()
            .border_color(border)
            .bg(theme.background)
            .text_sm()
            // Escape in the message field: the keyboard back to the editor.
            .on_action(cx.listener(
                |this, _: &gpui_kit::component::input::Escape, window, cx| {
                    this.focus_editor(window, cx)
                },
            ))
            .child(head(Some(cx), cx))
            .child(self.picker(cx))
            .when(!on, |panel| {
                panel.child(
                    div()
                        .id("ai-off")
                        .test_support()
                        .text_xs()
                        .text_color(danger)
                        .child(SharedString::from(format!(
                            "{OFF_NOTICE} Nothing is sent while it is off."
                        ))),
                )
            })
            .child(
                div()
                    .id("ai-thread")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.thread)
                    .child(self.render_thread(cx)),
            )
            .children(status)
            .children(chips)
            .child(
                div()
                    .id("ai-sentence")
                    .test_support()
                    .child(Textarea::new(&self.input)),
            )
            .child(
                div().text_xs().text_color(muted).child(SharedString::from(format!(
                    "Sent with it: the schema names and {described} table{} with their columns, keys and indexes{}.",
                    if described == 1 { "" } else { "s" },
                    if self.include_samples { ", and their first rows" } else { "" }
                ))),
            )
            // A result's own rows have their tick (K53): not the tables' sample rows too.
            .when(self.asked.as_ref().is_none_or(|a| a.ask.values().is_none()), |panel| panel.child(
                Checkbox::new("ai-sample-rows")
                    .label("Include sample rows (first 5 rows of the tables above)")
                    .checked(self.include_samples)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.set_include_samples(*checked, cx)
                    })),
            ))
            .children(samples_note.map(|why| div().text_xs().text_color(danger).child(SharedString::from(why))))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("ai-preview-toggle")
                            .ghost()
                            .small()
                            .label(if self.show_preview {
                                "Hide what is sent"
                            } else {
                                "What is sent…"
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_preview = !this.show_preview;
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("ai-send")
                            .primary()
                            .small()
                            .label("Send")
                            .tooltip("Enter sends, Shift+Enter makes a new line")
                            .disabled(!on || sending || nothing || reading)
                            .on_click(cx.listener(|this, _, window, cx| this.send(window, cx))),
                    ),
            )
            .when(self.show_preview, |panel| {
                panel.child(
                    div()
                        .id("ai-preview")
                        .test_support()
                        .max_h(px(280.))
                        .overflow_y_scroll()
                        .p_2()
                        .rounded(theme.radius)
                        .bg(theme.muted)
                        .font_family(mono.clone())
                        .text_xs()
                        .child(SharedString::from(self.preview_text(cx))),
                )
            })
    }
}

/// A chip of what goes with the message, with × to take it off.
fn chip(
    id: &'static str,
    text: String,
    cx: &Context<AiPanel>,
    remove: impl Fn(&mut AiPanel, &mut Context<AiPanel>) + 'static,
) -> impl IntoElement {
    let theme = cx.theme();
    h_flex()
        .id(id)
        .test_support()
        .gap_1()
        .px_2()
        .py_0p5()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .text_xs()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(SharedString::from(text)),
        )
        .child(
            Button::new((id, 1usize))
                .ghost()
                .xsmall()
                .label("×")
                .tooltip("Do not send it with this message")
                .on_click(cx.listener(move |this, _, _, cx| remove(this, cx))),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_the_entries_say_how_to_turn_the_assistant_on() {
        let off = menu_entries(false);
        assert_eq!(off.len(), 4);
        for (label, _, enabled) in &off {
            assert!(!enabled);
            assert!(label.ends_with("(turn on in Settings → AI)"), "{label}");
        }
        let on = menu_entries(true);
        let labels: Vec<&str> = on.iter().map(|(label, _, _)| label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "AI: Write SQL…",
                "AI: Explain query",
                "AI: Explain plan",
                "AI: Explain error"
            ]
        );
        assert!(on.iter().all(|(_, _, enabled)| *enabled));
        assert_eq!((on[0].1)().name(), "kerno::AiWriteSql");
    }
}
