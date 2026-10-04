//! Router -> session -> message pipeline -> translation -> outputs.
//!
//! Startup never opens an audio device. Blocking lifecycle and translation
//! work runs off Tokio's executor. All services share native Settings, and
//! one worker reconfigures active audio sessions after settings changes.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};

use serde_json::{json, Value};

use crate::pipeline::spec::{endpoints, Direction};
use crate::pipeline::{
    Host, LargeLog, Pipeline, SharedHistory, SmallLog, TranslateError, Translated,
};
use crate::protocol::Response;
use crate::router::{Reply, ResponseSink, Router};
use crate::settings::Settings;
use crate::sinks::Sinks;
use crate::transcription::native::Config;
use crate::transcription::session::{AudioSession, Backend, Kind, Level, Message};
use crate::translation::flow::TranslationFlow;
use crate::translation::native::{EngineClient, NativeTranslator};

/// Outputs can be substituted in tests without a clipboard, socket, or game.
pub trait Outputs: Send + Sync {
    fn osc(&self, message: &str, notification: bool) -> Result<(), String>;
    fn clipboard(&self, text: &str) -> Result<(), String>;
    fn websocket_alive(&self) -> bool;
    fn websocket_send(&self, message: Value);
    fn log_info(&self, text: &str) -> Result<(), String>;
}

impl Outputs for Sinks {
    fn osc(&self, message: &str, notification: bool) -> Result<(), String> {
        self.message(message, notification)
    }
    fn clipboard(&self, text: &str) -> Result<(), String> {
        self.copy_and_paste(text)
    }
    fn websocket_alive(&self) -> bool {
        Sinks::websocket_alive(self)
    }
    fn websocket_send(&self, message: Value) {
        self.broadcast(&message.to_string());
    }
    fn log_info(&self, text: &str) -> Result<(), String> {
        Sinks::log_info(self, text)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

// The sidecar checks credentials and clears rejected keys. Follow that checked
// config as it changes, preserving a client's history when only its model changes.
const CLIENTS: &[(&str, &str, Option<&str>)] = &[
    ("Plamo_API", "SELECTED_PLAMO_MODEL", None),
    ("Gemini_API", "SELECTED_GEMINI_MODEL", None),
    ("OpenAI_API", "SELECTED_OPENAI_MODEL", None),
    ("Groq_API", "SELECTED_GROQ_MODEL", None),
    ("OpenRouter_API", "SELECTED_OPENROUTER_MODEL", None),
    ("LMStudio", "SELECTED_LMSTUDIO_MODEL", Some("LMSTUDIO_URL")),
    (
        "OpenAI_Compatible",
        "SELECTED_OPENAI_COMPATIBLE_MODEL",
        Some("OPENAI_COMPATIBLE_URL"),
    ),
    ("Ollama", "SELECTED_OLLAMA_MODEL", None),
];

struct NativeHost {
    settings: Arc<Settings>,
    translator: Arc<NativeTranslator>,
    flow: TranslationFlow,
    clients: Mutex<HashMap<String, EngineClient>>,
    outputs: Arc<dyn Outputs>,
    sink: Arc<dyn ResponseSink>,
    transliterator: Mutex<Option<crate::transliteration::Transliterator>>,
    mute: Mutex<Option<bool>>,
    overlay: Mutex<Option<Arc<crate::overlay::Overlay>>>,
}

impl NativeHost {
    fn sync_clients(&self) {
        let keys = self.settings.get("AUTH_KEYS").unwrap_or(Value::Null);
        self.translator.set_deepl_key(
            keys.get("DeepL_API")
                .and_then(Value::as_str)
                .map(str::to_string),
        );
        let mut previous = lock(&self.clients);
        for &(engine, model_key, url_key) in CLIENTS {
            let api_key = keys
                .get(engine)
                .and_then(Value::as_str)
                .filter(|key| !key.is_empty())
                .map(str::to_string);
            let model = self.settings.get_str(model_key).unwrap_or_default();
            let base_url = url_key.and_then(|key| self.settings.get_str(key));
            let local = matches!(engine, "LMStudio" | "Ollama");
            if model.is_empty() || (!local && api_key.is_none()) {
                if previous.remove(engine).is_some() {
                    self.translator.remove_client(engine);
                }
                continue;
            }
            let next = EngineClient {
                api_key,
                base_url,
                model,
            };
            match previous.get(engine) {
                Some(old) if old == &next => continue,
                Some(old) if old.api_key == next.api_key && old.base_url == next.base_url => {
                    self.translator.set_model(engine, &next.model);
                }
                _ => self.translator.set_client(engine, next.clone()),
            }
            previous.insert(engine.to_string(), next);
        }
    }

    fn output_result(&self, result: Result<(), String>) {
        if let Err(error) = result {
            self.log(&format!("output failed: {error}"));
        }
    }
}

impl Host for NativeHost {
    fn run(&self, status: u16, endpoint: &str, payload: Value) {
        self.sink.emit(Response::new(status, endpoint, payload));
    }
    fn log(&self, text: &str) {
        eprintln!("[native pipeline] {text}");
    }
    fn translate(
        &self,
        direction: Direction,
        message: &str,
        language: Option<&str>,
    ) -> Result<Translated, TranslateError> {
        self.sync_clients();
        self.flow.translate(direction, message, language)
    }
    // Guarded by Runtime::unsupported before processing. These ports remain for
    // the later transliteration/overlay slices; do not silently discard a feature.
    fn transliterate(&self, text: &str, hiragana: bool, romaji: bool) -> Vec<Value> {
        let outcome = lock(&self.transliterator)
            .as_ref()
            .map(|t| t.transliterate(text, hiragana, romaji));
        match outcome {
            Some(Ok(result)) => result,
            Some(Err(error)) => {
                self.log(&error);
                Vec::new()
            }
            None => Vec::new(),
        }
    }
    fn overlay_available(&self) -> bool {
        lock(&self.overlay)
            .as_ref()
            .is_some_and(|overlay| overlay.available())
    }
    fn overlay_small_log(&self, message: &SmallLog<'_>) {
        if let Some(overlay) = lock(&self.overlay).as_ref() {
            self.output_result(overlay.small_log(message));
        }
    }
    fn overlay_large_log(&self, message: &LargeLog<'_>) {
        if let Some(overlay) = lock(&self.overlay).as_ref() {
            self.output_result(overlay.large_log(message));
        }
    }
    fn mic_mute_status(&self) -> Option<bool> {
        *lock(&self.mute)
    }
    fn send_osc(&self, message: &str) {
        self.output_result(self.outputs.osc(
            message,
            self.settings.get_bool("NOTIFICATION_VRC_SFX") == Some(true),
        ));
    }
    fn set_clipboard(&self, text: &str) {
        self.output_result(self.outputs.clipboard(text));
    }
    fn websocket_alive(&self) -> bool {
        self.outputs.websocket_alive()
    }
    fn websocket_send(&self, message: Value) {
        self.outputs.websocket_send(message);
    }
    fn log_info(&self, text: &str) {
        self.output_result(self.outputs.log_info(text));
    }
    fn set_setting(&self, name: &str, value: Value) {
        if let Err(error) = self.settings.set(name, value) {
            self.log(&format!("cannot set {name}: {error}"));
        }
    }
    fn fall_back_to_ctranslate2(&self) {
        let tab = self.settings.get_str("SELECTED_TAB_NO").unwrap_or_default();
        if let Some(mut engines) = self.settings.get("SELECTED_TRANSLATION_ENGINES") {
            engines[&tab] = json!("CTranslate2");
            self.set_setting("SELECTED_TRANSLATION_ENGINES", engines.clone());
            self.run(200, "/run/selected_translation_engines", engines);
        }
    }
    fn disable_translation(&self) {
        self.set_setting("ENABLE_TRANSLATION", json!(false));
    }
}

fn flags(kind: Kind) -> (&'static str, &'static str, &'static str) {
    match kind {
        Kind::Mic => (
            "ENABLE_TRANSCRIPTION_SEND",
            "ENABLE_CHECK_ENERGY_SEND",
            "transcription_send",
        ),
        Kind::Speaker => (
            "ENABLE_TRANSCRIPTION_RECEIVE",
            "ENABLE_CHECK_ENERGY_RECEIVE",
            "transcription_receive",
        ),
    }
}

fn meter(kind: Kind) -> &'static str {
    match kind {
        Kind::Mic => "check_mic_threshold",
        Kind::Speaker => "check_speaker_threshold",
    }
}

enum Change {
    Setting(String),
    Mute(Option<bool>),
    Stop,
}

pub struct Runtime {
    settings: Arc<Settings>,
    host: Arc<NativeHost>,
    pipeline: Pipeline,
    osc_query: Mutex<Option<Arc<crate::osc_query::OscQueryService>>>,
    mic: AudioSession,
    speaker: AudioSession,
    mic_lock: Mutex<()>,
    speaker_lock: Mutex<()>,
    ready: AtomicBool,
    stopped: AtomicBool,
    changes: mpsc::Sender<Change>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl Runtime {
    pub fn new(
        settings: Arc<Settings>,
        backend: Arc<dyn Backend>,
        translator: Arc<NativeTranslator>,
        outputs: Arc<dyn Outputs>,
        sink: Arc<dyn ResponseSink>,
    ) -> Arc<Self> {
        let history = SharedHistory::default();
        let flow = TranslationFlow::new(
            settings.clone() as Arc<dyn Config>,
            translator.clone(),
            history.clone(),
        );
        let host = Arc::new(NativeHost {
            settings: settings.clone(),
            translator,
            flow,
            clients: Mutex::default(),
            outputs,
            sink,
            transliterator: Mutex::new(None),
            mute: Mutex::new(None),
            overlay: Mutex::new(None),
        });
        let (changes, rx) = mpsc::channel();
        let runtime = Arc::new(Self {
            pipeline: Pipeline::with_history(settings.clone(), host.clone(), history),
            osc_query: Mutex::new(None),
            host,
            settings: settings.clone(),
            mic: AudioSession::new(Kind::Mic, backend.clone()),
            speaker: AudioSession::new(Kind::Speaker, backend),
            mic_lock: Mutex::new(()),
            speaker_lock: Mutex::new(()),
            ready: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
            changes: changes.clone(),
            worker: Mutex::new(None),
        });
        for kind in [Kind::Mic, Kind::Speaker] {
            let weak = Arc::downgrade(&runtime);
            runtime
                .session(kind)
                .set_message_callback(Some(Arc::new(move |message| {
                    if let Some(runtime) = weak.upgrade() {
                        runtime.message(kind, message);
                    }
                })));
            let host = runtime.host.clone();
            runtime.session(kind).set_level_callback(Arc::new(move |level| match level {
                Level::Value(value) => host.run(200, if kind == Kind::Mic { "/run/check_mic_volume" } else { "/run/check_speaker_volume" }, json!(value)),
                Level::NoDevice => host.run(400, endpoints::ERROR_DEVICE, json!({
                    "error_code": if kind == Kind::Mic { "DEVICE_NO_MIC" } else { "DEVICE_NO_SPEAKER" },
                    "message": format!("No {} device detected", kind.as_str()), "data": null,
                    "details": {}, "category": "device", "severity": "error",
                })),
            }));
        }
        // Listeners run under Settings' listener lock. They enqueue only: calling
        // set/reconfigure from here would re-enter that lock or block the bridge.
        settings.subscribe(move |name, _| {
            let _ = changes.send(Change::Setting(name.to_string()));
        });
        let weak = Arc::downgrade(&runtime);
        *lock(&runtime.worker) = Some(thread::spawn(move || {
            while let Ok(change) = rx.recv() {
                let name = match change {
                    Change::Setting(name) => name,
                    Change::Mute(mute) => {
                        if let Some(runtime) = weak.upgrade() {
                            runtime.apply_mute(mute);
                        }
                        continue;
                    }
                    Change::Stop => break,
                };
                let mut names = vec![name];
                for change in rx.try_iter() {
                    match change {
                        Change::Setting(name) => names.push(name),
                        Change::Mute(mute) => {
                            if let Some(runtime) = weak.upgrade() {
                                runtime.apply_mute(mute);
                            }
                        }
                        Change::Stop => return,
                    }
                }
                let Some(runtime) = weak.upgrade() else { break };
                if names.iter().any(|name| name == "VRC_MIC_MUTE_SYNC") {
                    let mute = *lock(&runtime.host.mute);
                    runtime.apply_mute(mute);
                }
                for kind in [Kind::Mic, Kind::Speaker] {
                    if names.iter().any(|name| restart_setting(kind, name)) {
                        runtime.refresh(kind);
                    }
                }
            }
        }));
        runtime
    }

    /// Called after native config/auth/model checks. No capture.
    pub fn activate(&self) {
        self.ready.store(true, Ordering::SeqCst);
    }

    pub fn install_transliterator(&self, transliterator: crate::transliteration::Transliterator) {
        *lock(&self.host.transliterator) = Some(transliterator);
    }

    pub fn install_overlay(&self, overlay: Arc<crate::overlay::Overlay>) {
        *lock(&self.host.overlay) = Some(overlay);
    }

    /// OSC callbacks enqueue only; the lifecycle worker pauses recording after
    /// draining buffered speech, without opening a new microphone.
    pub fn install_osc(self: &Arc<Self>) -> Result<(), String> {
        let changes = self.changes.clone();
        let service = Arc::new(crate::osc_query::OscQueryService::new(Arc::new(
            move |mute| {
                let _ = changes.send(Change::Mute(mute));
            },
        )));
        *lock(&self.osc_query) = Some(service);
        self.configure_osc()
    }

    pub fn osc_query_available(&self) -> bool {
        lock(&self.osc_query)
            .as_ref()
            .is_some_and(|service| service.query_enabled())
    }

    pub fn configure_osc(&self) -> Result<(), String> {
        if let Some(service) = lock(&self.osc_query).as_ref() {
            service.configure(
                &self
                    .settings
                    .get_str("OSC_IP_ADDRESS")
                    .unwrap_or_else(|| "127.0.0.1".into()),
                self.settings
                    .get("OSC_PORT")
                    .and_then(|p| p.as_u64())
                    .and_then(|p| u16::try_from(p).ok())
                    .unwrap_or(9000),
            )?;
            service.start()?;
            let enabled = service.query_enabled();
            let disabled = !enabled && self.settings.get_bool("VRC_MIC_MUTE_SYNC") == Some(true);
            if disabled {
                self.host.set_setting("VRC_MIC_MUTE_SYNC", json!(false));
                self.host
                    .run(200, "/set/disable/vrc_mic_mute_sync", json!(false));
            }
            self.host.run(200,"/run/enable_osc_query",json!({"data":enabled,"disabled_functions":if disabled{vec!["vrc_mic_mute_sync"]}else{Vec::<&str>::new()}}));
        }
        Ok(())
    }

    fn apply_mute(&self, mute: Option<bool>) {
        *lock(&self.host.mute) = mute;
        if self.stopped.load(Ordering::SeqCst) {
            return;
        }
        let _guard = self.lifecycle(Kind::Mic);
        if self.settings.get_bool("VRC_MIC_MUTE_SYNC") == Some(true) && mute == Some(true) {
            self.mic.pause();
        } else {
            self.mic.resume();
        }
    }

    pub fn ocr_message(&self, message: Value) {
        if self.unavailable().is_some()
            || self.settings.get_bool("ENABLE_OCR_CAPTURE") != Some(true)
        {
            return;
        }
        if let Err(error) = self.pipeline.ocr_message(&message) {
            self.host.log(&error.to_string());
        }
    }

    fn session(&self, kind: Kind) -> &AudioSession {
        if kind == Kind::Mic {
            &self.mic
        } else {
            &self.speaker
        }
    }
    fn lifecycle(&self, kind: Kind) -> MutexGuard<'_, ()> {
        lock(if kind == Kind::Mic {
            &self.mic_lock
        } else {
            &self.speaker_lock
        })
    }

    fn unsupported(&self, kind: Option<Kind>) -> Option<String> {
        if kind.is_some()
            && self
                .settings
                .get_str("SELECTED_TRANSCRIPTION_ENGINE")
                .as_deref()
                == Some("Deepgram")
        {
            let model = self
                .settings
                .get_str("SELECTED_DEEPGRAM_MODEL")
                .unwrap_or_default();
            if self
                .settings
                .get("DEEPGRAM_MODEL_LANGUAGES")
                .and_then(|languages| languages.get(&model).cloned())
                .and_then(|languages| languages.as_array().cloned())
                .is_none_or(|languages| languages.is_empty())
            {
                return Some(
                    "Deepgram model language metadata is not available in the native pipeline yet"
                        .into(),
                );
            }
        }
        let mut features = Vec::new();
        if lock(&self.host.overlay).is_none() {
            features.extend(["OVERLAY_SMALL_LOG", "OVERLAY_LARGE_LOG"]);
        }
        if lock(&self.host.transliterator).is_none() {
            features.extend(["CONVERT_MESSAGE_TO_HIRAGANA", "CONVERT_MESSAGE_TO_ROMAJI"]);
        }
        if kind == Some(Kind::Mic) && !self.osc_query_available() {
            features.push("VRC_MIC_MUTE_SYNC");
        }
        features
            .into_iter()
            .find(|name| self.settings.get_bool(name) == Some(true))
            .map(|name| format!("{name} is not available in the native pipeline yet"))
    }

    fn unavailable(&self) -> Option<Reply> {
        (!self.ready.load(Ordering::SeqCst) || self.stopped.load(Ordering::SeqCst)).then(|| {
            (
                503,
                json!({"message": "Native pipeline is not ready", "data": false}),
            )
        })
    }

    pub fn set_audio(&self, kind: Kind, transcript: bool, enabled: bool) -> Reply {
        let _guard = self.lifecycle(kind);
        if let Some(reply) = self.unavailable() {
            return reply;
        }
        if enabled && transcript {
            if let Some(message) = self.unsupported(Some(kind)) {
                return (409, json!({"message": message, "data": false}));
            }
        }
        let session = self.session(kind);
        let (speech_flag, meter_flag, _) = flags(kind);
        let flag = if transcript { speech_flag } else { meter_flag };
        // Set before spawning: an immediate transcript sees enabled=true, but a
        // synchronous or concurrent failure is free to reset it to false.
        self.host.set_setting(flag, json!(enabled));
        let result = session.reconfigure(
            transcript.then_some(enabled),
            (!transcript).then_some(enabled),
            None,
        );
        if enabled && kind == Kind::Mic {
            if self.settings.get_bool("VRC_MIC_MUTE_SYNC") == Some(true)
                && *lock(&self.host.mute) == Some(true)
            {
                session.pause();
            } else {
                session.resume();
            }
        }
        let wanted = if transcript {
            session.wants_transcript()
        } else {
            session.wants_energy()
        };
        if result.is_err() || !wanted {
            self.host.set_setting(flag, json!(false));
        }
        (200, json!(self.settings.get_bool(flag) == Some(true)))
    }

    fn message(&self, kind: Kind, message: Message) {
        if self.stopped.load(Ordering::SeqCst) {
            return;
        }
        if matches!(message, Message::Transcript { .. })
            && self.settings.get_bool(flags(kind).0) != Some(true)
        {
            return;
        }
        if matches!(message, Message::Failure(_)) {
            let (_, meter_flag, _) = flags(kind);
            if self.settings.get_bool(meter_flag) == Some(true) {
                self.host.set_setting(meter_flag, json!(false));
                self.host
                    .run(200, &format!("/set/disable/{}", meter(kind)), json!(false));
            }
        } else if let Some(message) = self.unsupported(Some(kind)) {
            self.host.log(&message);
            return;
        }
        self.update_filter();
        let result = if kind == Kind::Mic {
            self.pipeline.mic_message(&message.to_value())
        } else {
            self.pipeline.speaker_message(&message.to_value())
        };
        if let Err(error) = result {
            self.host.log(&error.to_string());
        }
    }

    fn update_filter(&self) {
        let words = self.settings.get("MIC_WORD_FILTER").unwrap_or(Value::Null);
        let words: Vec<_> = words
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        self.pipeline.set_word_filter(&words);
    }

    pub fn chat(&self, data: Option<Value>) -> Reply {
        if let Some(reply) = self.unavailable() {
            return reply;
        }
        if let Some(message) = self.unsupported(None) {
            return (409, json!({"message": message, "data": null}));
        }
        match self.pipeline.chat_message(&data.unwrap_or(Value::Null)) {
            Ok(reply) => (
                reply["status"].as_u64().unwrap_or(200) as u16,
                reply["result"].clone(),
            ),
            Err(error) => {
                self.host.log(&error.to_string());
                (500, json!("Internal error"))
            }
        }
    }

    fn refresh(&self, kind: Kind) {
        let _guard = self.lifecycle(kind);
        if self.unavailable().is_some() {
            return;
        }
        let session = self.session(kind);
        let (transcript, energy) = (session.wants_transcript(), session.wants_energy());
        if !transcript && !energy {
            return;
        }
        session.stop();
        if transcript && self.unsupported(Some(kind)).is_some() {
            self.host.set_setting(flags(kind).0, json!(false));
            self.host.run(
                200,
                &format!("/set/disable/{}", flags(kind).2),
                json!(false),
            );
            if energy {
                let _ = session.reconfigure(Some(false), Some(true), None);
            }
        } else {
            let result = session.reconfigure(Some(transcript), Some(energy), None);
            // An error has already gone through message(Failure), which resets
            // both flags and tells the UI once. Only no-device needs a sync here.
            if result.is_ok() && transcript && !session.wants_transcript() {
                self.host.set_setting(flags(kind).0, json!(false));
                self.host.run(
                    200,
                    &format!("/set/disable/{}", flags(kind).2),
                    json!(false),
                );
            }
        }
        if energy && !session.wants_energy() && self.settings.get_bool(flags(kind).1) == Some(true)
        {
            self.host.set_setting(flags(kind).1, json!(false));
            self.host
                .run(200, &format!("/set/disable/{}", meter(kind)), json!(false));
        }
        if kind == Kind::Mic {
            let muted = *lock(&self.host.mute);
            if self.settings.get_bool("VRC_MIC_MUTE_SYNC") == Some(true) && muted == Some(true) {
                session.pause();
            }
        }
    }

    pub fn shutdown(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(query) = lock(&self.osc_query).take() {
            query.shutdown();
        }
        let _ = self.changes.send(Change::Stop);
        for kind in [Kind::Mic, Kind::Speaker] {
            let _guard = self.lifecycle(kind);
            self.host.set_setting(flags(kind).0, json!(false));
            self.host.set_setting(flags(kind).1, json!(false));
            self.session(kind).stop();
        }
        if let Some(worker) = lock(&self.worker).take() {
            let _ = worker.join();
        }
    }
}

fn restart_setting(kind: Kind, name: &str) -> bool {
    let prefix = if kind == Kind::Mic {
        "MIC_"
    } else {
        "SPEAKER_"
    };
    name.starts_with(prefix)
        || name == if kind == Kind::Mic { "SELECTED_MIC_HOST" } else { "SELECTED_SPEAKER_HOST" }
        || name.starts_with("SELECTED_TRANSCRIPTION_")
        || name.starts_with("SELECTED_WHISPER_")
        || name == "WHISPER_WEIGHT_TYPE"
        || name.starts_with("TRANSCRIPTION_")
        || matches!(
            name,
            "SELECTED_GROQ_WHISPER_MODEL"
                | "SELECTED_OPENAI_WHISPER_MODEL"
                | "SELECTED_CUSTOM_WHISPER_MODEL"
                | "SELECTED_DEEPGRAM_MODEL"
        )
        || name
            == if kind == Kind::Mic {
                "SELECTED_MIC_DEVICE"
            } else {
                "SELECTED_SPEAKER_DEVICE"
            }
}

pub fn register(mut router: Router, runtime: &Arc<Runtime>) -> Router {
    for kind in [Kind::Mic, Kind::Speaker] {
        for (transcript, feature) in [(true, flags(kind).2), (false, meter(kind))] {
            for enabled in [true, false] {
                let endpoint = format!(
                    "/set/{}/{feature}",
                    if enabled { "enable" } else { "disable" }
                );
                let runtime = runtime.clone();
                router = router.handle(&endpoint, move |_| {
                    let runtime = runtime.clone();
                    async move {
                        tokio::task::spawn_blocking(move || {
                            runtime.set_audio(kind, transcript, enabled)
                        })
                        .await
                        .unwrap_or((500, json!("Internal error")))
                    }
                });
            }
        }
    }
    let runtime = runtime.clone();
    router.handle("/run/send_message_box", move |data| {
        let runtime = runtime.clone();
        async move {
            tokio::task::spawn_blocking(move || runtime.chat(data))
                .await
                .unwrap_or((500, json!("Internal error")))
        }
    })
}
