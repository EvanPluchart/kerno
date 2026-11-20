//! The AI assistant's screens (CLAUDE.md §4 J17, DBeaver #36951, #20072; D-068): the answer
//! panel at the side of a query tab, the AI entries of the editor's menu, and the session's
//! "What was sent" log (Settings → AI is `ai_settings_view`). What is sent and the one function
//! that sends are `kerno_core::ai`: this file only gathers what the user chose and shows the
//! answer, as text. An answer runs nothing: its SQL is inserted in the editor when the user
//! clicks Insert.

use std::sync::Arc;

use gpui_kit::base::TestTextExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{EditorState, Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, Focusable as _, FontWeight,
    Global, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, TestSupportExt as _,
    WeakEntity, Window, div, px,
};
use kerno_core::ai::{self, Action, AiSettings, Context as AiContext, Sample};
use kerno_core::schema::TableRef;
use kerno_core::sql::Catalog;
use kerno_core::storage::now_ms;
use kerno_core::{Secret, exec};

use crate::actions;
use crate::bridge;
use crate::query::tab::QueryTab;
use crate::state::AppState;

/// What an entry says while the assistant is off.
pub const OFF: &str = "turn on in Settings → AI";

/// What an "AI: …" command says while the assistant is off, wherever it runs: what turning it
/// on gives, in the words of the top of Settings → AI (K23).
pub const OFF_NOTICE: &str = "The AI assistant is off. Turned on in Settings → AI, it writes SQL \
     from a sentence (never run by itself) and explains a query, a plan or an error, sending \
     only names and types to the endpoint you choose.";

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
    /// The session in the catalog's database, for the sample rows.
    pub session: Option<Arc<dyn kerno_core::Session>>,
}

enum Stage {
    Ready,
    Sending {
        host: String,
        /// Closes the request's connection (Stop, C3).
        stop: ai::Stopper,
        _task: Task<()>,
    },
    Answered(String),
    Failed(String),
    Stopped,
}

/// The assistant at the side of a query tab: one request, its preview, its answer.
pub struct AiPanel {
    tab: WeakEntity<QueryTab>,
    editor: Entity<EditorState>,
    action: Action,
    facts: Facts,
    /// The sentence of "Write SQL".
    sentence: Option<Entity<InputState>>,
    /// "Include sample rows": off at every new request.
    include_samples: bool,
    /// The rows read for the tick, for these tables; `None` while reading.
    samples: Option<Result<Vec<Sample>, String>>,
    samples_for: Vec<TableRef>,
    show_preview: bool,
    /// An API key is stored for the endpoint (read off the UI thread, again when the active
    /// endpoint changes).
    has_key: bool,
    /// The endpoint `has_key` was read for.
    key_for: String,
    stage: Stage,
    /// Bumped by Stop and each request: a late answer is dropped.
    generation: u64,
    tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl AiPanel {
    pub fn new(
        tab: WeakEntity<QueryTab>,
        editor: Entity<EditorState>,
        action: Action,
        facts: Facts,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut subscriptions = Vec::new();
        let sentence = matches!(action, Action::WriteSql { .. }).then(|| {
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Describe the query, e.g. the ten customers who ordered most")
            });
            subscriptions.push(cx.subscribe_in(
                &input,
                window,
                |this: &mut Self, _, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => {
                        if this.include_samples {
                            this.load_samples(cx);
                        }
                        cx.notify();
                    }
                    InputEvent::PressEnter { .. } => this.send(window, cx),
                    _ => {}
                },
            ));
            input.update(cx, |input, cx| input.focus(window, cx));
            input
        });
        // Switched in Settings or in the picker: the key of the new endpoint, for the
        // preview's and the log's `Authorization` line.
        subscriptions.push(cx.observe_global::<AppState>(|this: &mut Self, cx| {
            if AppState::get(cx).settings.ai.endpoint != this.key_for {
                this.read_key(cx);
                cx.notify();
            }
        }));
        let mut panel = Self {
            tab,
            editor,
            action,
            facts,
            sentence,
            include_samples: false,
            samples: None,
            samples_for: Vec::new(),
            show_preview: false,
            has_key: false,
            key_for: String::new(),
            stage: Stage::Ready,
            generation: 0,
            tasks: Vec::new(),
            _subscriptions: subscriptions,
        };
        panel.read_key(cx);
        panel
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

    /// The request as it stands: the sentence typed so far for "Write SQL".
    pub fn action(&self, cx: &App) -> Action {
        match &self.sentence {
            Some(input) => Action::WriteSql {
                sentence: input.read(cx).value().to_string(),
            },
            None => self.action.clone(),
        }
    }

    /// What the model is told of the database, without rows.
    fn context(&self, cx: &App) -> AiContext {
        AiContext::from_catalog(
            &self.facts.catalog,
            &self.facts.engine,
            self.facts.server_version.clone(),
            self.facts.database.clone(),
            self.facts.schema.as_deref(),
            &self.action(cx),
        )
    }

    /// The exact payload Send would send; `None` while the sample rows are being read.
    pub fn payload(&self, cx: &App) -> Option<serde_json::Value> {
        let mut context = self.context(cx);
        if self.include_samples {
            context.sample_rows = Some(self.samples.clone()?.ok()?);
        }
        Some(ai::build_payload(
            &self.action(cx),
            &context,
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

    /// Reads the first rows of the tables described, when they changed: on the connection's
    /// own session, never recorded in the history.
    fn load_samples(&mut self, cx: &mut Context<Self>) {
        let tables = self.context(cx).sample_tables();
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

    /// Sends the request shown in the preview, through `kerno_core::ai::chat`, off the UI
    /// thread. The tick goes back off: the next request starts without rows.
    pub fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.stage, Stage::Sending { .. }) {
            return;
        }
        let settings: AiSettings = AppState::get(cx).settings.ai.clone();
        let endpoint = match ai::gate(&settings) {
            Ok(endpoint) => endpoint,
            Err(error) => {
                self.stage = Stage::Failed(error.to_string());
                cx.notify();
                return;
            }
        };
        if settings.model.trim().is_empty() {
            self.stage = Stage::Failed(
                "Choose a model in Settings → AI (List models asks the endpoint). Nothing was sent."
                    .into(),
            );
            cx.notify();
            return;
        }
        if let Action::WriteSql { sentence } = self.action(cx)
            && sentence.trim().is_empty()
        {
            return;
        }
        let Some(payload) = self.payload(cx) else {
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
        let secrets = AppState::get(cx).secrets.clone();
        let key_name = ai::key_name(&endpoint.base);
        let stop = ai::Stopper::default();
        let stopper = stop.clone();
        let asking = bridge::spawn_blocking(cx, move || {
            let key: Option<Secret> = secrets.get(&key_name).ok().flatten();
            ai::chat_until(&settings, key.as_ref(), &payload, &stopper)
        });
        let task = cx.spawn_in(window, async move |this, cx| {
            let answer = asking.await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.stage = match answer {
                    Ok(answer) => Stage::Answered(answer),
                    Err(error) => Stage::Failed(error.to_string()),
                };
                cx.notify();
            });
        });
        self.stage = Stage::Sending {
            host: endpoint.host,
            stop,
            _task: task,
        };
        self.include_samples = false;
        cx.notify();
    }

    /// Stops the request: its connection is closed at once (C3), so the endpoint sees the
    /// client go and the call ends on its thread; nothing of an answer reaches the panel, and a
    /// new request goes on a connection of its own.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        if let Stage::Sending { stop, .. } = &self.stage {
            stop.stop();
            self.generation += 1;
            self.stage = Stage::Stopped;
            cx.notify();
        }
    }

    /// The answer, once it came.
    pub fn answer(&self) -> Option<&str> {
        match &self.stage {
            Stage::Answered(answer) => Some(answer),
            _ => None,
        }
    }

    /// Why the request failed or was not sent.
    #[cfg(test)]
    pub fn failure(&self) -> Option<&str> {
        match &self.stage {
            Stage::Failed(why) => Some(why),
            _ => None,
        }
    }

    pub fn sending(&self) -> bool {
        matches!(self.stage, Stage::Sending { .. })
    }

    /// Puts the answer's SQL in the editor as one undoable edit, selected, and runs nothing:
    /// for "Write SQL" in place of the selection or on its own line at the cursor
    /// ([`ai::placed`]), after the text as a new statement for a proposed fix.
    pub fn insert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(answer) = self.answer() else {
            return;
        };
        let sql = ai::sql_block(answer);
        let at_end = !matches!(self.action, Action::WriteSql { .. });
        self.editor.update(cx, |editor, cx| {
            let (range, text, start) = if at_end {
                let at = editor.text().len();
                let prefix = if at > 0 { "\n\n" } else { "" };
                (at..at, format!("{prefix}{sql}"), prefix.len())
            } else {
                let range = editor.selected_range();
                let (text, start) = ai::placed(&editor.text().to_string(), range.clone(), &sql);
                (range, text, start)
            };
            editor.set_selected_range(range.clone(), cx);
            editor.replace(text, window, cx);
            let start = range.start + start;
            editor.set_selected_range(start..start + sql.len(), cx);
        });
        let focus = self.editor.read(cx).focus_handle(cx);
        focus.focus(window, cx);
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        let _ = self.tab.update(cx, |tab, cx| tab.close_ai(cx));
    }

    /// The preview's text: the request line, the headers that matter, then the body.
    pub fn preview_text(&self, cx: &App) -> String {
        let settings = &AppState::get(cx).settings.ai;
        let address = ai::endpoint(&settings.endpoint)
            .map(|endpoint| format!("{}/chat/completions", endpoint.base))
            .unwrap_or_else(|_| "(no endpoint set)".into());
        let body = match self.payload(cx) {
            Some(payload) => ai::preview(&payload),
            None => "(reading the sample rows…)".into(),
        };
        let key = if self.has_key {
            "\nAuthorization: Bearer ***"
        } else {
            ""
        };
        format!("POST {address}\nContent-Type: application/json{key}\n\n{body}")
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
        let context = self.context(cx);
        let described = context
            .tables
            .iter()
            .filter(|t| !t.columns.is_empty())
            .count();
        let sending = self.sending();
        let empty_sentence =
            matches!(self.action(cx), Action::WriteSql { sentence } if sentence.trim().is_empty());
        let reading = self.include_samples && self.samples.is_none();
        let subject: Option<AnyElement> = match &self.action {
            Action::WriteSql { .. } => self.sentence.as_ref().map(|input| {
                div()
                    .id("ai-sentence")
                    .test_support()
                    .child(Input::new(input))
                    .into_any_element()
            }),
            Action::ExplainQuery { sql } | Action::ExplainPlan { sql, .. } => Some(
                div()
                    .id("ai-subject")
                    .test_support()
                    .max_h(px(120.))
                    .overflow_y_scroll()
                    .font_family(mono.clone())
                    .text_xs()
                    .child(SharedString::from(sql.clone()))
                    .into_any_element(),
            ),
            Action::ExplainError { error, .. } => Some(
                div()
                    .id("ai-subject")
                    .test_support()
                    .max_h(px(120.))
                    .overflow_y_scroll()
                    .text_xs()
                    .text_color(danger)
                    .child(SharedString::from(error.clone()))
                    .into_any_element(),
            ),
        };
        let samples_note = match &self.samples {
            Some(Err(why)) if self.include_samples => Some(why.clone()),
            _ => None,
        };
        let stage: AnyElement = match &self.stage {
            Stage::Ready => div().into_any_element(),
            Stage::Sending { host, .. } => h_flex()
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
            Stage::Stopped => div()
                .id("ai-stopped")
                .test_support()
                .text_xs()
                .text_color(muted)
                .child("Stopped: the request was closed, nothing was inserted.")
                .into_any_element(),
            Stage::Failed(why) => div()
                .id("ai-error")
                .test_support()
                .text_xs()
                .text_color(danger)
                .child(SharedString::from(why.clone()))
                .into_any_element(),
            Stage::Answered(answer) => {
                let insert = match self.action {
                    Action::WriteSql { .. } => Some("Insert"),
                    Action::ExplainError { .. } => Some("Insert as new statement"),
                    _ => None,
                };
                let shown = ai::unfenced(answer);
                let copied = shown.clone();
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .id("ai-answer")
                            .test_support()
                            .p_2()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(border)
                            .text_sm()
                            .test_text(shown.clone())
                            .child(SharedString::from(shown)),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("ai-copy")
                                    .outline()
                                    .small()
                                    .label("Copy")
                                    .on_click(move |_, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()))
                                    }),
                            )
                            .children(insert.map(|label| {
                                Button::new("ai-insert")
                                    .primary()
                                    .small()
                                    .label(label)
                                    .on_click(cx.listener(|this, _, window, cx| this.insert(window, cx)))
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child("The answer is the model's text: check it. Inserted SQL runs only when you run it."),
                    )
                    .into_any_element()
            }
        };
        v_flex()
            .id("ai-panel")
            .test_support()
            .w(px(380.))
            .flex_none()
            .h_full()
            .p_3()
            .gap_2()
            .border_l_1()
            .border_color(border)
            .bg(theme.background)
            .text_sm()
            .overflow_y_scroll()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .font_weight(FontWeight::MEDIUM)
                            .child(SharedString::from(format!("AI · {}", self.action.label()))),
                    )
                    .child(
                        Button::new("ai-close")
                            .ghost()
                            .xsmall()
                            .label("Close")
                            .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                    ),
            )
            .child(self.picker(cx))
            .when(!on, |panel| {
                panel.child(
                    div()
                        .text_xs()
                        .text_color(danger)
                        .child(SharedString::from(format!("{OFF_NOTICE} Nothing is sent while it is off."))),
                )
            })
            .children(subject)
            .child(
                div().text_xs().text_color(muted).child(SharedString::from(format!(
                    "Sent with it: the schema names and {described} table{} with their columns and types{}.",
                    if described == 1 { "" } else { "s" },
                    if self.include_samples { ", and their first rows" } else { "" }
                ))),
            )
            .child(
                Checkbox::new("ai-sample-rows")
                    .label("Include sample rows (first 5 rows of the tables above)")
                    .checked(self.include_samples)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.set_include_samples(*checked, cx)
                    })),
            )
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
                            .disabled(!on || sending || empty_sentence || reading)
                            .on_click(cx.listener(|this, _, window, cx| this.send(window, cx))),
                    ),
            )
            .child(stage)
            .when(self.show_preview, |panel| {
                panel.child(
                    div()
                        .id("ai-preview")
                        .test_support()
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
