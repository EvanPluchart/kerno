//! Settings → AI (CLAUDE.md §4 J17; K23, D-095): what the assistant is for and the switch; the
//! saved endpoints, found on this computer, imported from a tool already set up or typed, each
//! with Test; the active endpoint's model; the session's "What was sent" log. What may be sent and
//! the one function that sends are `kerno_core::ai`.

use std::collections::HashMap;
use std::path::PathBuf;

use gpui_kit::base::TestTextExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, Global, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, TestSupportExt as _, Window, div, px,
};
use kerno_core::Secret;
use kerno_core::ai::endpoints::{self, Detected, FoundKey, Imports};
use kerno_core::ai::{self, AiSettings, Model, Reach, SavedEndpoint};
use kerno_core::storage::now_ms;

use crate::ai_view::sent_log;
use crate::bridge;
use crate::state::AppState;
use crate::theme;

/// What Settings → AI says of an endpoint on another host, until it is allowed: where it is,
/// in the words of its case (K23).
pub fn remote_warning(host: &str, reach: Reach) -> String {
    let place = match reach {
        Reach::Network => "on your network",
        _ => "on the internet",
    };
    format!(
        "Requests will leave this computer and go to {host}, {place}: the schema of the database \
         you ask about (and its first rows when you tick them) is sent there. Nothing is sent \
         until you allow it."
    )
}

/// Where Import and Detect look: the home directory, the environment and the local ports.
/// `main` sets this computer's at start; without it (tests), nothing: no home, an empty
/// environment, no port, so a test never reads the user's files or asks their servers.
#[derive(Clone, Default)]
pub struct AiSources {
    pub home: Option<PathBuf>,
    /// `None`: the process's environment, read when Import is clicked.
    pub env: Option<HashMap<String, String>>,
    pub ports: Vec<(String, u16)>,
}

impl Global for AiSources {}

impl AiSources {
    /// This computer's: `HOME`, the process's environment, the usual local ports.
    pub fn of_this_computer() -> Self {
        Self {
            home: std::env::var_os("HOME").map(PathBuf::from),
            env: None,
            ports: endpoints::LOCAL_SERVERS
                .iter()
                .map(|(name, port)| (name.to_string(), *port))
                .collect(),
        }
    }

    fn get(cx: &App) -> Self {
        cx.try_global::<Self>().cloned().unwrap_or_else(|| Self {
            env: Some(HashMap::new()),
            ..Self::default()
        })
    }
}

/// What Import says it reads, and Detect asks.
fn sources_note() -> String {
    let ports: Vec<String> = (endpoints::LOCAL_SERVERS.iter())
        .map(|(name, port)| format!("{name} ({port})"))
        .collect();
    format!(
        "Detect asks 127.0.0.1 on the ports of {}, only when you click. Import reads \
         ~/{}, ~/{} and the variables OPENAI_BASE_URL and OPENAI_API_KEY; it runs nothing.",
        ports.join(", "),
        endpoints::OMP_FILES[0],
        endpoints::PI_FILE
    )
}

/// One line of the Import list.
fn found_line(found: &endpoints::Found) -> String {
    let models = match found.models.len() {
        0 => "models not listed".to_string(),
        1 => "1 model".to_string(),
        n => format!("{n} models"),
    };
    let key = match &found.key {
        FoundKey::None => "no key".to_string(),
        FoundKey::Key(_) => "has a key".to_string(),
        FoundKey::Missing(why) => format!("no key: {why}"),
    };
    format!(
        "{} ({}) · {} · {models} · {key}",
        found.name, found.source, found.base
    )
}

/// The calls Kerno makes to `base`, as the Add form says them.
fn calls(base: &str) -> String {
    format!("Kerno will call {base}/models and {base}/chat/completions.")
}

/// Settings → AI (K23): what the assistant is for and the switch; the saved endpoints, the
/// active one among them, and the three ways to add one (Detect, Import, by hand), each with
/// Test; the active endpoint's model; the session's log.
pub struct AiSettingsView {
    model: Entity<InputState>,
    /// The endpoint the model field was filled for.
    model_for: String,
    key: Entity<InputState>,
    add_name: Entity<InputState>,
    add_address: Entity<InputState>,
    add_key: Entity<InputState>,
    adding: bool,
    add_error: Option<String>,
    /// Why List models failed.
    models_error: Option<String>,
    /// What happened to a key or an import last.
    key_note: Option<String>,
    /// Detect: `None` before, `Some(None)` while asking.
    detected: Option<Option<Result<Vec<Detected>, String>>>,
    /// Import: what was found and which are ticked.
    imports: Option<(Imports, Vec<bool>)>,
    /// Test's last line per base: latency and models, or why not; `None` while testing.
    tested: HashMap<String, Option<Result<String, String>>>,
    tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl AiSettingsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = AppState::get(cx).settings.ai.clone();
        let model = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Type or pick a model, e.g. llama3.2")
                .default_value(settings.model)
        });
        let input = |placeholder: &'static str,
                     masked: bool,
                     window: &mut Window,
                     cx: &mut Context<Self>| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(masked)
                    .placeholder(placeholder)
            })
        };
        let key = input("Only for endpoints that ask for one", true, window, cx);
        let add_name = input("Work models", false, window, cx);
        let add_address = input("https://llm.example.lan/v1", false, window, cx);
        let add_key = input("Only if it asks for one", true, window, cx);
        let subscriptions = vec![
            cx.subscribe(&model, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = input.read(cx).value().trim().to_string();
                    if text != AppState::get(cx).settings.ai.model {
                        AppState::update_settings(cx, |settings| {
                            settings.ai.adopt_legacy();
                            settings.ai.set_model(&text)
                        });
                    }
                    this.models_error = None;
                    cx.notify();
                }
            }),
            cx.subscribe_in(&key, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.save_key(window, cx);
                }
            }),
            cx.subscribe(&add_address, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
        ];
        Self {
            model,
            model_for: settings.endpoint,
            key,
            add_name,
            add_address,
            add_key,
            adding: false,
            add_error: None,
            models_error: None,
            key_note: None,
            detected: None,
            imports: None,
            tested: HashMap::new(),
            tasks: Vec::new(),
            _subscriptions: subscriptions,
        }
    }

    /// The settings with the active address among the saved endpoints.
    fn settings(cx: &App) -> AiSettings {
        let mut settings = AppState::get(cx).settings.ai.clone();
        settings.adopt_legacy();
        settings
    }

    /// Makes `base` the active endpoint.
    pub fn use_endpoint(&mut self, base: &str, cx: &mut Context<Self>) {
        let base = base.to_string();
        AppState::update_settings(cx, |settings| {
            settings.ai.adopt_legacy();
            settings.ai.activate(&base);
        });
        self.key_note = None;
        self.models_error = None;
        cx.notify();
    }

    /// Forgets `base` and its key.
    fn remove_endpoint(&mut self, base: &str, cx: &mut Context<Self>) {
        let removed = base.to_string();
        AppState::update_settings(cx, |settings| {
            settings.ai.adopt_legacy();
            settings.ai.remove_endpoint(&removed);
        });
        self.tested.remove(base);
        let secrets = AppState::get(cx).secrets.clone();
        let name = ai::key_name(base);
        let removing = bridge::spawn_blocking(cx, move || secrets.delete(&name));
        self.tasks.push(cx.spawn(async move |this, cx| {
            let removed = removing.await;
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = removed {
                    this.key_note = Some(format!("Its key could not be removed: {error}"));
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Puts `text` in the model field and chooses it.
    pub fn set_model(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.model
            .update(cx, |input, cx| input.set_value(text, window, cx));
        let text = text.to_string();
        AppState::update_settings(cx, |settings| {
            settings.ai.adopt_legacy();
            settings.ai.set_model(&text)
        });
        cx.notify();
    }

    /// Stores `key` for `base` in the secret store, off the UI thread; says it failed, and
    /// that it was saved when `say_saved` (an import says its own line).
    fn store_key(&mut self, base: String, key: Secret, say_saved: bool, cx: &mut Context<Self>) {
        let secrets = AppState::get(cx).secrets.clone();
        let host = ai::endpoint(&base).map_or_else(|_| base.clone(), |found| found.host);
        let saving = bridge::spawn_blocking(cx, move || secrets.set(&ai::key_name(&base), &key));
        self.tasks.push(cx.spawn(async move |this, cx| {
            let saved = saving.await;
            let _ = this.update(cx, |this, cx| {
                match saved {
                    Ok(()) if !say_saved => return,
                    Ok(()) => {
                        this.key_note = Some(format!(
                            "Key saved in the system's secret store, for {host} only."
                        ))
                    }
                    Err(error) => {
                        this.key_note =
                            Some(format!("The key of {host} could not be saved: {error}"))
                    }
                }
                cx.notify();
            });
        }));
    }

    /// Stores the typed key for the active endpoint and empties the field.
    pub fn save_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.key.read(cx).value().trim().to_string();
        if typed.is_empty() {
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
        self.store_key(endpoint.base, Secret::new(typed), true, cx);
    }

    /// Forgets the key of the active endpoint.
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

    /// `GET /models` of `base` through the one sender (the switch, the address checks, the
    /// confirmation), with its key, timed; the models are kept for the picker. On click only.
    fn ask_models(
        &mut self,
        base: &str,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, Result<(u128, Vec<String>), String>) + 'static,
    ) {
        let mut settings = Self::settings(cx);
        settings.endpoint = base.to_string();
        let secrets = AppState::get(cx).secrets.clone();
        let name = ai::key_name(base);
        let asking = bridge::spawn_blocking(cx, move || {
            let key = secrets.get(&name).ok().flatten();
            let started = std::time::Instant::now();
            ai::list_models(&settings, key.as_ref())
                .map(|models| (started.elapsed().as_millis(), models))
                .map_err(|error| error.to_string())
        });
        let base = base.to_string();
        self.tasks.push(cx.spawn(async move |this, cx| {
            let answer = asking.await;
            let _ = this.update(cx, |this, cx| {
                if let Ok((_, models)) = &answer {
                    let models = models.clone();
                    AppState::update_settings(cx, |settings| {
                        settings.ai.adopt_legacy();
                        settings.ai.set_models(&base, &models)
                    });
                }
                done(this, answer);
                cx.notify();
            });
        }));
    }

    /// Test: one `GET /models`, its latency and the number of models, or why not.
    pub fn test_endpoint(&mut self, base: &str, cx: &mut Context<Self>) {
        self.tested.insert(base.to_string(), None);
        let tested = base.to_string();
        self.ask_models(base, cx, move |this, answer| {
            let line = answer.map(|(ms, models)| match models.len() {
                0 => {
                    format!("Answered in {ms} ms, but lists no model: type the model's name below.")
                }
                1 => format!("Answered in {ms} ms · 1 model."),
                n => format!("Answered in {ms} ms · {n} models."),
            });
            this.tested.insert(tested, Some(line));
        });
        cx.notify();
    }

    /// "List models": the active endpoint's list, for the picker.
    pub fn list_models(&mut self, cx: &mut Context<Self>) {
        let base = AppState::get(cx).settings.ai.endpoint.clone();
        if let Err(error) = ai::gate(&AppState::get(cx).settings.ai) {
            self.models_error = Some(error.to_string());
            cx.notify();
            return;
        }
        self.models_error = None;
        self.ask_models(&base, cx, |this, answer| {
            this.models_error = answer.err();
        });
    }

    /// Detect: the usual local ports, on 127.0.0.1 only, off the UI thread.
    pub fn detect(&mut self, cx: &mut Context<Self>) {
        let enabled = AppState::get(cx).settings.ai.enabled;
        let ports = AiSources::get(cx).ports;
        let asking = bridge::spawn_blocking(cx, move || {
            endpoints::detect(enabled, &ports, endpoints::DETECT_TIMEOUT)
                .map_err(|error| error.to_string())
        });
        self.detected = Some(None);
        self.tasks.push(cx.spawn(async move |this, cx| {
            let found = asking.await;
            let _ = this.update(cx, |this, cx| {
                this.detected = Some(Some(found));
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Saves the server Detect found at `index`.
    fn add_detected(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(Some(Ok(found))) = &self.detected else {
            return;
        };
        let Some(found) = found.get(index).cloned() else {
            return;
        };
        AppState::update_settings(cx, |settings| {
            settings.ai.adopt_legacy();
            settings.ai.save_endpoint(SavedEndpoint {
                name: format!("{} on this computer", found.name),
                model: found
                    .models
                    .first()
                    .map(|m| m.id.clone())
                    .unwrap_or_default(),
                base: found.base,
                models: found.models,
            });
        });
        cx.notify();
    }

    /// Import: reads the files of omp and pi and the environment, off the UI thread.
    pub fn import(&mut self, cx: &mut Context<Self>) {
        let sources = AiSources::get(cx);
        let reading = bridge::spawn_blocking(cx, move || {
            let env = |name: &str| match &sources.env {
                Some(env) => env.get(name).cloned(),
                None => std::env::var(name).ok(),
            };
            endpoints::import(sources.home.as_deref(), &env)
        });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let imports = reading.await;
            let _ = this.update(cx, |this, cx| {
                let ticked = vec![true; imports.found.len()];
                this.imports = Some((imports, ticked));
                cx.notify();
            });
        }));
    }

    /// Ticks or unticks the provider Import found at `index`.
    pub fn tick(&mut self, index: usize, on: bool, cx: &mut Context<Self>) {
        if let Some((_, ticked)) = &mut self.imports
            && let Some(tick) = ticked.get_mut(index)
        {
            *tick = on;
            cx.notify();
        }
    }

    /// Saves the ticked providers: address, name and models into the settings, the key into
    /// the secret store.
    pub fn import_ticked(&mut self, cx: &mut Context<Self>) {
        let Some((imports, ticked)) = self.imports.take() else {
            return;
        };
        let chosen: Vec<endpoints::Found> = (imports.found.into_iter().zip(ticked))
            .filter(|(_, ticked)| *ticked)
            .map(|(found, _)| found)
            .collect();
        let mut said = vec![match chosen.len() {
            1 => "1 endpoint imported; its key, if any, is in the system's secret store.".into(),
            n => format!("{n} endpoints imported; their keys are in the system's secret store."),
        }];
        for found in chosen {
            let saved = Self::settings(cx)
                .endpoints
                .iter()
                .any(|saved| saved.base == found.base);
            let endpoint = SavedEndpoint {
                name: found.name.clone(),
                base: found.base.clone(),
                model: match saved {
                    true => String::new(),
                    false => found
                        .models
                        .first()
                        .map(|m| m.id.clone())
                        .unwrap_or_default(),
                },
                models: found.models,
            };
            AppState::update_settings(cx, |settings| {
                settings.ai.adopt_legacy();
                settings.ai.save_endpoint(endpoint)
            });
            match found.key {
                FoundKey::Key(key) => self.store_key(found.base, key, false, cx),
                FoundKey::Missing(why) => {
                    said.push(format!("{} was imported without a key: {why}.", found.name))
                }
                FoundKey::None => {}
            }
        }
        self.key_note = Some(said.join(" "));
        cx.notify();
    }

    /// Opens or closes the form of an endpoint typed by hand.
    fn toggle_add(&mut self, cx: &mut Context<Self>) {
        self.adding = !self.adding;
        self.add_error = None;
        cx.notify();
    }

    /// Saves the endpoint typed by hand, and its key.
    pub fn add_by_hand(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let address = self.add_address.read(cx).value().to_string();
        let found = match endpoints::normalize(&address) {
            Ok(found) => found,
            Err(why) => {
                self.add_error = Some(why);
                cx.notify();
                return;
            }
        };
        let name = self.add_name.read(cx).value().trim().to_string();
        let name = if name.is_empty() {
            endpoints::default_name(&found.base)
        } else {
            name
        };
        let key = self.add_key.read(cx).value().trim().to_string();
        let base = found.base.clone();
        AppState::update_settings(cx, |settings| {
            settings.ai.adopt_legacy();
            settings.ai.save_endpoint(SavedEndpoint {
                name,
                base,
                ..SavedEndpoint::default()
            })
        });
        if !key.is_empty() {
            self.store_key(found.base, Secret::new(key), true, cx);
        }
        for input in [&self.add_name, &self.add_address, &self.add_key] {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.adding = false;
        self.add_error = None;
        cx.notify();
    }

    /// One saved endpoint: active or Use, name and model, address and where it is, Test,
    /// Remove; under it, Test's line and the question before a host other than this computer.
    fn endpoint_row(
        &self,
        index: usize,
        saved: &SavedEndpoint,
        settings: &AiSettings,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (muted, danger, warning) = (theme.muted_foreground, theme.danger, theme.warning);
        let active = saved.base == settings.endpoint;
        let checked = ai::endpoint(&saved.base);
        let reach = checked.as_ref().map(endpoints::reach);
        let place = match reach {
            Ok(Reach::ThisComputer) => "this computer",
            Ok(Reach::Network) => "your network",
            Ok(Reach::Internet) => "the internet",
            Err(_) => "an address Kerno refuses",
        };
        let model = match saved.model.as_str() {
            "" => "no model chosen".to_string(),
            model => model.to_string(),
        };
        let base = saved.base.clone();
        let tested: Option<AnyElement> = self.tested.get(&saved.base).map(|line| match line {
            None => div()
                .text_xs()
                .text_color(muted)
                .child("Testing…")
                .into_any_element(),
            Some(Ok(line)) => div()
                .id(("ai-tested", index))
                .test_support()
                .text_xs()
                .text_color(muted)
                .child(SharedString::from(line.clone()))
                .test_text(line.clone())
                .into_any_element(),
            Some(Err(why)) => div()
                .id(("ai-tested", index))
                .test_support()
                .text_xs()
                .text_color(danger)
                .child(SharedString::from(why.clone()))
                .test_text(why.clone())
                .into_any_element(),
        });
        let question: Option<AnyElement> = match (&checked, reach) {
            (Ok(found), Ok(reach)) if reach != Reach::ThisComputer => {
                let base = found.base.clone();
                Some(if settings.confirmed.contains(&found.base) {
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().text_xs().text_color(muted).child(
                            SharedString::from(format!("Requests to {} are allowed.", found.host)),
                        ))
                        .child(
                            Button::new(("ai-forget-host", index))
                                .ghost()
                                .xsmall()
                                .label("Stop allowing")
                                .on_click(move |_, _, cx| {
                                    let base = base.clone();
                                    AppState::update_settings(cx, |settings| {
                                        settings.ai.confirmed.remove(&base);
                                    });
                                }),
                        )
                        .into_any_element()
                } else {
                    v_flex()
                        .id(("ai-remote-warning", index))
                        .test_support()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(warning)
                                .child(SharedString::from(remote_warning(&found.host, reach))),
                        )
                        .child(
                            Button::new(("ai-allow-host", index))
                                .outline()
                                .xsmall()
                                .label(SharedString::from(format!(
                                    "Allow requests to {}",
                                    found.host
                                )))
                                .on_click(move |_, _, cx| {
                                    let base = base.clone();
                                    AppState::update_settings(cx, |settings| {
                                        settings.ai.confirmed.insert(base);
                                    });
                                }),
                        )
                        .into_any_element()
                })
            }
            _ => None,
        };
        v_flex()
            .id(("ai-endpoint-row", index))
            .test_support()
            .gap_1()
            .p_2()
            .rounded(theme.radius)
            .border_1()
            .border_color(if active { theme.primary } else { theme.border })
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        theme::chip(("ai-use", index), active)
                            .xsmall()
                            .label(if active { "Active" } else { "Use" })
                            .on_click(cx.listener({
                                let base = base.clone();
                                move |this, _, _, cx| this.use_endpoint(&base, cx)
                            })),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(SharedString::from(format!("{} · {model}", saved.name))),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(SharedString::from(format!("{} · {place}", saved.base))),
                            )
                            .test_text(format!(
                                "{} · {model}\n{} · {place}",
                                saved.name, saved.base
                            )),
                    )
                    .child(
                        Button::new(("ai-test", index))
                            .outline()
                            .xsmall()
                            .label("Test")
                            .on_click(cx.listener({
                                let base = base.clone();
                                move |this, _, _, cx| this.test_endpoint(&base, cx)
                            })),
                    )
                    .child(
                        Button::new(("ai-remove", index))
                            .ghost()
                            .xsmall()
                            .label("Remove")
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.remove_endpoint(&base, cx)),
                            ),
                    ),
            )
            .children(tested)
            .children(question)
            .into_any_element()
    }
}

/// What the top of Settings → AI says the assistant is for, before any field (K23).
pub const PURPOSE: &str = "Turned on, the assistant does three things, only when you ask: it \
     writes SQL from a sentence (inserted in the editor, never run by itself), explains a query or \
     its plan, and explains an error with a proposed fix. Find it in the editor's right-click \
     menu, the command palette and Query ▸ AI Assistant (“AI: …”), and the “Explain … with AI” \
     buttons of a plan or an error. It sends the endpoint you choose the names of your schemas, \
     tables and columns with their types; rows only when you tick “Include sample rows” for that \
     request.";

impl Render for AiSettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = Self::settings(cx);
        // The model field follows the active endpoint (switched here or in the panel).
        if self.model_for != settings.endpoint {
            self.model_for = settings.endpoint.clone();
            let model = settings.model.clone();
            self.model
                .update(cx, |input, cx| input.set_value(model, window, cx));
        }
        let theme = cx.theme();
        let (muted, danger, mono) = (
            theme.muted_foreground,
            theme.danger,
            theme.mono_font_family.clone(),
        );
        let note = |text: String| {
            div()
                .text_xs()
                .text_color(muted)
                .child(SharedString::from(text))
        };
        let heading = |text: String| {
            div()
                .pt_2()
                .font_weight(FontWeight::MEDIUM)
                .child(SharedString::from(text))
        };
        let label = |text: &'static str| div().w(px(80.)).flex_none().child(text);
        let active = settings.active().cloned();

        // The endpoints.
        let rows: Vec<AnyElement> = (settings.endpoints.iter().enumerate())
            .map(|(index, saved)| self.endpoint_row(index, saved, &settings, cx))
            .collect();
        let empty = rows.is_empty().then(|| {
            const EMPTY: &str = "No endpoint yet: Detect one on this computer, Import one from a \
                 tool already set up, or Add one by hand.";
            div()
                .id("ai-no-endpoint")
                .test_support()
                .p_2()
                .rounded(theme.radius)
                .border_1()
                .border_dashed()
                .border_color(theme.border)
                .text_xs()
                .text_color(muted)
                .child(EMPTY)
                .test_text(EMPTY)
        });
        let detected: Option<AnyElement> = self.detected.as_ref().map(|state| match state {
            None => note("Asking this computer's usual ports…".into()).into_any_element(),
            Some(Err(why)) => div()
                .id("ai-detect-error")
                .test_support()
                .text_xs()
                .text_color(danger)
                .child(SharedString::from(why.clone()))
                .into_any_element(),
            Some(Ok(found)) if found.is_empty() => div()
                .id("ai-detected-none")
                .test_support()
                .child(note(
                    "Nothing answered on this computer's usual ports. Start the server, or Add it \
                     by hand."
                        .into(),
                ))
                .into_any_element(),
            Some(Ok(found)) => v_flex()
                .gap_1()
                .children(found.iter().enumerate().map(|(index, found)| {
                    let added = settings
                        .endpoints
                        .iter()
                        .any(|saved| saved.base == found.base);
                    let what = match (found.needs_key, found.models.len()) {
                        (true, _) => "needs a key".to_string(),
                        (false, 0) => "lists no model".to_string(),
                        (false, 1) => "1 model".to_string(),
                        (false, n) => format!("{n} models"),
                    };
                    let line = format!("{} · {what} · {}", found.name, found.base);
                    h_flex()
                        .id(("ai-detected", index))
                        .test_support()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .text_xs()
                                .child(SharedString::from(line.clone()))
                                .test_text(line),
                        )
                        .child(
                            Button::new(("ai-detected-add", index))
                                .outline()
                                .xsmall()
                                .label(if added { "Added" } else { "Add" })
                                .disabled(added)
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.add_detected(index, cx)),
                                ),
                        )
                }))
                .into_any_element(),
        });
        let imports: Option<AnyElement> = self.imports.as_ref().map(|(imports, ticked)| {
            v_flex()
                .id("ai-import-list")
                .test_support()
                .gap_1()
                .when(imports.found.is_empty(), |list| {
                    list.child(note(
                        "Nothing found: no OpenAI-compatible provider in those files or \
                         variables."
                            .into(),
                    ))
                })
                .children(imports.found.iter().enumerate().map(|(index, found)| {
                    let line = found_line(found);
                    Checkbox::new(("ai-found", index))
                        .label(SharedString::from(line.clone()))
                        .checked(ticked.get(index).copied().unwrap_or(false))
                        .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                            this.tick(index, *checked, cx)
                        }))
                        .test_text(line)
                }))
                .children(
                    (imports.notes.iter()).map(|text| note(text.clone()).test_text(text.clone())),
                )
                .when(!imports.found.is_empty(), |list| {
                    list.child(
                        h_flex().child(
                            Button::new("ai-import-ticked")
                                .primary()
                                .small()
                                .label("Import ticked")
                                .disabled(!ticked.iter().any(|tick| *tick))
                                .on_click(cx.listener(|this, _, _, cx| this.import_ticked(cx))),
                        ),
                    )
                })
                .into_any_element()
        });
        let add_form = self.adding.then(|| {
            let address = self.add_address.read(cx).value().to_string();
            let what = match (address.trim().is_empty(), endpoints::normalize(&address)) {
                (true, _) => note("With or without /v1; http:// only for this computer.".into())
                    .into_any_element(),
                (false, Ok(found)) => note(calls(&found.base)).into_any_element(),
                (false, Err(why)) => div()
                    .id("ai-add-error")
                    .test_support()
                    .text_xs()
                    .text_color(danger)
                    .child(SharedString::from(why))
                    .into_any_element(),
            };
            v_flex()
                .id("ai-add-form")
                .test_support()
                .gap_1()
                .p_2()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .child(
                    h_flex().gap_2().child(label("Name")).child(
                        div()
                            .id("ai-add-name")
                            .test_support()
                            .flex_1()
                            .child(Input::new(&self.add_name).small()),
                    ),
                )
                .child(
                    h_flex().gap_2().child(label("Address")).child(
                        div()
                            .id("ai-add-address")
                            .test_support()
                            .flex_1()
                            .child(Input::new(&self.add_address).small()),
                    ),
                )
                .child(
                    h_flex().gap_2().child(label("API key")).child(
                        div()
                            .id("ai-add-key")
                            .test_support()
                            .flex_1()
                            .child(Input::new(&self.add_key).small()),
                    ),
                )
                .child(what)
                .children(self.add_error.clone().map(|why| {
                    div()
                        .text_xs()
                        .text_color(danger)
                        .child(SharedString::from(why))
                }))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("ai-add-save")
                                .primary()
                                .small()
                                .label("Add")
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.add_by_hand(window, cx)),
                                ),
                        )
                        .child(
                            Button::new("ai-add-cancel")
                                .ghost()
                                .small()
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_add(cx))),
                        ),
                )
                .into_any_element()
        });

        // The model of the active endpoint: typed, or picked from its list.
        let typed = self.model.read(cx).value().trim().to_lowercase();
        let known = active
            .as_ref()
            .map(|saved| saved.models.clone())
            .unwrap_or_default();
        let exact = known.iter().any(|model| model.id.to_lowercase() == typed);
        let shown: Vec<(usize, Model)> = (known.iter().cloned().enumerate())
            .filter(|(_, model)| {
                exact || typed.is_empty() || model.shown().to_lowercase().contains(&typed)
            })
            .collect();
        let model_section = match &active {
            None => note("Choose an endpoint above first.".into()).into_any_element(),
            Some(saved) => v_flex()
                .gap_1()
                .child(
                    h_flex()
                        .gap_2()
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
                                .disabled(!settings.enabled)
                                .on_click(cx.listener(|this, _, _, cx| this.list_models(cx))),
                        ),
                )
                .children(self.models_error.clone().map(|why| {
                    div()
                        .id("ai-models-error")
                        .test_support()
                        .text_xs()
                        .text_color(danger)
                        .child(SharedString::from(why))
                }))
                .child(if known.is_empty() {
                    note(format!(
                        "No list of {}'s models yet: List models asks it, or type the model's \
                         name.",
                        saved.name
                    ))
                    .into_any_element()
                } else if shown.is_empty() {
                    note("No listed model matches: it is used as typed.".into()).into_any_element()
                } else {
                    v_flex()
                        .id("ai-model-list")
                        .test_support()
                        .max_h(px(150.))
                        .overflow_y_scroll()
                        .rounded(theme.radius)
                        .border_1()
                        .border_color(theme.border)
                        .children(shown.into_iter().map(|(index, model)| {
                            let chosen = model.id == settings.model;
                            let id = model.id.clone();
                            div()
                                .id(("ai-model", index))
                                .test_support()
                                .px_2()
                                .py_0p5()
                                .text_xs()
                                .cursor_pointer()
                                .when(chosen, |row| row.bg(theme.list_active))
                                .hover(|row| row.bg(theme.list_hover))
                                .child(SharedString::from(model.shown()))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.set_model(&id, window, cx)
                                }))
                                .test_text(model.shown())
                        }))
                        .into_any_element()
                })
                .into_any_element(),
        };

        let now = now_ms();
        let log = sent_log(cx);
        v_flex()
            .id("settings-ai")
            .test_support()
            .gap_2()
            // 1. What it is for, and the switch.
            .child(
                div()
                    .id("ai-purpose")
                    .test_support()
                    .text_sm()
                    .child(PURPOSE)
                    .test_text(PURPOSE),
            )
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
                "Off, no request is ever made. On, Kerno asks only the active endpoint, only when \
                 you send a request; each request shows what it sends."
                    .into(),
            ))
            // 2. The endpoints, the active one first among equals.
            .child(heading("Endpoints".into()))
            .children(empty)
            .children(rows)
            .when_some(active.clone(), |page, saved| {
                page.child(
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .flex_none()
                                .text_xs()
                                .child(SharedString::from(format!("API key of {}", saved.name))),
                        )
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
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.save_key(window, cx)),
                                ),
                        )
                        .child(
                            Button::new("ai-remove-key")
                                .ghost()
                                .small()
                                .label("Remove")
                                .on_click(cx.listener(|this, _, _, cx| this.remove_key(cx))),
                        ),
                )
            })
            .child({
                let line = match &self.key_note {
                    Some(note) => note.clone(),
                    None => "Each endpoint's key is kept in the system's secret store, never in \
                             Kerno's files or logs."
                        .into(),
                };
                div()
                    .id("ai-key-note")
                    .test_support()
                    .child(note(line.clone()))
                    .test_text(line)
            })
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Button::new("ai-detect")
                            .outline()
                            .small()
                            .label("Detect on this computer")
                            .on_click(cx.listener(|this, _, _, cx| this.detect(cx))),
                    )
                    .child(
                        Button::new("ai-import")
                            .outline()
                            .small()
                            .label("Import from omp, pi or the environment")
                            .on_click(cx.listener(|this, _, _, cx| this.import(cx))),
                    )
                    .child(
                        Button::new("ai-add")
                            .outline()
                            .small()
                            .label("Add by hand")
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_add(cx))),
                    ),
            )
            .child(note(sources_note()))
            .children(detected)
            .children(imports)
            .children(add_form)
            // 3. The model.
            .child(heading(match &active {
                Some(saved) => format!("Model of {}", saved.name),
                None => "Model".into(),
            }))
            .child(model_section)
            // 4. What was sent.
            .child(heading("What was sent".into()))
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
