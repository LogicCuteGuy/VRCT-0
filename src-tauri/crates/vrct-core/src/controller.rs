//! Native settings/controller endpoints. The checked-in contract records the
//! original public endpoint surface, and is also used to audit route coverage.
use crate::protocol::Response;
use crate::router::{Reply, ResponseSink, Router};
use crate::settings::{Devices, Settings};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

#[derive(Deserialize)]
pub struct Contract {
    pub endpoint: String,
    pub method: String,
    pub setting: Option<String>,
    pub references: Vec<String>,
}
pub fn contract() -> &'static [Contract] {
    static CONTRACT: OnceLock<Vec<Contract>> = OnceLock::new();
    CONTRACT.get_or_init(|| {
        serde_json::from_str(include_str!("../assets/controller_contract.json"))
            .expect("controller contract")
    })
}

/// Changes that also control a service must finish (or return an error) before
/// answering the UI. Implementations execute on a blocking worker, never under
/// the settings listener lock.
pub trait Effects: Send + Sync {
    fn changed(&self, name: &str) -> Result<(), String>;
}

pub struct Controller {
    settings: Arc<Settings>,
    sink: Arc<dyn ResponseSink>,
    devices: Arc<dyn Devices>,
    effects: Arc<dyn Effects>,
    keys: HashMap<String, String>,
    serial: std::sync::Mutex<()>,
}

fn error(message: impl Into<String>, old: Value) -> Reply {
    (
        400,
        json!({"error_code":"VALIDATION_CONFIG_VALUE_INVALID", "message":message.into(),
        "data":old, "details":{},"category":"validation","severity":"warning"}),
    )
}

impl Controller {
    pub fn new(
        settings: Arc<Settings>,
        sink: Arc<dyn ResponseSink>,
        devices: Arc<dyn Devices>,
        effects: Arc<dyn Effects>,
    ) -> Arc<Self> {
        let mut keys = HashMap::new();
        for row in contract() {
            if let Some(key) = &row.setting {
                keys.insert(row.endpoint.clone(), key.clone());
            } else if row.endpoint.starts_with("/get/data/") && row.references.len() == 1 {
                keys.insert(row.endpoint.clone(), row.references[0].clone());
            }
        }
        for &(name, provider) in crate::auth::PROVIDERS {
            let upper = name.to_uppercase();
            if provider != "DeepL_API" {
                keys.insert(
                    format!("/get/data/selectable_{name}_model_list"),
                    format!("SELECTABLE_{upper}_MODEL_LIST"),
                );
                keys.insert(
                    format!("/get/data/selected_{name}_model"),
                    format!("SELECTED_{upper}_MODEL"),
                );
            }
        }
        for row in contract()
            .iter()
            .filter(|r| r.endpoint.starts_with("/set/"))
        {
            let rest = row.endpoint.rsplit('/').next().unwrap_or_default();
            if let Some(key) = keys.get(&format!("/get/data/{rest}")).cloned() {
                keys.insert(row.endpoint.clone(), key);
            } else if row.references.len() == 1
                && crate::settings::schema::find(&row.references[0]).is_some()
            {
                keys.insert(row.endpoint.clone(), row.references[0].clone());
            }
        }
        Arc::new(Self {
            settings,
            sink,
            devices,
            effects,
            keys,
            serial: std::sync::Mutex::new(()),
        })
    }

    pub fn owns(&self, endpoint: &str) -> bool {
        self.keys.contains_key(endpoint)
            || matches!(
                endpoint,
                "/get/data/selectable_translation_engines"
                    | "/get/data/selectable_transcription_engines"
                    | "/get/data/selectable_language_list"
                    | "/get/data/selectable_mic_host_list"
                    | "/get/data/selectable_mic_device_list"
                    | "/get/data/selectable_speaker_device_list"
                    | "/run/swap_your_language_and_target_language"
            )
    }

    pub fn register(self: &Arc<Self>, mut router: Router) -> Router {
        for row in contract().iter().filter(|row| self.owns(&row.endpoint)) {
            let this = self.clone();
            let endpoint = row.endpoint.clone();
            router = router.handle(&row.endpoint, move |data| {
                let this = this.clone();
                let endpoint = endpoint.clone();
                async move {
                    tokio::task::spawn_blocking(move || {
                        this.answer(&endpoint, data.unwrap_or(Value::Null))
                    })
                    .await
                    .unwrap_or((500, json!("Internal error")))
                }
            });
        }
        router
    }

    pub fn answer(&self, endpoint: &str, data: Value) -> Reply {
        let _serial = self.serial.lock().unwrap_or_else(|p| p.into_inner());
        match endpoint {
            "/get/data/selectable_translation_engines" => return (200, self.translation_engines()),
            "/get/data/selectable_transcription_engines" => {
                let status = self
                    .settings
                    .get("SELECTABLE_TRANSCRIPTION_ENGINE_STATUS")
                    .unwrap_or_default();
                return (
                    200,
                    json!(crate::settings::tables::TRANSCRIPTION_ENGINES
                        .iter()
                        .filter(|e| status[**e] == true)
                        .collect::<Vec<_>>()),
                );
            }
            "/get/data/selectable_language_list" => return (200, self.languages()),
            "/get/data/selectable_mic_host_list" => return (200, json!(self.devices.mic_hosts())),
            "/get/data/selectable_mic_device_list" => {
                return (
                    200,
                    json!(self.devices.mic_device_names(
                        &self
                            .settings
                            .get_str("SELECTED_MIC_HOST")
                            .unwrap_or_default()
                    )),
                )
            }
            "/get/data/selectable_speaker_device_list" => {
                return (200, json!(self.devices.speaker_device_names()))
            }
            "/run/swap_your_language_and_target_language" => return self.swap(),
            _ => {}
        }
        let Some(key) = self.keys.get(endpoint) else {
            return (404, json!("Invalid endpoint"));
        };
        if endpoint.starts_with("/get/") {
            return (200, self.settings.get(key).unwrap_or(Value::Null));
        }
        let old = self.settings.get(key).unwrap_or(Value::Null);
        let dependent: Vec<_> = match key.as_str() {
            "SELECTED_TRANSLATION_COMPUTE_DEVICE" => vec!["SELECTED_TRANSLATION_COMPUTE_TYPE"],
            "SELECTED_TRANSCRIPTION_COMPUTE_DEVICE" => vec!["SELECTED_TRANSCRIPTION_COMPUTE_TYPE"],
            "SELECTED_MIC_HOST" => vec!["SELECTED_MIC_DEVICE"],
            "WEBSOCKET_SERVER" => vec!["OBS_BROWSER_SOURCE"],
            "OBS_BROWSER_SOURCE" => vec!["WEBSOCKET_SERVER"],
            _ => Vec::new(),
        }
        .into_iter()
        .map(|key| (key, self.settings.get(key).unwrap_or(Value::Null)))
        .collect();
        if endpoint.starts_with("/set/data/selected_") && endpoint.ends_with("_model") {
            let provider = endpoint
                .trim_start_matches("/set/data/selected_")
                .trim_end_matches("_model");
            let model = crate::settings::pyconv::py_str(&data);
            let list = self
                .settings
                .get(&format!(
                    "SELECTABLE_{}_MODEL_LIST",
                    provider.to_uppercase()
                ))
                .unwrap_or_default();
            if !list.as_array().is_some_and(|a| a.contains(&json!(model))) {
                let code = if provider.ends_with("whisper") || provider == "deepgram" {
                    "MODEL_TRANSCRIPTION_INVALID".into()
                } else {
                    format!("MODEL_{}_INVALID", provider.to_uppercase())
                };
                return crate::errors::reply(&code, old, None);
            }
        }
        let value = if endpoint.starts_with("/set/enable/") {
            json!(true)
        } else if endpoint.starts_with("/set/disable/") {
            json!(false)
        } else {
            data.clone()
        };
        let value = if key.starts_with("OBS_BROWSER_SOURCE_") && !key.ends_with("COLOR")
            || matches!(
                key.as_str(),
                "OSC_PORT"
                    | "WEBSOCKET_PORT"
                    | "OCR_POLL_INTERVAL_MS"
                    | "OCR_BUBBLE_MIN_TEXT_LENGTH"
            ) {
            match crate::settings::pyconv::py_int(&value)
                .and_then(crate::settings::pyconv::int_value)
            {
                Some(value) => value,
                None => return error("Value must be a number", old),
            }
        } else if key.ends_with("COLOR") {
            json!(crate::settings::pyconv::py_str(&value)
                .trim()
                .to_uppercase())
        } else {
            value
        };
        let reply = crate::setters::answer_for(endpoint, &self.settings, data).unwrap_or_else(
            || match self.settings.set(key, value) {
                Ok(()) => (200, self.settings.get(key).unwrap_or(Value::Null)),
                Err(e) => error(e.to_string(), old.clone()),
            },
        );
        if reply.0 != 200 {
            return reply;
        }
        if matches!(
            key.as_str(),
            "SELECTED_TRANSLATION_COMPUTE_DEVICE" | "SELECTED_TRANSCRIPTION_COMPUTE_DEVICE"
        ) {
            let type_key = if key == "SELECTED_TRANSLATION_COMPUTE_DEVICE" {
                "SELECTED_TRANSLATION_COMPUTE_TYPE"
            } else {
                "SELECTED_TRANSCRIPTION_COMPUTE_TYPE"
            };
            let _ = self.settings.set(type_key, json!("auto"));
            self.emit(&format!("/run/{}", type_key.to_lowercase()), json!("auto"));
        }
        if key == "SELECTED_MIC_HOST" {
            let host = self.settings.get_str(key).unwrap_or_default();
            let names = self.devices.mic_device_names(&host);
            let previous = dependent
                .first()
                .map(|(_, v)| v.clone())
                .unwrap_or_default();
            let selected = if names.iter().any(|n| previous == n.as_str()) {
                previous
            } else {
                self.devices
                    .default_mic()
                    .filter(|(h, n)| h == &host && names.contains(n))
                    .map(|(_, n)| json!(n))
                    .unwrap_or_else(|| names.first().map(|n| json!(n)).unwrap_or(json!("NoDevice")))
            };
            let _ = self.settings.set("SELECTED_MIC_DEVICE", selected.clone());
            self.emit("/run/selectable_mic_device_list", json!(names));
            self.emit("/run/selected_mic_device", selected);
        }
        if matches!(
            key.as_str(),
            "SELECTED_TRANSCRIPTION_ENGINE" | "SELECTED_DEEPGRAM_MODEL"
        ) {
            self.refresh_transcription();
        }
        if let Err(e) = self.effects.changed(key) {
            let _ = self.settings.set(key, old.clone());
            for (name, value) in dependent {
                let _ = self.settings.set(name, value);
            }
            let _ = self.effects.changed(key);
            return error(e, old);
        }
        if key.starts_with("SELECTED_")
            && (key.contains("LANGUAGE")
                || key == "SELECTED_TRANSLATION_ENGINES"
                || key == "SELECTED_TAB_NO")
        {
            self.refresh_engines();
        }
        (200, self.settings.get(key).unwrap_or(Value::Null))
    }

    pub fn emit(&self, endpoint: &str, result: Value) {
        self.sink.emit(Response::new(200, endpoint, result));
    }

    pub fn translation_engines(&self) -> Value {
        let tab = self.settings.get_str("SELECTED_TAB_NO").unwrap_or_default();
        let your = self
            .settings
            .get("SELECTED_YOUR_LANGUAGES")
            .unwrap_or_default();
        let target = self
            .settings
            .get("SELECTED_TARGET_LANGUAGES")
            .unwrap_or_default();
        let status = self
            .settings
            .get("SELECTABLE_TRANSLATION_ENGINE_STATUS")
            .unwrap_or_default();
        let weight = self
            .settings
            .get_str("CTRANSLATE2_WEIGHT_TYPE")
            .unwrap_or_default();
        let own = your[&tab]["1"]["language"].as_str().unwrap_or_default();
        let enabled = target[&tab]
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(_, v)| v["enable"] == true)
            .map(|(_, v)| v)
            .collect::<Vec<_>>();
        if enabled.iter().any(|v| v["language"] == own) {
            return if status["CTranslate2"] == true {
                json!(["CTranslate2"])
            } else {
                json!([])
            };
        }
        json!(crate::settings::tables::TRANSLATION_ENGINES
            .iter()
            .filter(|engine| status[**engine] == true
                && enabled.iter().all(|slot| {
                    crate::translation::languages::language_codes(
                        engine,
                        &weight,
                        slot["country"].as_str().unwrap_or_default(),
                        own,
                        slot["language"].as_str().unwrap_or_default(),
                    )
                    .is_ok()
                }))
            .collect::<Vec<_>>())
    }

    pub fn refresh_engines(&self) {
        let mut engines = self.translation_engines();
        let tab = self.settings.get_str("SELECTED_TAB_NO").unwrap_or_default();
        let mut selected = self
            .settings
            .get("SELECTED_TRANSLATION_ENGINES")
            .unwrap_or_default();
        if !engines
            .as_array()
            .is_some_and(|a| a.contains(&selected[&tab]))
        {
            selected[&tab] = json!("CTranslate2");
            let _ = self
                .settings
                .set("SELECTED_TRANSLATION_ENGINES", selected.clone());
        }
        let engine = selected[&tab].as_str().unwrap_or("CTranslate2");
        let names = crate::translation::languages::source_languages(
            engine,
            &self
                .settings
                .get_str("CTRANSLATE2_WEIGHT_TYPE")
                .unwrap_or_default(),
        );
        let available = crate::settings::tables::TRANSCRIPTION_LANGUAGES
            .iter()
            .filter(|(language, _)| names.iter().any(|n| n == language))
            .flat_map(|(language, countries)| {
                countries
                    .iter()
                    .map(move |country| json!({"language":language,"country":country}))
            })
            .collect::<Vec<_>>();
        if self.fallback_languages(Some(&tab), &available) {
            engines = self.translation_engines();
        }
        self.emit("/run/translation_engines", engines);
        self.emit("/run/selected_translation_engines", selected);
        self.emit("/run/selectable_language_list", self.languages());
    }

    pub fn languages(&self) -> Value {
        let deepgram = self
            .settings
            .get_str("SELECTED_TRANSCRIPTION_ENGINE")
            .as_deref()
            == Some("Deepgram");
        let model = self
            .settings
            .get_str("SELECTED_DEEPGRAM_MODEL")
            .unwrap_or_default();
        let metadata = self
            .settings
            .get("DEEPGRAM_MODEL_LANGUAGES")
            .unwrap_or_default();
        let codes: Vec<String> = metadata[&model]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        let mut result = Vec::new();
        let weight = self
            .settings
            .get_str("CTRANSLATE2_WEIGHT_TYPE")
            .unwrap_or_default();
        let translation_languages = crate::settings::tables::TRANSLATION_ENGINES
            .iter()
            .flat_map(|engine| crate::translation::languages::source_languages(engine, &weight))
            .collect::<std::collections::HashSet<_>>();
        for (language, countries) in crate::settings::tables::TRANSCRIPTION_LANGUAGES {
            if !translation_languages.contains(*language) {
                continue;
            }
            for country in *countries {
                if !deepgram
                    || crate::transcription::deepgram::is_language_supported(
                        language, country, &codes,
                    )
                {
                    result.push(json!({"language":language,"country":country}));
                }
            }
        }
        result.sort_by(|a, b| a["language"].as_str().cmp(&b["language"].as_str()));
        json!(result)
    }

    pub fn refresh_transcription(&self) {
        let selected = self
            .settings
            .get_str("SELECTED_TRANSCRIPTION_ENGINE")
            .unwrap_or_default();
        let status = self
            .settings
            .get("SELECTABLE_TRANSCRIPTION_ENGINE_STATUS")
            .unwrap_or_default();
        let mut next = selected.clone();
        if status[&selected] != true {
            let weight = self
                .settings
                .get_str("WHISPER_WEIGHT_TYPE")
                .unwrap_or_default();
            let available = self
                .settings
                .get("SELECTABLE_WHISPER_WEIGHT_TYPE_DICT")
                .unwrap_or_default()[&weight]
                == true;
            next = if matches!(selected.as_str(), "Google" | "Whisper") && available {
                let alternate = if selected == "Google" {
                    "Whisper"
                } else {
                    "Google"
                };
                if status[alternate] == true {
                    alternate.into()
                } else {
                    String::new()
                }
            } else {
                "Whisper".into()
            };
            let _ = self.settings.set(
                "SELECTED_TRANSCRIPTION_ENGINE",
                if next.is_empty() {
                    Value::Null
                } else {
                    json!(next)
                },
            );
        }
        let available = self.languages().as_array().cloned().unwrap_or_default();
        self.fallback_languages(None, &available);
        if selected != next {
            self.emit(
                "/run/selected_transcription_engine",
                if next.is_empty() {
                    Value::Null
                } else {
                    json!(next)
                },
            );
        }
        self.emit("/run/selectable_language_list", self.languages());
    }

    fn fallback_languages(&self, only_tab: Option<&str>, available: &[Value]) -> bool {
        let mut your = self
            .settings
            .get("SELECTED_YOUR_LANGUAGES")
            .unwrap_or_default();
        let mut target = self
            .settings
            .get("SELECTED_TARGET_LANGUAGES")
            .unwrap_or_default();
        let supported = |slot: &Value| {
            available
                .iter()
                .any(|v| v["language"] == slot["language"] && v["country"] == slot["country"])
        };
        let pick = |taken: &[String]| {
            ["Japanese", "English"]
                .iter()
                .flat_map(|name| available.iter().filter(move |v| v["language"] == *name))
                .chain(available.iter())
                .find(|v| !taken.iter().any(|name| v["language"] == name.as_str()))
                .cloned()
        };
        let tabs = your
            .as_object()
            .into_iter()
            .flatten()
            .map(|(tab, _)| tab.clone())
            .collect::<Vec<_>>();
        let mut changed = false;
        for tab in tabs {
            if only_tab.is_some_and(|only| only != tab) {
                continue;
            }
            let enabled = target[&tab]
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(_, v)| v["enable"] == true)
                .filter_map(|(_, v)| v["language"].as_str().map(str::to_string))
                .collect::<Vec<_>>();
            if !supported(&your[&tab]["1"]) {
                if let Some(mut default) = pick(&enabled) {
                    default["enable"] = json!(true);
                    your[&tab]["1"] = default;
                    changed = true;
                }
            }
            let mut taken = vec![your[&tab]["1"]["language"]
                .as_str()
                .unwrap_or_default()
                .to_string()];
            if let Some(slots) = target[&tab].as_object_mut() {
                for (_, slot) in slots.iter_mut() {
                    if slot["enable"] != true {
                        continue;
                    }
                    if !supported(slot) {
                        if let Some(default) = pick(&taken) {
                            slot["language"] = default["language"].clone();
                            slot["country"] = default["country"].clone();
                            changed = true;
                        }
                    }
                    taken.push(slot["language"].as_str().unwrap_or_default().to_string());
                }
            }
        }
        if changed {
            let _ = self.settings.set("SELECTED_YOUR_LANGUAGES", your.clone());
            let _ = self
                .settings
                .set("SELECTED_TARGET_LANGUAGES", target.clone());
            self.emit("/run/selected_your_languages", your);
            self.emit("/run/selected_target_languages", target);
        }
        changed
    }

    fn swap(&self) -> Reply {
        let tab = self.settings.get_str("SELECTED_TAB_NO").unwrap_or_default();
        let mut your = self
            .settings
            .get("SELECTED_YOUR_LANGUAGES")
            .unwrap_or_default();
        let mut target = self
            .settings
            .get("SELECTED_TARGET_LANGUAGES")
            .unwrap_or_default();
        let old = your[&tab]["1"].clone();
        your[&tab]["1"] = target[&tab]["1"].clone();
        target[&tab]["1"] = old;
        if self
            .settings
            .set("SELECTED_YOUR_LANGUAGES", your.clone())
            .is_err()
            || self
                .settings
                .set("SELECTED_TARGET_LANGUAGES", target.clone())
                .is_err()
        {
            return (400, json!(false));
        }
        self.emit("/run/selected_your_languages", your.clone());
        self.emit("/run/selected_target_languages", target.clone());
        self.refresh_engines();
        (200, json!({"your":your,"target":target}))
    }
}
