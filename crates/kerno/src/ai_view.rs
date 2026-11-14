//! The AI assistant's screens (CLAUDE.md §4 J17, DBeaver #36951, #20072; D-068): the answer
//! panel at the side of a query tab, the AI entries of the editor's menu, Settings → AI, and
//! the session's "What was sent" log. What is sent and the one function that sends are
//! `kerno_core::ai`: this file only gathers what the user chose and shows the answer, as text.
//! An answer runs nothing: its SQL is inserted in the editor when the user clicks Insert.

use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{EditorState, Input, InputEvent, InputState};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
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
use crate::theme;

/// What an entry says while the assistant is off.
pub const OFF: &str = "turn on in Settings → AI";

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
    Sending { host: String, _task: Task<()> },
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
    /// An API key is stored for the endpoint (read once, off the UI thread).
    has_key: bool,
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
                let collected = exec::collect(session.clone(), sql)
                    .await
                    .map_err(|error| format!("{}.{}: {error}", table.schema, table.name))?;
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
        let asking = bridge::spawn_blocking(cx, move || {
            let key: Option<Secret> = secrets.get(&key_name).ok().flatten();
            ai::chat(&settings, key.as_ref(), &payload)
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
            _task: task,
        };
        self.include_samples = false;
        cx.notify();
    }

    /// Stops waiting: the answer, if it comes, is dropped (the request itself ends when the
    /// server answers or after the 60 s timeout, on its own thread).
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        if matches!(self.stage, Stage::Sending { .. }) {
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
    /// at the cursor for "Write SQL", after the text as a new statement for a proposed fix.
    pub fn insert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(answer) = self.answer() else {
            return;
        };
        let sql = ai::sql_block(answer);
        let at_end = !matches!(self.action, Action::WriteSql { .. });
        self.editor.update(cx, |editor, cx| {
            let at = if at_end {
                editor.text().len()
            } else {
                editor.cursor()
            };
            let prefix = if at_end && at > 0 { "\n\n" } else { "" };
            editor.set_selected_range(at..at, cx);
            editor.insert(format!("{prefix}{sql}"), window, cx);
            let start = at + prefix.len();
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
                .text_xs()
                .text_color(muted)
                .child("Stopped. An answer that comes now is dropped.")
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
                let copied = answer.clone();
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
                            .child(SharedString::from(answer.clone())),
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
            .when(!on, |panel| {
                panel.child(
                    div()
                        .text_xs()
                        .text_color(danger)
                        .child("The AI assistant is off: turn it on in Settings → AI. Nothing is sent while it is off."),
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

/// What Settings → AI says of an endpoint on another host, until it is allowed.
pub fn remote_warning(host: &str) -> String {
    format!(
        "Requests will leave this computer and go to {host}: the schema of the database you ask \
         about (and its first rows when you tick them) is sent there. Nothing is sent until you \
         allow it."
    )
}

/// Settings → AI: on or off, the endpoint and its key, the model, and the session's log.
pub struct AiSettingsView {
    endpoint: Entity<InputState>,
    model: Entity<InputState>,
    key: Entity<InputState>,
    /// The models "List models" found, or why not.
    models: Option<Result<Vec<String>, String>>,
    /// What happened to the key last.
    key_note: Option<String>,
    tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl AiSettingsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = AppState::get(cx).settings.ai.clone();
        let endpoint = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(ai::SUGGESTIONS[0].1)
                .default_value(settings.endpoint)
        });
        let model = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("llama3.2")
                .default_value(settings.model)
        });
        let key = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("Only for endpoints that ask for one")
        });
        let subscriptions = vec![
            cx.subscribe(&endpoint, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = input.read(cx).value().trim().to_string();
                    AppState::update_settings(cx, |settings| settings.ai.endpoint = text);
                    this.models = None;
                    this.key_note = None;
                    cx.notify();
                }
            }),
            cx.subscribe(&model, |_, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = input.read(cx).value().trim().to_string();
                    AppState::update_settings(cx, |settings| settings.ai.model = text);
                    cx.notify();
                }
            }),
            cx.subscribe_in(&key, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.save_key(window, cx);
                }
            }),
        ];
        Self {
            endpoint,
            model,
            key,
            models: None,
            key_note: None,
            tasks: Vec::new(),
            _subscriptions: subscriptions,
        }
    }

    /// Puts `text` in the endpoint field (a suggestion).
    pub fn set_endpoint(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.endpoint
            .update(cx, |input, cx| input.set_value(text, window, cx));
        AppState::update_settings(cx, |settings| settings.ai.endpoint = text.to_string());
        cx.notify();
    }

    /// Puts `text` in the model field.
    pub fn set_model(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.model
            .update(cx, |input, cx| input.set_value(text, window, cx));
        AppState::update_settings(cx, |settings| settings.ai.model = text.to_string());
        cx.notify();
    }

    /// Stores the typed key in the secret store, for this endpoint only, and empties the field.
    pub fn save_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.key.read(cx).value().to_string();
        if typed.trim().is_empty() {
            return;
        }
        let endpoint = match ai::endpoint(&AppState::get(cx).settings.ai.endpoint) {
            Ok(endpoint) => endpoint,
            Err(why) => {
                self.key_note = Some(format!("Not saved: {why}"));
                cx.notify();
                return;
            }
        };
        self.key
            .update(cx, |input, cx| input.set_value("", window, cx));
        let secrets = AppState::get(cx).secrets.clone();
        let host = endpoint.host.clone();
        let saving = bridge::spawn_blocking(cx, move || {
            secrets.set(&ai::key_name(&endpoint.base), &Secret::new(typed.trim()))
        });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let saved = saving.await;
            let _ = this.update(cx, |this, cx| {
                this.key_note = Some(match saved {
                    Ok(()) => format!("Key saved in the system's secret store, for {host} only."),
                    Err(error) => format!("The key could not be saved: {error}"),
                });
                cx.notify();
            });
        }));
    }

    /// Forgets the key of the endpoint.
    fn remove_key(&mut self, cx: &mut Context<Self>) {
        let Ok(endpoint) = ai::endpoint(&AppState::get(cx).settings.ai.endpoint) else {
            return;
        };
        let secrets = AppState::get(cx).secrets.clone();
        let removing =
            bridge::spawn_blocking(cx, move || secrets.delete(&ai::key_name(&endpoint.base)));
        self.tasks.push(cx.spawn(async move |this, cx| {
            let removed = removing.await;
            let _ = this.update(cx, |this, cx| {
                this.key_note = Some(match removed {
                    Ok(()) => "Key removed.".into(),
                    Err(error) => format!("The key could not be removed: {error}"),
                });
                cx.notify();
            });
        }));
    }

    /// "List models": `GET /models` through the one sender, so it obeys the same switch and
    /// checks; asked only when the user clicks.
    pub fn list_models(&mut self, cx: &mut Context<Self>) {
        let settings = AppState::get(cx).settings.ai.clone();
        let endpoint = match ai::gate(&settings) {
            Ok(endpoint) => endpoint,
            Err(error) => {
                self.models = Some(Err(error.to_string()));
                cx.notify();
                return;
            }
        };
        let secrets = AppState::get(cx).secrets.clone();
        let asking = bridge::spawn_blocking(cx, move || {
            let key = secrets.get(&ai::key_name(&endpoint.base)).ok().flatten();
            ai::list_models(&settings, key.as_ref()).map_err(|error| error.to_string())
        });
        self.models = None;
        self.tasks.push(cx.spawn(async move |this, cx| {
            let models = asking.await;
            let _ = this.update(cx, |this, cx| {
                this.models = Some(models);
                cx.notify();
            });
        }));
        cx.notify();
    }
}

impl Render for AiSettingsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = AppState::get(cx).settings.ai.clone();
        let theme = cx.theme();
        let (muted, danger, warning, mono) = (
            theme.muted_foreground,
            theme.danger,
            theme.warning,
            theme.mono_font_family.clone(),
        );
        let note = |text: String| {
            div()
                .text_xs()
                .text_color(muted)
                .child(SharedString::from(text))
        };
        let label = |text: &'static str| div().w(px(120.)).child(text);
        let checked = ai::endpoint(&settings.endpoint);
        let where_to: AnyElement =
            match &checked {
                Err(why) if !settings.endpoint.is_empty() => div()
                    .id("ai-endpoint-error")
                    .test_support()
                    .text_xs()
                    .text_color(danger)
                    .child(SharedString::from(why.clone()))
                    .into_any_element(),
                Err(_) => {
                    note("Kerno asks only this address, and only when you send a request.".into())
                        .into_any_element()
                }
                Ok(endpoint) if endpoint.loopback => note(format!(
                    "{} is this computer: nothing leaves it.",
                    endpoint.host
                ))
                .into_any_element(),
                Ok(endpoint) if settings.confirmed.contains(&endpoint.base) => {
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().text_xs().text_color(warning).child(
                            SharedString::from(format!(
                                "Requests leave this computer and go to {}: allowed.",
                                endpoint.host
                            )),
                        ))
                        .child({
                            let base = endpoint.base.clone();
                            Button::new("ai-forget-host")
                                .ghost()
                                .xsmall()
                                .label("Stop allowing")
                                .on_click(move |_, _, cx| {
                                    let base = base.clone();
                                    AppState::update_settings(cx, |settings| {
                                        settings.ai.confirmed.remove(&base);
                                    });
                                })
                        })
                        .into_any_element()
                }
                Ok(endpoint) => v_flex()
                    .id("ai-remote-warning")
                    .test_support()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(warning)
                            .child(SharedString::from(remote_warning(&endpoint.host))),
                    )
                    .child({
                        let base = endpoint.base.clone();
                        Button::new("ai-allow-host")
                            .outline()
                            .small()
                            .label(SharedString::from(format!(
                                "Allow requests to {}",
                                endpoint.host
                            )))
                            .on_click(move |_, _, cx| {
                                let base = base.clone();
                                AppState::update_settings(cx, |settings| {
                                    settings.ai.confirmed.insert(base);
                                });
                            })
                    })
                    .into_any_element(),
            };
        let models: Option<AnyElement> =
            self.models.as_ref().map(|models| match models {
                Ok(models) if models.is_empty() => {
                    note("The endpoint lists no model.".into()).into_any_element()
                }
                Ok(models) => h_flex()
                    .flex_wrap()
                    .gap_1()
                    .children(models.iter().enumerate().map(|(index, name)| {
                        let name = name.clone();
                        theme::chip(("ai-model", index), settings.model == name)
                            .xsmall()
                            .label(SharedString::from(name.clone()))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.set_model(&name, window, cx)
                            }))
                    }))
                    .into_any_element(),
                Err(why) => div()
                    .id("ai-models-error")
                    .test_support()
                    .text_xs()
                    .text_color(danger)
                    .child(SharedString::from(why.clone()))
                    .into_any_element(),
            });
        let now = now_ms();
        let log = sent_log(cx);
        v_flex()
            .id("settings-ai")
            .test_support()
            .gap_2()
            .child(
                Checkbox::new("settings-ai-enabled")
                    .label("Enable the AI assistant")
                    .checked(settings.enabled)
                    .on_click(|checked, window, cx| {
                        let checked = *checked;
                        AppState::update_settings(cx, |settings| settings.ai.enabled = checked);
                        window.refresh();
                    }),
            )
            .child(note(
                "Off, no request is ever made. On, Kerno talks to the OpenAI-compatible endpoint \
                 below only when you ask (Write SQL, Explain query, plan or error): it sends the \
                 names of the schemas, tables and columns with their types, never rows unless you \
                 tick “Include sample rows” for that request. Each request shows what it sends."
                    .into(),
            ))
            .child(
                h_flex().gap_2().child(label("Endpoint")).child(
                    div()
                        .id("ai-endpoint")
                        .test_support()
                        .flex_1()
                        .child(Input::new(&self.endpoint).small()),
                ),
            )
            .child(
                h_flex()
                    .gap_1()
                    .pl(px(128.))
                    .children(ai::SUGGESTIONS.iter().enumerate().map(
                        |(index, (name, address))| {
                            let address = *address;
                            Button::new(("ai-suggestion", index))
                                .ghost()
                                .xsmall()
                                .label(SharedString::from(format!("{name} — {address}")))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.set_endpoint(address, window, cx)
                                }))
                        },
                    )),
            )
            .child(where_to)
            .child(
                h_flex()
                    .gap_2()
                    .child(label("Model"))
                    .child(
                        div()
                            .id("ai-model")
                            .test_support()
                            .flex_1()
                            .child(Input::new(&self.model).small()),
                    )
                    .child(
                        Button::new("ai-list-models")
                            .outline()
                            .small()
                            .label("List models")
                            .disabled(!settings.enabled || checked.is_err())
                            .on_click(cx.listener(|this, _, _, cx| this.list_models(cx))),
                    ),
            )
            .children(models)
            .child(
                h_flex()
                    .gap_2()
                    .child(label("API key"))
                    .child(
                        div()
                            .id("ai-key")
                            .test_support()
                            .flex_1()
                            .child(Input::new(&self.key).small()),
                    )
                    .child(
                        Button::new("ai-save-key")
                            .outline()
                            .small()
                            .label("Save key")
                            .disabled(checked.is_err())
                            .on_click(cx.listener(|this, _, window, cx| this.save_key(window, cx))),
                    )
                    .child(
                        Button::new("ai-remove-key")
                            .ghost()
                            .small()
                            .label("Remove")
                            .disabled(checked.is_err())
                            .on_click(cx.listener(|this, _, _, cx| this.remove_key(cx))),
                    ),
            )
            .child(note(match &self.key_note {
                Some(note) => note.clone(),
                None => "The key is kept in the system's secret store, one per endpoint, never \
                         in Kerno's files or logs."
                    .into(),
            }))
            .child(
                div()
                    .pt_2()
                    .font_weight(FontWeight::MEDIUM)
                    .child("What was sent"),
            )
            .child(
                v_flex()
                    .id("ai-sent-log")
                    .test_support()
                    .gap_2()
                    .when(log.is_empty(), |list| {
                        list.child(note(
                            "Nothing was sent during this run. The list is kept in memory only."
                                .into(),
                        ))
                    })
                    .children(log.into_iter().rev().enumerate().map(|(index, sent)| {
                        v_flex()
                            .id(("ai-sent", index))
                            .test_support()
                            .gap_1()
                            .child(note(format!(
                                "{} · POST {}{}",
                                crate::library::ago(now, sent.at_ms),
                                sent.endpoint,
                                if sent.key {
                                    " · Authorization: Bearer ***"
                                } else {
                                    ""
                                }
                            )))
                            .child(
                                div()
                                    .max_h(px(160.))
                                    .overflow_hidden()
                                    .font_family(mono.clone())
                                    .text_xs()
                                    .child(SharedString::from(sent.body)),
                            )
                    })),
            )
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
