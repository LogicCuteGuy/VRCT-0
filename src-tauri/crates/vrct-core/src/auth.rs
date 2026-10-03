//! Credential checks, provider model discovery and selection, all in-process.
use crate::controller::Controller;
use crate::router::{Reply, Router};
use crate::settings::Settings;
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::sync::Arc;

pub const PROVIDERS: &[(&str, &str)] = &[
    ("deepl", "DeepL_API"),
    ("plamo", "Plamo_API"),
    ("gemini", "Gemini_API"),
    ("openai", "OpenAI_API"),
    ("groq", "Groq_API"),
    ("openrouter", "OpenRouter_API"),
    ("lmstudio", "LMStudio"),
    ("ollama", "Ollama"),
    ("openai_compatible", "OpenAI_Compatible"),
    ("groq_whisper", "Groq_Whisper"),
    ("openai_whisper", "OpenAI_Whisper"),
    ("custom_whisper", "Custom_Whisper"),
    ("deepgram", "Deepgram"),
];
#[derive(Default)]
pub struct Models {
    pub names: Option<Vec<String>>,
    pub languages: Value,
}
pub trait Catalog: Send + Sync {
    fn fetch(
        &self,
        engine: String,
        key: Option<String>,
        base: Option<String>,
    ) -> BoxFuture<'static, Result<Models, String>>;
}
pub struct HttpCatalog;
impl Catalog for HttpCatalog {
    fn fetch(
        &self,
        engine: String,
        key: Option<String>,
        base: Option<String>,
    ) -> BoxFuture<'static, Result<Models, String>> {
        Box::pin(async move {
            if engine == "DeepL_API" {
                let key = key.ok_or("missing credential")?;
                if ![36, 39].contains(&key.len())
                    || !crate::translation::deepl::check(crate::translation::deepl::Check {
                        auth_key: key,
                        server_url: base,
                    })
                    .await?
                {
                    return Err("Authentication failed".into());
                }
                return Ok(Models::default());
            }
            if matches!(
                engine.as_str(),
                "Groq_Whisper" | "OpenAI_Whisper" | "Custom_Whisper" | "Deepgram"
            ) {
                let key = key
                    .filter(|key| !key.is_empty())
                    .ok_or("missing credential")?;
                let base = base.unwrap_or_else(|| crate::transcription::deepgram::BASE_URL.into());
                let headers = [(
                    "authorization",
                    format!(
                        "{} {key}",
                        if engine == "Deepgram" {
                            "Token"
                        } else {
                            "Bearer"
                        }
                    ),
                )];
                let result = crate::translation::http::get_json(
                    &format!("{}/models", base.trim_end_matches('/')),
                    &headers,
                )
                .await?;
                if engine == "Deepgram" {
                    let mut languages = serde_json::Map::new();
                    let mut names = Vec::new();
                    for item in result["stt"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|i| i["batch"] == true)
                    {
                        if let Some(name) = item["name"].as_str().filter(|name| !name.is_empty()) {
                            if !languages.contains_key(name) {
                                names.push(name.into());
                                languages.insert(
                                    name.into(),
                                    item.get("languages")
                                        .filter(|v| v.is_array())
                                        .cloned()
                                        .unwrap_or(json!([])),
                                );
                            }
                        }
                    }
                    names.sort();
                    return Ok(Models {
                        names: Some(names),
                        languages: Value::Object(languages),
                    });
                }
                let mut names: Vec<String> = result["data"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|i| i["id"].as_str())
                    .filter(|name| {
                        engine == "Custom_Whisper"
                            || ["whisper", "transcribe"]
                                .iter()
                                .any(|word| name.to_lowercase().contains(word))
                    })
                    .map(str::to_string)
                    .collect();
                names.sort();
                return Ok(Models {
                    names: Some(names),
                    languages: Value::Null,
                });
            }
            let target = || crate::translation::catalog::Target {
                engine: engine.clone(),
                api_key: key.clone(),
                base_url: base.clone(),
            };
            if !crate::translation::catalog::auth_check(target()).await? {
                return Err("Authentication failed".into());
            }
            let names = crate::translation::catalog::models(target()).await?;
            Ok(Models {
                names: Some(names),
                languages: Value::Null,
            })
        })
    }
}

pub struct Auth {
    settings: Arc<Settings>,
    controller: Arc<Controller>,
    catalog: Arc<dyn Catalog>,
    serial: tokio::sync::Mutex<()>,
}
fn speech(engine: &str) -> bool {
    matches!(
        engine,
        "Groq_Whisper" | "OpenAI_Whisper" | "Custom_Whisper" | "Deepgram"
    )
}
fn key_property(engine: &str) -> &'static str {
    if speech(engine) {
        "TRANSCRIPTION_AUTH_KEYS"
    } else {
        "AUTH_KEYS"
    }
}
fn status_property(engine: &str) -> &'static str {
    if speech(engine) {
        "SELECTABLE_TRANSCRIPTION_ENGINE_STATUS"
    } else {
        "SELECTABLE_TRANSLATION_ENGINE_STATUS"
    }
}
fn local(engine: &str) -> bool {
    matches!(engine, "LMStudio" | "Ollama")
}
fn url_property(engine: &str) -> Option<&'static str> {
    match engine {
        "LMStudio" => Some("LMSTUDIO_URL"),
        "OpenAI_Compatible" => Some("OPENAI_COMPATIBLE_URL"),
        "Custom_Whisper" => Some("TRANSCRIPTION_CUSTOM_URL"),
        "Groq_Whisper" => Some("GROQ_WHISPER_BASE_URL"),
        "OpenAI_Whisper" => Some("OPENAI_WHISPER_BASE_URL"),
        _ => None,
    }
}

impl Auth {
    pub fn new(
        settings: Arc<Settings>,
        controller: Arc<Controller>,
        catalog: Arc<dyn Catalog>,
    ) -> Arc<Self> {
        Arc::new(Self {
            settings,
            controller,
            catalog,
            serial: tokio::sync::Mutex::new(()),
        })
    }
    pub fn getter(&self, endpoint: &str) -> Option<Value> {
        for &(name, engine) in PROVIDERS {
            if endpoint == format!("/get/data/{name}_auth_key") && !local(engine) {
                return Some(self.key(engine));
            }
            if endpoint == format!("/get/data/connected_{name}") && local(engine) {
                return Some(json!(
                    self.settings
                        .get(status_property(engine))
                        .unwrap_or_default()[engine]
                        == true
                ));
            }
        }
        None
    }
    pub fn owns(endpoint: &str) -> bool {
        PROVIDERS.iter().any(|&(name, engine)| {
            if local(engine) {
                endpoint == format!("/get/data/connected_{name}")
                    || endpoint == format!("/run/{name}_connection")
                    || engine == "LMStudio" && endpoint == "/set/data/lmstudio_url"
            } else {
                ["get", "set", "delete"]
                    .iter()
                    .any(|action| endpoint == format!("/{action}/data/{name}_auth_key"))
                    || matches!(engine, "Custom_Whisper" | "OpenAI_Compatible")
                        && endpoint == format!("/set/data/{name}_url")
            }
        })
    }
    pub fn register(self: &Arc<Self>, mut router: Router) -> Router {
        for &(name, engine) in PROVIDERS {
            if !local(engine) {
                for action in ["get", "set", "delete"] {
                    let this = self.clone();
                    let endpoint = format!("/{action}/data/{name}_auth_key");
                    router = router.handle(&endpoint, move |data| {
                        let this = this.clone();
                        async move {
                            let _guard = this.serial.lock().await;
                            match action {
                                "get" => (200, this.key(engine)),
                                "delete" => {
                                    this.clear(name, engine);
                                    (200, Value::Null)
                                }
                                _ => {
                                    this.refresh(
                                        name,
                                        engine,
                                        Some(crate::settings::pyconv::py_str(
                                            &data.unwrap_or(Value::Null),
                                        )),
                                    )
                                    .await
                                }
                            }
                        }
                    });
                }
            } else {
                let this = self.clone();
                router = router.handle(&format!("/get/data/connected_{name}"), move |_| {
                    let this = this.clone();
                    async move {
                        (
                            200,
                            json!(
                                this.settings
                                    .get(status_property(engine))
                                    .unwrap_or_default()[engine]
                                    == true
                            ),
                        )
                    }
                });
                let this = self.clone();
                router = router.handle(&format!("/run/{name}_connection"), move |_| {
                    let this = this.clone();
                    async move {
                        let _guard = this.serial.lock().await;
                        this.refresh(name, engine, None).await
                    }
                });
            }
            if let Some(property) = url_property(engine)
                .filter(|_| matches!(engine, "LMStudio" | "OpenAI_Compatible" | "Custom_Whisper"))
            {
                let this = self.clone();
                let route_name = if engine == "Custom_Whisper" {
                    "custom_whisper"
                } else {
                    name
                };
                router = router.handle(&format!("/set/data/{route_name}_url"), move |data| {
                    let this = this.clone();
                    async move {
                        let _guard = this.serial.lock().await;
                        let old = this.settings.get(property).unwrap_or(Value::Null);
                        this.change_url(name, engine, property, data.unwrap_or(Value::Null), old)
                            .await
                    }
                });
            }
        }
        router
    }
    pub async fn change_url(
        &self,
        name: &str,
        engine: &str,
        property: &str,
        data: Value,
        old: Value,
    ) -> Reply {
        let text = crate::settings::pyconv::py_str(&data).trim().to_string();
        let code = if engine == "Custom_Whisper" {
            "CONNECTION_TRANSCRIPTION_CUSTOM_URL_INVALID"
        } else {
            "CONNECTION_OPENAI_COMPATIBLE_URL_INVALID"
        };
        if engine == "OpenAI_Compatible" && text.is_empty() {
            return crate::errors::reply(code, old, None);
        }
        let key = self
            .key(engine)
            .as_str()
            .filter(|key| !key.is_empty())
            .map(str::to_string);
        if local(engine) || key.is_none() {
            if self.settings.set(property, json!(text)).is_err() {
                return crate::errors::reply(code, old, None);
            }
            if local(engine) {
                let _ = self.refresh(name, engine, None).await;
            }
            return (200, self.settings.get(property).unwrap_or(Value::Null));
        }
        match self
            .catalog
            .fetch(engine.into(), key.clone(), Some(text.clone()))
            .await
        {
            Ok(models)
                if models
                    .names
                    .as_ref()
                    .is_some_and(|models| !models.is_empty()) =>
            {
                if self.settings.set(property, json!(text)).is_err() {
                    return crate::errors::reply(code, old, None);
                }
                self.set_status(engine, true);
                self.models(name, models.names.unwrap_or_default());
                self.refresh_selection(engine);
                (200, self.settings.get(property).unwrap_or(Value::Null))
            }
            _ => {
                self.set_status(engine, false);
                self.models(name, Vec::new());
                self.refresh_selection(engine);
                crate::errors::reply(code, old, None)
            }
        }
    }
    fn key(&self, engine: &str) -> Value {
        self.settings
            .get(key_property(engine))
            .unwrap_or_default()
            .get(engine)
            .cloned()
            .unwrap_or(Value::Null)
    }
    fn set_status(&self, engine: &str, enabled: bool) {
        let prop = status_property(engine);
        let mut status = self.settings.get(prop).unwrap_or(json!({}));
        status[engine] = json!(enabled);
        let _ = self.settings.set(prop, status);
    }
    fn models(&self, name: &str, names: Vec<String>) {
        let list = format!("SELECTABLE_{}_MODEL_LIST", name.to_uppercase());
        let selected = format!("SELECTED_{}_MODEL", name.to_uppercase());
        let _ = self.settings.set(&list, json!(names));
        let current = self.settings.get(&selected).unwrap_or(Value::Null);
        if !names.iter().any(|n| current == n.as_str()) {
            let _ = self.settings.set(
                &selected,
                names.first().map(|n| json!(n)).unwrap_or(Value::Null),
            );
        }
        self.controller
            .emit(&format!("/run/selectable_{name}_model_list"), json!(names));
        self.controller.emit(
            &format!("/run/selected_{name}_model"),
            self.settings.get(&selected).unwrap_or(Value::Null),
        );
    }
    fn clear(&self, name: &str, engine: &str) {
        let prop = key_property(engine);
        let mut keys = self.settings.get(prop).unwrap_or(json!({}));
        keys[engine] = Value::Null;
        let _ = self.settings.set(prop, keys);
        self.set_status(engine, false);
        if engine != "DeepL_API" {
            self.models(name, Vec::new());
        }
        if engine == "Deepgram" {
            let _ = self.settings.set("DEEPGRAM_MODEL_LANGUAGES", json!({}));
        }
        self.refresh_selection(engine);
    }
    fn refresh_selection(&self, engine: &str) {
        if speech(engine) {
            let status = self
                .settings
                .get("SELECTABLE_TRANSCRIPTION_ENGINE_STATUS")
                .unwrap_or_default();
            self.controller.refresh_transcription();
            self.controller.emit(
                "/get/data/selectable_transcription_engines",
                json!(crate::settings::tables::TRANSCRIPTION_ENGINES
                    .iter()
                    .filter(|e| status[**e] == true)
                    .collect::<Vec<_>>()),
            );
            self.controller
                .emit("/run/selectable_language_list", self.controller.languages());
        } else {
            self.controller.refresh_engines();
        }
    }
    pub async fn refresh(&self, name: &str, engine: &str, key: Option<String>) -> Reply {
        let key = key.map(|key| {
            if speech(engine) || engine == "OpenAI_Compatible" {
                key.trim().into()
            } else {
                key
            }
        });
        if let Some(code) = invalid_key(engine, key.as_deref().unwrap_or_default()) {
            let old = if engine == "DeepL_API" {
                self.key(engine)
            } else {
                self.clear(name, engine);
                Value::Null
            };
            return crate::errors::reply(code, old, None);
        }
        let base = url_property(engine).and_then(|p| self.settings.get_str(p));
        match self.catalog.fetch(engine.into(), key.clone(), base).await {
            Ok(models) if models.names.as_ref().is_none_or(|names| !names.is_empty()) => {
                if !local(engine) {
                    let prop = key_property(engine);
                    let mut keys = self.settings.get(prop).unwrap_or(json!({}));
                    keys[engine] = json!(key);
                    let _ = self.settings.set(prop, keys);
                }
                self.set_status(engine, true);
                if let Some(names) = models.names {
                    self.models(name, names);
                }
                if engine == "Deepgram" {
                    let _ = self
                        .settings
                        .set("DEEPGRAM_MODEL_LANGUAGES", models.languages);
                }
                self.refresh_selection(engine);
                (
                    200,
                    if local(engine) {
                        json!(true)
                    } else {
                        self.key(engine)
                    },
                )
            }
            _ => {
                let old = if engine == "DeepL_API" {
                    self.key(engine)
                } else {
                    self.clear(name, engine);
                    if local(engine) {
                        json!(false)
                    } else {
                        Value::Null
                    }
                };
                failed(engine, old)
            }
        }
    }
    pub async fn initialize(&self) {
        let _guard = self.serial.lock().await;
        for &(name, engine) in PROVIDERS {
            let key = self
                .key(engine)
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            if local(engine) || key.is_some() {
                let _ = self.refresh(name, engine, key).await;
            }
        }
    }
}
fn invalid_key(engine: &str, key: &str) -> Option<&'static str> {
    let count = key.chars().count();
    match engine {
        "DeepL_API" if ![36, 39].contains(&count) => Some("AUTH_DEEPL_LENGTH"),
        "Plamo_API" if count < 72 => Some("AUTH_PLAMO_LENGTH"),
        "Gemini_API" if count < 39 => Some("AUTH_GEMINI_LENGTH"),
        "OpenAI_API" if !key.starts_with("sk-") || count < 164 => Some("AUTH_OPENAI_INVALID"),
        "Groq_API" if !key.starts_with("gsk") || count < 40 => Some("AUTH_GROQ_INVALID"),
        "OpenRouter_API" if count < 20 => Some("AUTH_OPENROUTER_INVALID"),
        "OpenAI_Compatible" if key.is_empty() => Some("AUTH_OPENAI_COMPATIBLE_INVALID"),
        engine if speech(engine) && key.is_empty() => Some("TRANSCRIPTION_API_AUTH_FAILED"),
        _ => None,
    }
}
fn failed(engine: &str, old: Value) -> Reply {
    let code = if speech(engine) {
        "TRANSCRIPTION_API_AUTH_FAILED".into()
    } else {
        format!(
            "{}_{}_FAILED",
            if local(engine) { "CONNECTION" } else { "AUTH" },
            engine.trim_end_matches("_API").to_uppercase()
        )
    };
    crate::errors::reply(&code, old, None)
}
