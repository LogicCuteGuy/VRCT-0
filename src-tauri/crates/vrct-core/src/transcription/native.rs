//! The session's surroundings in the real app: which device the settings select, how its recorder is
//! set up, and which engine recognises. The decisions follow `MicSession` / `SpeakerSession` in
//! `model.py` and `AudioTranscriber.__init__`; they are made by plain functions of the settings
//! ([`recorder_plan`], [`transcriber_plan`], [`ask`]) that are checked against the Python code, and
//! [`NativeBackend`] builds what they describe.
//!
//! Anything that touches the machine (the device list, a capture, the speech model) comes through
//! [`Platform`] and [`WhisperLoader`], so all of this runs in tests without a sound card.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use tokio::runtime::Handle;

use super::cloud::Blocking;
use super::deepgram::DeepgramProvider;
use super::google::GoogleProvider;
use super::openai::{OpenAiCompatible, ENGINES as API_ENGINES};
use super::phrases::{Engine, Format, PhraseTranscriber, Recognizer, Settings as PhraseSettings};
use super::recorder::{EnergyParams, Recorder};
use super::session::{Ask, Backend, EngineTranscriber, Kind, Transcriber};
use crate::audio::devices::{Device, DeviceList, NO_DEVICE};
use crate::audio::vad::VadConfig;
use crate::audio::FRAME_SAMPLES;
use crate::settings::Settings;

/// `GROQ_WHISPER_BASE_URL` and `OPENAI_WHISPER_BASE_URL` in `config.py`: not settings, fixed addresses.
pub const GROQ_WHISPER_BASE_URL: &str = "https://api.groq.com/openai/v1";
pub const OPENAI_WHISPER_BASE_URL: &str = "https://api.openai.com/v1";

/// Longest stretch of speech the segmenter lets go on before it cuts (`_MAX_SPEECH_DURATION_MS`).
const MAX_SPEECH_MS: f64 = 7000.0;
const FRAME_MS: f64 = FRAME_SAMPLES as f64 * 1000.0 / crate::audio::TARGET_SAMPLE_RATE as f64;

/// Where the settings are read from. [`Settings`] is the real one; tests give a map.
pub trait Config: Send + Sync {
    fn get(&self, name: &str) -> Option<Value>;
}

impl Config for Settings {
    fn get(&self, name: &str) -> Option<Value> {
        Settings::get(self, name)
    }
}

impl Config for std::collections::HashMap<String, Value> {
    fn get(&self, name: &str) -> Option<Value> {
        std::collections::HashMap::get(self, name).cloned()
    }
}

fn value(config: &dyn Config, name: &str) -> Value {
    config.get(name).unwrap_or(Value::Null)
}

fn text(config: &dyn Config, name: &str) -> String {
    value(config, name).as_str().unwrap_or_default().to_string()
}

/// Python's `config.X is True`.
fn is_true(config: &dyn Config, name: &str) -> bool {
    value(config, name) == Value::Bool(true)
}

fn number(value: &Value) -> f64 {
    value.as_f64().unwrap_or(0.0)
}

fn prefix(kind: Kind) -> &'static str {
    match kind {
        Kind::Mic => "MIC",
        Kind::Speaker => "SPEAKER",
    }
}

// ---- the device ---------------------------------------------------------------------------------------

/// The device the settings select (`_resolve_device` with no override): None when none is chosen or the
/// chosen one is not plugged in.
pub fn selected_device<'a>(kind: Kind, config: &dyn Config, devices: &'a DeviceList) -> Option<&'a Device> {
    match kind {
        Kind::Mic => {
            let name = text(config, "SELECTED_MIC_DEVICE");
            if name == NO_DEVICE {
                return None;
            }
            devices.resolve_mic(&name)
        }
        Kind::Speaker => {
            let name = text(config, "SELECTED_SPEAKER_DEVICE");
            if name == NO_DEVICE {
                return None;
            }
            devices.resolve_speaker(&name)
        }
    }
}

// ---- the recorder -------------------------------------------------------------------------------------

/// What `_create_recorder` builds. The numbers are the settings' own values, passed along unchanged.
#[derive(Debug, Clone, PartialEq)]
pub enum RecorderPlan {
    /// Silero decides where speech starts and ends.
    Vad { record_timeout: Value },
    /// The energy threshold does.
    Energy { energy_threshold: Value, dynamic_energy_threshold: Value, phrase_time_limit: Value, record_timeout: Value },
}

pub fn recorder_plan(kind: Kind, config: &dyn Config) -> RecorderPlan {
    let p = prefix(kind);
    let phrase_timeout = value(config, &format!("{p}_PHRASE_TIMEOUT"));
    let mut record_timeout = value(config, &format!("{p}_RECORD_TIMEOUT"));
    // A recording never needs to be longer than the pause that ends a phrase.
    if number(&record_timeout) > number(&phrase_timeout) {
        record_timeout = phrase_timeout;
    }
    if is_true(config, &format!("{p}_ENABLE_VAD")) {
        return RecorderPlan::Vad { record_timeout };
    }
    RecorderPlan::Energy {
        energy_threshold: value(config, &format!("{p}_THRESHOLD")),
        dynamic_energy_threshold: value(config, &format!("{p}_AUTOMATIC_THRESHOLD")),
        phrase_time_limit: record_timeout.clone(),
        record_timeout,
    }
}

/// The segmenter's settings for a session: Python's `VadSegmenter(max_speech_frames=...)` otherwise at its defaults.
pub fn vad_config(kind: Kind) -> VadConfig {
    VadConfig {
        // `max(1, round(7000 / FRAME_DURATION_MS))`, rounding halves to even like Python.
        max_speech_frames: Some(((MAX_SPEECH_MS / FRAME_MS).round_ties_even() as usize).max(1)),
        label: kind.as_str().to_string(),
        ..VadConfig::default()
    }
}

fn energy_params(plan: &RecorderPlan) -> Option<EnergyParams> {
    let RecorderPlan::Energy { energy_threshold, dynamic_energy_threshold, phrase_time_limit, record_timeout } = plan else {
        return None;
    };
    Some(EnergyParams {
        energy_threshold: number(energy_threshold),
        dynamic_energy_threshold: dynamic_energy_threshold.as_bool().unwrap_or(false),
        phrase_time_limit: number(phrase_time_limit),
        record_timeout: number(record_timeout),
    })
}

// ---- the engine ---------------------------------------------------------------------------------------

/// Which engine recognises, with what it is given (`AudioTranscriber.__init__`).
#[derive(Debug, Clone, PartialEq)]
pub enum EnginePlan {
    Google,
    Whisper { dir: PathBuf, device: String, device_index: i64, compute_type: String },
    /// Groq, OpenAI or a custom server: the same API with another address.
    OpenAiCompatible { engine: String, api_key: String, base_url: String, model: String },
    Deepgram { api_key: String, model: String, model_languages: Vec<String> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct TranscriberPlan {
    pub speaker: bool,
    pub phrase_timeout: Value,
    pub max_phrases: Value,
    pub segmented: bool,
    pub engine: EnginePlan,
}

/// The local Whisper model: whether its files are there, and how to run it.
pub trait WhisperLoader: Send + Sync {
    /// `checkWhisperWeight`: the model for `weight_type` is under `root` and can be loaded.
    fn available(&self, dir: &Path) -> bool;
    fn load(&self, dir: &Path, device: &str, device_index: i32, compute_type: &str) -> Result<Box<dyn Recognizer + Send>, String>;
}

pub fn whisper_dir(config: &dyn Config) -> PathBuf {
    Path::new(&text(config, "PATH_LOCAL")).join("weights").join("whisper").join(text(config, "WHISPER_WEIGHT_TYPE"))
}

fn api_key(config: &dyn Config, engine: &str) -> String {
    value(config, "TRANSCRIPTION_AUTH_KEYS").get(engine).and_then(Value::as_str).unwrap_or_default().to_string()
}

pub fn transcriber_plan(kind: Kind, config: &dyn Config, whisper: &dyn WhisperLoader) -> TranscriberPlan {
    let p = prefix(kind);
    let selected = text(config, "SELECTED_TRANSCRIPTION_ENGINE");
    let dir = whisper_dir(config);

    let engine = if selected == "Whisper" && whisper.available(&dir) {
        let compute = value(config, "SELECTED_TRANSCRIPTION_COMPUTE_DEVICE");
        EnginePlan::Whisper {
            dir,
            device: compute["device"].as_str().unwrap_or("cpu").to_string(),
            device_index: compute["device_index"].as_i64().unwrap_or(0),
            compute_type: text(config, "SELECTED_TRANSCRIPTION_COMPUTE_TYPE"),
        }
    } else if API_ENGINES.contains(&selected.as_str()) {
        let (base_url, model) = match selected.as_str() {
            "Groq_Whisper" => (GROQ_WHISPER_BASE_URL.to_string(), text(config, "SELECTED_GROQ_WHISPER_MODEL")),
            "OpenAI_Whisper" => (OPENAI_WHISPER_BASE_URL.to_string(), text(config, "SELECTED_OPENAI_WHISPER_MODEL")),
            _ => (text(config, "TRANSCRIPTION_CUSTOM_URL"), text(config, "SELECTED_CUSTOM_WHISPER_MODEL")),
        };
        EnginePlan::OpenAiCompatible { api_key: api_key(config, &selected), engine: selected, base_url, model }
    } else if selected == "Deepgram" {
        let model = text(config, "SELECTED_DEEPGRAM_MODEL");
        let languages = value(config, "DEEPGRAM_MODEL_LANGUAGES")
            .get(&model)
            .and_then(Value::as_array)
            .map(|codes| codes.iter().filter_map(|code| code.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        EnginePlan::Deepgram { api_key: api_key(config, "Deepgram"), model, model_languages: languages }
    } else {
        // Google, and Whisper without its files: the free endpoint, as in Python.
        EnginePlan::Google
    };

    TranscriberPlan {
        speaker: kind == Kind::Speaker,
        phrase_timeout: value(config, &format!("{p}_PHRASE_TIMEOUT")),
        max_phrases: value(config, &format!("{p}_MAX_PHRASES")),
        segmented: is_true(config, &format!("{p}_ENABLE_VAD")),
        engine,
    }
}

/// What one round of recognition asks (`MicSession._transcribe` / `SpeakerSession._transcribe`): the
/// languages switched on in the current tab, and the engine's thresholds. Read fresh every time.
pub fn ask(kind: Kind, config: &dyn Config) -> Ask {
    let p = prefix(kind);
    let slots_key = match kind {
        Kind::Mic => "SELECTED_YOUR_LANGUAGES",
        Kind::Speaker => "SELECTED_TARGET_LANGUAGES",
    };
    let tab = text(config, "SELECTED_TAB_NO");
    let slots = value(config, slots_key);
    let (mut languages, mut countries) = (Vec::new(), Vec::new());
    if let Some(slots) = slots.get(&tab).and_then(Value::as_object) {
        for slot in slots.values().filter(|slot| slot["enable"] == Value::Bool(true)) {
            languages.push(slot["language"].as_str().unwrap_or_default().to_string());
            countries.push(slot["country"].as_str().unwrap_or_default().to_string());
        }
    }
    Ask {
        languages,
        countries,
        avg_logprob: number(&value(config, &format!("{p}_AVG_LOGPROB"))),
        no_speech_prob: number(&value(config, &format!("{p}_NO_SPEECH_PROB"))),
        no_repeat_ngram_size: number(&value(config, &format!("{p}_NO_REPEAT_NGRAM_SIZE"))) as u32,
    }
}

// ---- building it ----------------------------------------------------------------------------------------

/// What the machine provides.
pub trait Platform: Send + Sync {
    /// The devices that are plugged in now.
    fn devices(&self) -> DeviceList;
    fn devices_for_host(&self, _host: &str) -> DeviceList { self.devices() }
    /// Opens `device` for the energy-threshold recorder.
    fn energy_recorder(&self, kind: Kind, device: &Device, params: EnergyParams) -> Result<Arc<dyn Recorder>, String>;
    /// Opens `device` for the Silero recorder.
    fn vad_recorder(&self, kind: Kind, device: &Device, config: VadConfig) -> Result<Arc<dyn Recorder>, String>;
    fn energy_recorder_on_host(&self, kind: Kind, _host: &str, device: &Device, params: EnergyParams) -> Result<Arc<dyn Recorder>, String> {
        self.energy_recorder(kind, device, params)
    }
    fn vad_recorder_on_host(&self, kind: Kind, _host: &str, device: &Device, config: VadConfig) -> Result<Arc<dyn Recorder>, String> {
        self.vad_recorder(kind, device, config)
    }
}

pub struct NativeBackend {
    config: Arc<dyn Config>,
    platform: Arc<dyn Platform>,
    whisper: Arc<dyn WhisperLoader>,
    /// Runs the cloud engines' requests.
    runtime: Handle,
}

impl NativeBackend {
    pub fn new(config: Arc<dyn Config>, platform: Arc<dyn Platform>, whisper: Arc<dyn WhisperLoader>, runtime: Handle) -> Self {
        NativeBackend { config, platform, whisper, runtime }
    }
}

/// How the audio is gathered into phrases for this transcriber.
pub fn phrase_settings(plan: &TranscriberPlan, format: Format) -> PhraseSettings {
    PhraseSettings {
        speaker: plan.speaker,
        format,
        phrase_timeout: number(&plan.phrase_timeout) as i64,
        max_phrases: number(&plan.max_phrases) as i64,
        engine: match plan.engine {
            EnginePlan::Google => Engine::Google,
            EnginePlan::Whisper { .. } => Engine::Whisper,
            EnginePlan::OpenAiCompatible { .. } | EnginePlan::Deepgram { .. } => Engine::Cloud,
        },
        segmented: plan.segmented,
    }
}

impl Backend for NativeBackend {
    fn selected_device(&self, kind: Kind) -> Option<Device> {
        let host = text(self.config.as_ref(), &format!("SELECTED_{}_HOST", prefix(kind)));
        selected_device(kind, self.config.as_ref(), &self.platform.devices_for_host(&host)).cloned()
    }

    fn open_recorder(&self, kind: Kind, device: &Device) -> Result<Arc<dyn Recorder>, String> {
        let plan = recorder_plan(kind, self.config.as_ref());
        let host = text(self.config.as_ref(), &format!("SELECTED_{}_HOST", prefix(kind)));
        match energy_params(&plan) {
            Some(params) => self.platform.energy_recorder_on_host(kind, &host, device, params),
            None => self.platform.vad_recorder_on_host(kind, &host, device, vad_config(kind)),
        }
    }

    fn create_transcriber(&self, kind: Kind, format: Format) -> Result<Box<dyn Transcriber>, String> {
        let plan = transcriber_plan(kind, self.config.as_ref(), self.whisper.as_ref());
        let recognizer: Box<dyn Recognizer + Send> = match &plan.engine {
            EnginePlan::Google => Box::new(Blocking::new(GoogleProvider::new(), self.runtime.clone())),
            EnginePlan::OpenAiCompatible { engine, api_key, base_url, model } => {
                Box::new(Blocking::new(OpenAiCompatible::new(api_key, base_url, model, engine), self.runtime.clone()))
            }
            EnginePlan::Deepgram { api_key, model, model_languages } => {
                Box::new(Blocking::new(DeepgramProvider::new(api_key, model, model_languages.clone()), self.runtime.clone()))
            }
            EnginePlan::Whisper { dir, device, device_index, compute_type } => {
                self.whisper.load(dir, device, *device_index as i32, compute_type)?
            }
        };
        let phrases = PhraseTranscriber::new(phrase_settings(&plan, format));
        let (config, kind) = (Arc::clone(&self.config), kind);
        Ok(Box::new(EngineTranscriber::new(phrases, Some(recognizer), Box::new(move || ask(kind, config.as_ref())))))
    }
}

// ---- local Whisper ------------------------------------------------------------------------------------

/// Whisper from the model files on disk, when this build can run them (the `ct2` feature).
pub struct FileWhisper;

impl WhisperLoader for FileWhisper {
    fn available(&self, dir: &Path) -> bool {
        ["model.bin", "config.json", "tokenizer.json"].iter().all(|name| dir.join(name).is_file())
    }

    fn load(&self, dir: &Path, device: &str, device_index: i32, compute_type: &str) -> Result<Box<dyn Recognizer + Send>, String> {
        load_whisper(dir, device, device_index, compute_type)
    }
}

#[cfg(feature = "ct2")]
fn load_whisper(dir: &Path, device: &str, device_index: i32, compute_type: &str) -> Result<Box<dyn Recognizer + Send>, String> {
    use super::whisper::model::WhisperModel;
    use super::whisper::provider::LocalWhisper;
    let model = WhisperModel::load(dir, device, device_index, compute_type, 4)?;
    Ok(Box::new(LocalWhisper::new(model)))
}

/// Not an engine switch: audio the user chose to keep on this machine must not go to a web service because
/// this build cannot run the model.
#[cfg(not(feature = "ct2"))]
fn load_whisper(_: &Path, _: &str, _: i32, _: &str) -> Result<Box<dyn Recognizer + Send>, String> {
    Err("local Whisper is not part of this build".to_string())
}
