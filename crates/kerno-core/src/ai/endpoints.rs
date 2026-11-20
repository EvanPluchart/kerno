//! Saved endpoints of the AI assistant (K23, D-095): several named OpenAI-compatible servers,
//! one active; found on this computer ([`detect`]), imported from a tool already set up
//! ([`import`]) or typed by hand ([`normalize`]). Pure functions over what they are given: the
//! home directory, the environment and the ports come from the caller, so tests never read the
//! real ones.

use std::net::IpAddr;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{AiError, AiSettings, Call, Endpoint, Stopper, endpoint, parse_models, send};
use crate::config::Secret;

/// How long Detect waits for each local port.
pub const DETECT_TIMEOUT: Duration = Duration::from_millis(1500);

/// The servers Detect asks on 127.0.0.1, with their default ports.
pub const LOCAL_SERVERS: [(&str, u16); 6] = [
    ("Ollama", 11434),
    ("LM Studio", 1234),
    ("llama.cpp", 8080),
    ("vLLM", 8000),
    ("LiteLLM", 4000),
    ("Jan", 1337),
];

/// A model an endpoint offers: its id, and the name a tool gave it, if any.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Model {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl Model {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: None,
        }
    }

    /// The label and the id, or the id alone.
    pub fn shown(&self) -> String {
        match &self.label {
            Some(label) if *label != self.id => format!("{label} ({})", self.id),
            _ => self.id.clone(),
        }
    }
}

/// One saved endpoint. Its key is the secret-store entry [`super::key_name`] of `base`: one
/// endpoint per base address.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedEndpoint {
    pub name: String,
    /// The base address, as [`endpoint`] gives it (no trailing `/`).
    pub base: String,
    /// The model chosen for it.
    pub model: String,
    /// The models it offers, as last listed or imported.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<Model>,
}

impl AiSettings {
    /// Makes the saved list hold the active address (`endpoint`, `model`), which settings
    /// stored before K23 (or by an older Kerno since) hold alone: the address, as the client
    /// always read it, becomes a saved endpoint with its model. Nothing is lost: its key
    /// (`ai/<base>`) and its confirmation are under the same base.
    pub fn adopt_legacy(&mut self) {
        let text = self.endpoint.trim();
        if text.is_empty() {
            return;
        }
        let base = endpoint(text)
            .map(|endpoint| endpoint.base)
            .unwrap_or_else(|_| text.to_string());
        self.endpoint = base.clone();
        match self.endpoints.iter_mut().find(|saved| saved.base == base) {
            Some(saved) => {
                if !self.model.is_empty() {
                    saved.model = self.model.clone();
                }
            }
            None => self.endpoints.push(SavedEndpoint {
                name: default_name(&base),
                base,
                model: self.model.clone(),
                models: Vec::new(),
            }),
        }
    }

    /// The active endpoint.
    pub fn active(&self) -> Option<&SavedEndpoint> {
        self.endpoints
            .iter()
            .find(|saved| saved.base == self.endpoint)
    }

    /// Makes `base` the active endpoint, with its model.
    pub fn activate(&mut self, base: &str) {
        if let Some(saved) = self.endpoints.iter().find(|saved| saved.base == base) {
            self.endpoint = saved.base.clone();
            self.model = saved.model.clone();
        }
    }

    /// Chooses the active endpoint's model.
    pub fn set_model(&mut self, model: &str) {
        self.model = model.to_string();
        let active = self.endpoint.clone();
        if let Some(saved) = self.endpoints.iter_mut().find(|saved| saved.base == active) {
            saved.model = model.to_string();
        }
    }

    /// Saves `new`, or updates the endpoint of the same base (its name; its model and list
    /// when `new` has them). The first one saved becomes the active one.
    pub fn save_endpoint(&mut self, new: SavedEndpoint) {
        match self
            .endpoints
            .iter_mut()
            .find(|saved| saved.base == new.base)
        {
            Some(saved) => {
                saved.name = new.name;
                if !new.model.is_empty() {
                    saved.model = new.model;
                }
                if !new.models.is_empty() {
                    saved.models = new.models;
                }
            }
            None => self.endpoints.push(new.clone()),
        }
        if self.endpoint.is_empty() || self.endpoint == new.base {
            self.activate(&new.base);
        }
    }

    /// Forgets `base` and its confirmation; when it was the active one, the next saved
    /// endpoint becomes active, or none.
    pub fn remove_endpoint(&mut self, base: &str) {
        self.endpoints.retain(|saved| saved.base != base);
        self.confirmed.remove(base);
        if self.endpoint == base {
            self.endpoint.clear();
            self.model.clear();
            if let Some(next) = self.endpoints.first().map(|saved| saved.base.clone()) {
                self.activate(&next);
            }
        }
    }

    /// Keeps the models `base` offers now, with the labels known before.
    pub fn set_models(&mut self, base: &str, ids: &[String]) {
        if let Some(saved) = self.endpoints.iter_mut().find(|saved| saved.base == base) {
            saved.models = ids
                .iter()
                .map(|id| Model {
                    id: id.clone(),
                    label: (saved.models.iter())
                        .find(|known| known.id == *id)
                        .and_then(|known| known.label.clone()),
                })
                .collect();
        }
    }
}

/// The name a new endpoint gets: the local server its port is known for, else its host.
pub fn default_name(base: &str) -> String {
    let Ok(found) = endpoint(base) else {
        return base.to_string();
    };
    if found.loopback
        && let Some((name, _)) = LOCAL_SERVERS.iter().find(|(_, port)| *port == found.port)
    {
        return format!("{name} on this computer");
    }
    found.host
}

/// Reads an address typed by hand, imported or found: with or without `/v1`, a trailing `/`,
/// even with `/chat/completions` or `/models` pasted after it; without `http://` for this
/// computer or `https://` for another host. Then the checks of [`endpoint`].
pub fn normalize(text: &str) -> Result<Endpoint, String> {
    let text = text.trim();
    let with_scheme = if text.contains("://") || text.is_empty() {
        text.to_string()
    } else {
        let host = text.split(['/', ':']).next().unwrap_or_default();
        let local = host.eq_ignore_ascii_case("localhost")
            || host.starts_with("127.")
            || text.starts_with('[');
        format!("{}://{text}", if local { "http" } else { "https" })
    };
    let Ok(mut url) = url::Url::parse(&with_scheme) else {
        return endpoint(text);
    };
    let mut path = url.path().trim_end_matches('/').to_string();
    for call in ["/chat/completions", "/models"] {
        if let Some(stripped) = path.strip_suffix(call) {
            path = stripped.to_string();
        }
    }
    if path.is_empty() {
        path = "/v1".into();
    }
    url.set_path(&path);
    endpoint(url.as_str())
}

/// Where an endpoint is, for the warning before the first request goes there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    ThisComputer,
    /// A private address or a local name: on the user's network or VPN.
    Network,
    Internet,
}

/// Where `endpoint` is. A private, shared (100.64/10, VPNs such as Tailscale) or link-local
/// address, or a name with a single label or a label `lan`, `local`, `internal`, `intranet`,
/// `localdomain` (`llm.lan.example.com`), or under `home.arpa`, is on the network; any other
/// host on the internet. Only the warning's words follow: both ask before anything is sent.
pub fn reach(endpoint: &Endpoint) -> Reach {
    if endpoint.loopback {
        return Reach::ThisComputer;
    }
    let local = match endpoint.host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => {
            let [a, b, ..] = ip.octets();
            ip.is_private() || ip.is_link_local() || (a == 100 && (64..128).contains(&b))
        }
        Ok(IpAddr::V6(ip)) => {
            let first = ip.segments()[0];
            (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
        Err(_) => {
            let host = endpoint.host.to_ascii_lowercase();
            let host = host.trim_end_matches('.');
            !host.contains('.')
                || host.ends_with(".home.arpa")
                || host.split('.').any(|label| {
                    matches!(
                        label,
                        "lan" | "local" | "internal" | "intranet" | "localdomain"
                    )
                })
        }
    };
    if local {
        Reach::Network
    } else {
        Reach::Internet
    }
}

/// A server that answered Detect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    /// Ollama, LM Studio…, by its port.
    pub name: String,
    pub base: String,
    pub models: Vec<Model>,
    /// It answered that it needs a key (401, 403): its models are not known.
    pub needs_key: bool,
}

/// Detect: asks `GET /v1/models` of `127.0.0.1` on each of `servers` at once, waiting
/// `timeout` for each, and returns those that answered as an OpenAI-compatible server does
/// (a model list, or a refusal for want of a key). Loopback only, by construction; refused
/// while the assistant is off, as every request is. Blocking: run it off the UI thread.
pub fn detect(
    enabled: bool,
    servers: &[(String, u16)],
    timeout: Duration,
) -> Result<Vec<Detected>, AiError> {
    if !enabled {
        return Err(AiError::Off);
    }
    let answers = std::thread::scope(|scope| {
        let asking: Vec<_> = servers
            .iter()
            .map(|(name, port)| {
                scope.spawn(move || {
                    let settings = AiSettings {
                        enabled,
                        endpoint: format!("http://127.0.0.1:{port}/v1"),
                        ..AiSettings::default()
                    };
                    let answer = send(&settings, None, &Call::Models, &Stopper::default(), timeout);
                    (name.clone(), settings.endpoint, answer)
                })
            })
            .collect();
        asking
            .into_iter()
            .filter_map(|thread| thread.join().ok())
            .collect::<Vec<_>>()
    });
    Ok(answers
        .into_iter()
        .filter_map(|(name, base, answer)| match answer {
            Ok(text) => parse_models(&text).ok().map(|ids| Detected {
                name,
                base,
                models: ids.into_iter().map(Model::new).collect(),
                needs_key: false,
            }),
            Err(AiError::Status(401 | 403, _)) => Some(Detected {
                name,
                base,
                models: Vec::new(),
                needs_key: true,
            }),
            Err(_) => None,
        })
        .collect())
}

/// The key of an endpoint found by Import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FoundKey {
    None,
    Key(Secret),
    /// Declared but not usable here: why, without the key.
    Missing(String),
}

/// An OpenAI-compatible provider Import found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// Where it was declared: `omp`, `pi`, `environment`.
    pub source: &'static str,
    pub name: String,
    pub base: String,
    pub key: FoundKey,
    pub models: Vec<Model>,
}

/// What Import found, and what it skipped and why.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Imports {
    pub found: Vec<Found>,
    pub notes: Vec<String>,
}

/// The files Import reads, under the home directory.
pub const OMP_FILES: [&str; 2] = [".omp/agent/models.yml", ".omp/agent/models.yaml"];
pub const PI_FILE: &str = ".pi/agent/models.json";

/// Import: the OpenAI-compatible providers declared for omp (`~/.omp/agent/models.yml`, else
/// `models.yaml`) and pi (`~/.pi/agent/models.json`), and `OPENAI_BASE_URL` /
/// `OPENAI_API_KEY`. `env` reads an environment variable. Only files are read; a key given by
/// a command (`!…`) is never run. A file that cannot be read or parsed is a note, not a stop.
pub fn import(home: Option<&Path>, env: &dyn Fn(&str) -> Option<String>) -> Imports {
    let mut imports = Imports::default();
    if let Some(home) = home {
        if let Some((file, text)) = OMP_FILES
            .iter()
            .find_map(|file| Some((*file, std::fs::read_to_string(home.join(file)).ok()?)))
        {
            match serde_saphyr::from_str::<Value>(&text) {
                Ok(value) => providers(&value, "omp", env, &mut imports),
                Err(error) => imports.notes.push(format!(
                    "~/{file} could not be read as YAML: {}",
                    first_line(&error.to_string())
                )),
            }
        }
        if let Ok(text) = std::fs::read_to_string(home.join(PI_FILE)) {
            match serde_json::from_str::<Value>(&text) {
                Ok(value) => providers(&value, "pi", env, &mut imports),
                Err(error) => imports
                    .notes
                    .push(format!("~/{PI_FILE} could not be read as JSON: {error}")),
            }
        }
    }
    let base = env("OPENAI_BASE_URL").filter(|value| !value.trim().is_empty());
    let key = env("OPENAI_API_KEY").filter(|value| !value.trim().is_empty());
    if base.is_some() || key.is_some() {
        let text = base.unwrap_or_else(|| "https://api.openai.com/v1".into());
        match normalize(&text) {
            Ok(found) => imports.found.push(Found {
                source: "environment",
                name: format!("OPENAI_BASE_URL ({})", found.host),
                base: found.base,
                key: key.map_or(FoundKey::None, |key| FoundKey::Key(Secret::new(key.trim()))),
                models: Vec::new(),
            }),
            Err(why) => imports
                .notes
                .push(format!("OPENAI_BASE_URL was skipped: {why}")),
        }
    }
    imports
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or_default()
}

/// The `providers` of an omp or pi file.
fn providers(
    file: &Value,
    source: &'static str,
    env: &dyn Fn(&str) -> Option<String>,
    imports: &mut Imports,
) {
    let Some(providers) = file.get("providers").and_then(Value::as_object) else {
        imports
            .notes
            .push(format!("{source}: the file declares no providers."));
        return;
    };
    for (id, provider) in providers {
        let skip = |why: String| format!("{source}: “{id}” was skipped: {why}");
        let api = provider.get("api").and_then(Value::as_str);
        if api != Some("openai-completions") {
            imports.notes.push(skip(match api {
                Some(api) => format!(
                    "its API is “{api}”; Kerno speaks OpenAI-compatible chat completions \
                     (openai-completions)."
                ),
                None => "it does not say its API (openai-completions).".into(),
            }));
            continue;
        }
        let Some(address) = provider.get("baseUrl").and_then(Value::as_str) else {
            imports.notes.push(skip("it has no baseUrl.".into()));
            continue;
        };
        let found = match normalize(address) {
            Ok(found) => found,
            Err(why) => {
                imports.notes.push(skip(why));
                continue;
            }
        };
        let models = (provider.get("models").and_then(Value::as_array))
            .into_iter()
            .flatten()
            .filter(|model| {
                (model.get("api").and_then(Value::as_str))
                    .is_none_or(|api| api == "openai-completions")
            })
            .filter_map(|model| {
                Some(Model {
                    id: model.get("id")?.as_str()?.to_string(),
                    label: (model.get("name").and_then(Value::as_str)).map(str::to_string),
                })
            })
            .collect();
        imports.found.push(Found {
            source,
            name: (provider.get("name").and_then(Value::as_str))
                .unwrap_or(id)
                .to_string(),
            base: found.base,
            key: resolve_key(provider.get("apiKey").and_then(Value::as_str), env),
            models,
        });
    }
}

/// An `apiKey` as omp and pi read it: `!command` (never run here), `$NAME` or `${NAME}` (pi),
/// the exact name of a set environment variable (omp, older pi), else the key itself. A value
/// shaped like a variable name (`MY_KEY`) whose variable is not set is said, not taken as the
/// key.
pub fn resolve_key(raw: Option<&str>, env: &dyn Fn(&str) -> Option<String>) -> FoundKey {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return FoundKey::None;
    };
    let unset = |name: &str| {
        FoundKey::Missing(format!(
            "its key is the environment variable {name}, which is not set for Kerno"
        ))
    };
    if raw.starts_with('!') {
        return FoundKey::Missing(
            "its key is given by a command, which Kerno does not run: type the key".into(),
        );
    }
    if raw.contains('$') {
        let mut key = String::new();
        let mut rest = raw;
        while let Some(at) = rest.find('$') {
            key.push_str(&rest[..at]);
            rest = &rest[at + 1..];
            if let Some(after) = rest.strip_prefix('$') {
                key.push('$');
                rest = after;
                continue;
            }
            let (name, after) = match rest.strip_prefix('{') {
                Some(inner) => match inner.find('}') {
                    Some(end) => (&inner[..end], &inner[end + 1..]),
                    None => (inner, ""),
                },
                None => {
                    let end = rest
                        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                        .unwrap_or(rest.len());
                    (&rest[..end], &rest[end..])
                }
            };
            match env(name).filter(|value| !value.is_empty()) {
                Some(value) => key.push_str(&value),
                None => return unset(name),
            }
            rest = after;
        }
        key.push_str(rest);
        return FoundKey::Key(Secret::new(key));
    }
    if let Some(value) = env(raw).filter(|value| !value.is_empty()) {
        return FoundKey::Key(Secret::new(value));
    }
    let variable = raw.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && raw.contains('_')
        && raw
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    if variable {
        return unset(raw);
    }
    FoundKey::Key(Secret::new(raw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn an_address_with_or_without_v1_and_a_slash_is_the_same() {
        for text in [
            "http://localhost:11434",
            "http://localhost:11434/",
            "http://localhost:11434/v1",
            "http://localhost:11434/v1/",
            "localhost:11434",
            "http://localhost:11434/v1/chat/completions",
            "http://localhost:11434/v1/models",
        ] {
            assert_eq!(
                normalize(text).unwrap().base,
                "http://localhost:11434/v1",
                "{text}"
            );
        }
        assert_eq!(
            normalize("llm.example.lan/v1").unwrap().base,
            "https://llm.example.lan/v1"
        );
        // Another path is kept: some servers live under one.
        assert_eq!(
            normalize("https://llm.example.lan/openai/v1/")
                .unwrap()
                .base,
            "https://llm.example.lan/openai/v1"
        );
        // The checks of today stay: no plain http to another host, no key in the address.
        assert!(normalize("http://llm.example.lan").is_err());
        assert!(normalize("https://user:sk-1@llm.example.lan/v1").is_err());
        assert!(normalize("").is_err());
    }

    #[test]
    fn the_warning_says_network_or_internet() {
        let reach_of = |text: &str| reach(&normalize(text).unwrap());
        assert_eq!(reach_of("http://localhost:11434"), Reach::ThisComputer);
        for network in [
            "https://llm.example.lan/v1",
            // The shape of a company's LAN proxy: `lan` is an inner label.
            "https://llm.lan.example.com/v1",
            "https://gpu.local/v1",
            "https://models.internal/v1",
            "https://box.home.arpa/v1",
            "https://spark/v1",
            "https://10.1.2.3/v1",
            "https://172.20.0.5/v1",
            "https://192.168.1.20:8000/v1",
            "https://100.101.102.103/v1",
            "https://169.254.1.1/v1",
            "https://[fd12::1]/v1",
            "https://[fe80::1]/v1",
        ] {
            assert_eq!(reach_of(network), Reach::Network, "{network}");
        }
        for internet in [
            "https://api.example.com/v1",
            "https://8.8.8.8/v1",
            "https://100.200.1.1/v1",
            "https://[2001:db8::1]/v1",
            "https://planet.example.com/v1",
        ] {
            assert_eq!(reach_of(internet), Reach::Internet, "{internet}");
        }
    }

    /// Settings stored before K23 (one address, its model, its confirmation) become one saved
    /// endpoint under the base the client always used: its key (`ai/<base>`) and its
    /// confirmation are found again.
    #[test]
    fn the_settings_of_before_become_one_saved_endpoint() {
        let stored = r#"{"enabled": true, "endpoint": "https://llm.example.lan:8443/v1/",
            "model": "qwen-low", "confirmed": ["https://llm.example.lan:8443/v1"]}"#;
        let mut settings: AiSettings = serde_json::from_str(stored).unwrap();
        let key_before = super::super::key_name(&endpoint(&settings.endpoint).unwrap().base);
        settings.adopt_legacy();
        assert_eq!(settings.endpoints.len(), 1);
        let saved = settings.active().unwrap();
        assert_eq!(saved.base, "https://llm.example.lan:8443/v1");
        assert_eq!(saved.model, "qwen-low");
        assert_eq!(saved.name, "llm.example.lan");
        assert_eq!(super::super::key_name(&saved.base), key_before);
        assert!(settings.confirmed.contains(&saved.base));
        assert!(super::super::gate(&settings).is_ok());
        // Read again, nothing is added twice.
        settings.adopt_legacy();
        assert_eq!(settings.endpoints.len(), 1);
        // A base without /v1 keeps its byte-exact name: its key is not lost.
        let mut bare = AiSettings {
            endpoint: "http://localhost:8080".into(),
            ..AiSettings::default()
        };
        bare.adopt_legacy();
        assert_eq!(bare.active().unwrap().base, "http://localhost:8080");
        assert_eq!(bare.active().unwrap().name, "llama.cpp on this computer");
        // An older Kerno that changed the address and model: those win.
        let mut changed = settings.clone();
        changed.endpoint = "http://localhost:11434/v1".into();
        changed.model = "llama3.2".into();
        changed.adopt_legacy();
        assert_eq!(changed.endpoints.len(), 2);
        assert_eq!(changed.active().unwrap().model, "llama3.2");
        // What an older Kerno reads: the active endpoint's address and model, unchanged keys.
        let written = serde_json::to_value(&changed).unwrap();
        assert_eq!(written["endpoint"], "http://localhost:11434/v1");
        assert_eq!(written["model"], "llama3.2");
        assert_eq!(written["endpoints"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn switching_removing_and_choosing_a_model_keep_the_mirror() {
        let mut settings = AiSettings::default();
        settings.save_endpoint(SavedEndpoint {
            name: "Ollama on this computer".into(),
            base: "http://localhost:11434/v1".into(),
            model: "llama3.2".into(),
            models: Vec::new(),
        });
        assert_eq!(settings.endpoint, "http://localhost:11434/v1");
        settings.save_endpoint(SavedEndpoint {
            name: "Work models".into(),
            base: "https://llm.example.lan/v1".into(),
            ..SavedEndpoint::default()
        });
        // The first saved stays active.
        assert_eq!(settings.model, "llama3.2");
        settings.activate("https://llm.example.lan/v1");
        settings.set_model("qwen-high");
        settings.activate("http://localhost:11434/v1");
        assert_eq!(settings.model, "llama3.2");
        settings.activate("https://llm.example.lan/v1");
        assert_eq!(settings.model, "qwen-high");
        settings
            .confirmed
            .insert("https://llm.example.lan/v1".into());
        settings.set_models(
            "https://llm.example.lan/v1",
            &["qwen-high".into(), "glm".into()],
        );
        assert_eq!(settings.active().unwrap().models.len(), 2);
        // Removing the active one: the next becomes active, and nothing brings it back.
        settings.remove_endpoint("https://llm.example.lan/v1");
        assert!(settings.confirmed.is_empty());
        assert_eq!(settings.endpoint, "http://localhost:11434/v1");
        settings.adopt_legacy();
        assert_eq!(settings.endpoints.len(), 1);
        settings.remove_endpoint("http://localhost:11434/v1");
        assert!(settings.endpoint.is_empty() && settings.model.is_empty());
        settings.adopt_legacy();
        assert!(settings.endpoints.is_empty());
    }

    const OMP: &str = r#"
# Written by the kit.
providers:
  work:
    baseUrl: https://llm.example.lan/v1
    apiKey: sk-work-literal-0001
    api: openai-completions
    models:
      - id: qwen-low
        name: "Qwen · gpu1 · low"
        reasoning: true
        input: [text, image]
        contextWindow: 262144
        maxTokens: 65536
      - id: glm-4
        unknownKey: { nested: true }
  by-env:
    baseUrl: https://llm.example.lan:8443/
    apiKey: WORK_LLM_KEY
    api: openai-completions
    models:
      - id: deepseek-r1
  unset-env:
    baseUrl: https://other.example.lan/v1
    apiKey: MISSING_LLM_KEY
    api: openai-completions
  by-command:
    baseUrl: https://cmd.example.lan/v1
    apiKey: "!op read op://vault/llm/key"
    api: openai-completions
  anthropic:
    baseUrl: https://gateway.example.lan
    api: anthropic-messages
    models:
      - id: claude
"#;

    #[test]
    fn omp_providers_are_read_with_their_keys_and_others_skipped() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".omp/agent")).unwrap();
        std::fs::write(home.path().join(".omp/agent/models.yml"), OMP).unwrap();
        let imports = import(
            Some(home.path()),
            &env_of(&[("WORK_LLM_KEY", "sk-from-env-0002")]),
        );
        let found: HashMap<&str, &Found> =
            imports.found.iter().map(|f| (f.name.as_str(), f)).collect();
        assert_eq!(found.len(), 4, "{imports:?}");
        let work = found["work"];
        assert_eq!(work.source, "omp");
        assert_eq!(work.base, "https://llm.example.lan/v1");
        assert_eq!(work.key, FoundKey::Key(Secret::new("sk-work-literal-0001")));
        assert_eq!(work.models[0].label.as_deref(), Some("Qwen · gpu1 · low"));
        assert_eq!(work.models[1], Model::new("glm-4"));
        let by_env = found["by-env"];
        assert_eq!(by_env.base, "https://llm.example.lan:8443/v1");
        assert_eq!(by_env.key, FoundKey::Key(Secret::new("sk-from-env-0002")));
        assert!(matches!(&found["unset-env"].key,
            FoundKey::Missing(why) if why.contains("MISSING_LLM_KEY")));
        assert!(matches!(&found["by-command"].key,
            FoundKey::Missing(why) if why.contains("command")));
        assert!(found["by-command"].models.is_empty());
        assert_eq!(imports.notes.len(), 1, "{:?}", imports.notes);
        assert!(imports.notes[0].contains("anthropic-messages"));
        // No key in what is said.
        let said = format!("{:?}", imports.notes);
        assert!(!said.contains("sk-"));
    }

    #[test]
    fn pi_providers_the_environment_and_a_broken_file() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".pi/agent")).unwrap();
        std::fs::write(
            home.path().join(".pi/agent/models.json"),
            r#"{"providers": {
                "lab": {"name": "Lab models", "baseUrl": "http://localhost:8000",
                        "api": "openai-completions", "apiKey": "${LAB_KEY}",
                        "models": [{"id": "qwen", "name": "Qwen"}, {"id": "x", "api": "openai-responses"}]},
                "plain": {"baseUrl": "http://192.168.1.4:8000/v1", "api": "openai-completions"},
                "hosted": {"baseUrl": "https://api.example.com/v1", "api": "openai-responses"}
            }}"#,
        )
        .unwrap();
        std::fs::create_dir_all(home.path().join(".omp/agent")).unwrap();
        std::fs::write(
            home.path().join(".omp/agent/models.yml"),
            "providers: [unclosed",
        )
        .unwrap();
        let imports = import(
            Some(home.path()),
            &env_of(&[
                ("LAB_KEY", "sk-lab-0003"),
                ("OPENAI_BASE_URL", "https://llm.example.lan/v1/"),
                ("OPENAI_API_KEY", "sk-env-0004"),
            ]),
        );
        let names: Vec<&str> = imports.found.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["Lab models", "OPENAI_BASE_URL (llm.example.lan)"]);
        let lab = &imports.found[0];
        assert_eq!(lab.base, "http://localhost:8000/v1");
        assert_eq!(lab.key, FoundKey::Key(Secret::new("sk-lab-0003")));
        assert_eq!(
            lab.models,
            [Model {
                id: "qwen".into(),
                label: Some("Qwen".into())
            }]
        );
        assert_eq!(imports.found[1].base, "https://llm.example.lan/v1");
        assert_eq!(
            imports.found[1].key,
            FoundKey::Key(Secret::new("sk-env-0004"))
        );
        let notes = imports.notes.join("\n");
        assert!(notes.contains("could not be read as YAML"), "{notes}");
        assert!(
            notes.contains("“plain” was skipped: Plain http://"),
            "{notes}"
        );
        assert!(notes.contains("openai-responses"), "{notes}");
        // Nothing at all: no note, nothing found.
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(import(Some(empty.path()), &env_of(&[])), Imports::default());
        assert_eq!(import(None, &env_of(&[])), Imports::default());
    }

    #[test]
    fn keys_are_resolved_as_omp_and_pi_do() {
        let env = env_of(&[("A_KEY", "sk-a"), ("B", "b")]);
        let key = |raw| resolve_key(Some(raw), &env);
        assert_eq!(key("A_KEY"), FoundKey::Key(Secret::new("sk-a")));
        assert_eq!(key("$A_KEY"), FoundKey::Key(Secret::new("sk-a")));
        assert_eq!(
            key("pre-${B}-$B.x"),
            FoundKey::Key(Secret::new("pre-b-b.x"))
        );
        assert_eq!(key("$$literal"), FoundKey::Key(Secret::new("$literal")));
        assert_eq!(key("sk-literal"), FoundKey::Key(Secret::new("sk-literal")));
        assert!(matches!(key("NOT_SET"), FoundKey::Missing(_)));
        assert!(matches!(key("$NOT_SET"), FoundKey::Missing(_)));
        assert!(matches!(key("!pass show llm"), FoundKey::Missing(_)));
        assert_eq!(resolve_key(None, &env), FoundKey::None);
        assert_eq!(resolve_key(Some("  "), &env), FoundKey::None);
    }

    /// Detect asks the ports given, on 127.0.0.1, and lists what answered as an
    /// OpenAI-compatible server; a closed port and a server that is not one are left out.
    #[test]
    fn detect_lists_the_local_servers_that_answer() {
        use crate::testing::FakeAi;
        let fake = FakeAi::start("");
        let fake_port: u16 = fake.base()["http://127.0.0.1:".len()..]
            .trim_end_matches("/v1")
            .parse()
            .unwrap();
        let closed = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        // A server that answers, but not as a model server.
        let other = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let other_port = other.local_addr().unwrap().port();
        std::thread::spawn(move || {
            use std::io::{Read as _, Write as _};
            for mut stream in other.incoming().flatten() {
                let mut buffer = [0; 1024];
                let _ = stream.read(&mut buffer);
                let _ = stream.write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
        });
        let servers = vec![
            ("Fake".to_string(), fake_port),
            ("Closed".to_string(), closed),
            ("Other".to_string(), other_port),
        ];
        assert_eq!(detect(false, &servers, DETECT_TIMEOUT), Err(AiError::Off));
        assert_eq!(fake.connections(), 0);
        let found = detect(true, &servers, DETECT_TIMEOUT).unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].name, "Fake");
        assert_eq!(found[0].base, fake.base());
        let ids: Vec<&str> = found[0].models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["fake-model", "other-model"]);
        assert_eq!(fake.requests()[0].line, "GET /v1/models HTTP/1.1");
    }
}
