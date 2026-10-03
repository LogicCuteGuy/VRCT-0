//! What a mic/speaker session builds from the settings, against the real `MicSession` / `SpeakerSession` and
//! `AudioTranscriber` (`fixtures/regenerate_native_golden.py` lifts them out of the Python source, stubs only
//! what would open a device or load a model, and records the arguments). The planning functions get the same
//! settings and have to come to the same device, recorder, engine and per-round question.
//!
//! The rest checks that `NativeBackend` builds what the plans say, with a fake machine.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use vrct_core::audio::devices::{Device, DeviceList};
use vrct_core::audio::vad::VadConfig;
use vrct_core::transcription::native::{
    ask, phrase_settings, recorder_plan, selected_device, transcriber_plan, vad_config, Config, EnginePlan, NativeBackend, Platform, RecorderPlan,
    WhisperLoader,
};
use vrct_core::transcription::phrases::{Engine, Format, Recognition, RecognizeError, Recognizer, Request};
use vrct_core::transcription::recorder::{AudioQueue, DeviceError, EnergyParams, EnergyQueue, RecordError, Recorder};
use vrct_core::transcription::session::{Backend, Kind};

fn golden() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/native_golden.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn config_of(scenario: &Value) -> HashMap<String, Value> {
    scenario["config"].as_object().unwrap().iter().map(|(key, value)| (key.clone(), value.clone())).collect()
}

/// The devices Python's stub `device_manager` listed, without the `NoDevice` placeholders.
fn devices_of(scenario: &Value) -> DeviceList {
    let real = |list: &Value| -> Vec<Device> {
        list.as_array()
            .unwrap()
            .iter()
            .filter(|device| device["name"] != "NoDevice")
            .map(|device| Device { name: device["name"].as_str().unwrap().to_string(), channels: 2, default_sample_rate: 48_000 })
            .collect()
    };
    DeviceList {
        mics: scenario["devices"]["mics"].as_object().unwrap().values().flat_map(&real).collect(),
        speakers: real(&scenario["devices"]["speakers"]),
        ..DeviceList::default()
    }
}

/// Whether the model files are there, as the scenario says.
struct Files(bool);

impl WhisperLoader for Files {
    fn available(&self, _: &Path) -> bool {
        self.0
    }
    fn load(&self, _: &Path, _: &str, _: i32, _: &str) -> Result<Box<dyn Recognizer + Send>, String> {
        Err("not used".into())
    }
}

fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// The same description of a session's decisions as the generator writes.
fn describe(kind: Kind, config: &dyn Config, devices: &DeviceList, whisper: bool) -> Value {
    let name = match kind {
        Kind::Mic => "Mic",
        Kind::Speaker => "Speaker",
    };
    let device = selected_device(kind, config, devices).map(|device| device.name.clone());
    let mut out = json!({"device": device});

    if let Some(device) = &device {
        out["recorder"] = match recorder_plan(kind, config) {
            RecorderPlan::Vad { record_timeout } => json!({
                "class": format!("Selected{name}VadRecorder"),
                "kwargs": {"device": device, "record_timeout": record_timeout},
            }),
            RecorderPlan::Energy { energy_threshold, dynamic_energy_threshold, phrase_time_limit, record_timeout } => json!({
                "class": format!("Selected{name}EnergyAndAudioRecorder"),
                "kwargs": {
                    "device": device,
                    "energy_threshold": energy_threshold,
                    "dynamic_energy_threshold": dynamic_energy_threshold,
                    "phrase_time_limit": phrase_time_limit,
                    "record_timeout": record_timeout,
                },
            }),
        };
    }

    let plan = transcriber_plan(kind, config, &Files(whisper));
    let (engine, provider) = match &plan.engine {
        EnginePlan::Google => ("Google".to_string(), Value::Null),
        EnginePlan::Whisper { dir, device, device_index, compute_type } => (
            "Whisper".to_string(),
            json!({"kind": "whisper", "dir": slash(dir), "device": device, "device_index": device_index, "compute_type": compute_type}),
        ),
        EnginePlan::OpenAiCompatible { engine, api_key, base_url, model } => (
            engine.clone(),
            json!({"kind": "provider", "api_key": api_key, "base_url": base_url, "model": model, "engine_name": engine}),
        ),
        EnginePlan::Deepgram { api_key, model, model_languages } => (
            "Deepgram".to_string(),
            json!({"kind": "provider", "api_key": api_key, "model": model, "model_languages": model_languages}),
        ),
    };
    out["transcriber"] = json!({
        "engine": engine,
        "provider": provider,
        "speaker": plan.speaker,
        "phrase_timeout": plan.phrase_timeout,
        "max_phrases": plan.max_phrases,
        "vad_segmented": plan.segmented,
        "source": kind.as_str(),
    });

    let asked = ask(kind, config);
    out["ask"] = json!({
        "languages": asked.languages,
        "countries": asked.countries,
        "avg_logprob": asked.avg_logprob,
        "no_speech_prob": asked.no_speech_prob,
        "no_repeat_ngram_size": asked.no_repeat_ngram_size,
    });
    out
}

/// Python writes the Whisper directory with the separators of the platform it ran on.
fn normalise(mut value: Value) -> Value {
    if let Some(dir) = value.pointer_mut("/transcriber/provider/dir") {
        *dir = json!(dir.as_str().unwrap().replace('\\', "/"));
    }
    value
}

#[test]
fn the_plans_are_the_ones_the_python_sessions_make() {
    let golden = golden();
    let scenarios = golden["scenarios"].as_array().unwrap();
    assert!(scenarios.len() >= 25);
    let mut differing = Vec::new();
    for scenario in scenarios {
        let config = config_of(scenario);
        let devices = devices_of(scenario);
        let whisper = scenario["whisper_available"].as_bool().unwrap();
        for (kind, key) in [(Kind::Mic, "mic"), (Kind::Speaker, "speaker")] {
            let got = normalise(describe(kind, &config, &devices, whisper));
            let expected = normalise(scenario["expected"][key].clone());
            if got != expected {
                differing.push(format!("{} / {key}\n  expected {expected}\n  got      {got}", scenario["name"]));
            }
        }
    }
    assert!(differing.is_empty(), "{} plan(s) differ:\n{}", differing.len(), differing.join("\n"));
}

// ---- building from the plans ------------------------------------------------------------------------------

struct FakeRecorder {
    errors: DeviceError,
}

impl Recorder for FakeRecorder {
    fn format(&self) -> Format {
        Format { sample_rate: 48_000, sample_width: 2, channels: 2 }
    }
    fn record_into(&self, _: AudioQueue, _: Option<EnergyQueue>) -> Result<(), RecordError> {
        Ok(())
    }
    fn pause(&self) {}
    fn resume(&self) {}
    fn stop(&self) {}
    fn is_listening(&self) -> bool {
        true
    }
    fn device_error(&self) -> &DeviceError {
        &self.errors
    }
}

#[derive(Debug, PartialEq)]
enum Opened {
    Energy(Kind, String, EnergyParams),
    Vad(Kind, String, usize, String),
}

struct FakeMachine {
    devices: DeviceList,
    opened: Mutex<Vec<Opened>>,
    refuse: bool,
}

impl Platform for FakeMachine {
    fn devices(&self) -> DeviceList {
        self.devices.clone()
    }
    fn energy_recorder(&self, kind: Kind, device: &Device, params: EnergyParams) -> Result<Arc<dyn Recorder>, String> {
        if self.refuse {
            return Err("busy".into());
        }
        self.opened.lock().unwrap().push(Opened::Energy(kind, device.name.clone(), params));
        Ok(Arc::new(FakeRecorder { errors: DeviceError::new() }))
    }
    fn vad_recorder(&self, kind: Kind, device: &Device, config: VadConfig) -> Result<Arc<dyn Recorder>, String> {
        self.opened.lock().unwrap().push(Opened::Vad(kind, device.name.clone(), config.max_speech_frames.unwrap(), config.label));
        Ok(Arc::new(FakeRecorder { errors: DeviceError::new() }))
    }
}

/// Remembers what it was asked to load.
#[derive(Default)]
struct FakeWhisper {
    present: bool,
    loaded: Mutex<Vec<(String, String, i32, String)>>,
    fail: bool,
}

struct Silent;

impl Recognizer for Silent {
    fn recognize(&mut self, _: &Request<'_>) -> Result<Recognition, RecognizeError> {
        Err(RecognizeError::NoMatch)
    }
}

/// What the backend is given; the test keeps the other handle to look at what was loaded.
struct Loader(Arc<FakeWhisper>);

impl WhisperLoader for Loader {
    fn available(&self, _: &Path) -> bool {
        self.0.present
    }
    fn load(&self, dir: &Path, device: &str, device_index: i32, compute_type: &str) -> Result<Box<dyn Recognizer + Send>, String> {
        self.0.loaded.lock().unwrap().push((slash(dir), device.to_string(), device_index, compute_type.to_string()));
        if self.0.fail {
            return Err("cannot load".into());
        }
        Ok(Box::new(Silent))
    }
}

struct Rig {
    backend: NativeBackend,
    machine: Arc<FakeMachine>,
    whisper: Arc<FakeWhisper>,
    _runtime: tokio::runtime::Runtime,
}

fn rig(overrides: &[(&str, Value)], present: bool, fail: bool, refuse: bool) -> Rig {
    let golden = golden();
    let defaults = &golden["scenarios"][0];
    let mut config = config_of(defaults);
    for (key, value) in overrides {
        config.insert(key.to_string(), value.clone());
    }
    let machine = Arc::new(FakeMachine { devices: devices_of(defaults), opened: Mutex::new(Vec::new()), refuse });
    let whisper = Arc::new(FakeWhisper { present, fail, ..FakeWhisper::default() });
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
    let backend = NativeBackend::new(Arc::new(config), machine.clone(), Arc::new(Loader(Arc::clone(&whisper))), runtime.handle().clone());
    Rig { backend, machine, whisper, _runtime: runtime }
}

const FORMAT: Format = Format { sample_rate: 16_000, sample_width: 2, channels: 1 };

#[test]
fn the_device_comes_from_the_machine_and_the_settings() {
    let rig = rig(&[], false, false, false);
    assert_eq!(rig.backend.selected_device(Kind::Mic).unwrap().name, "Mic A");
    assert_eq!(rig.backend.selected_device(Kind::Speaker).unwrap().name, "Speaker A [Loopback]");
    let rig = self::rig(&[("SELECTED_MIC_DEVICE", json!("Mic B"))], false, false, false);
    assert_eq!(rig.backend.selected_device(Kind::Mic).unwrap().name, "Mic B");
    let rig = self::rig(&[("SELECTED_MIC_DEVICE", json!("NoDevice"))], false, false, false);
    assert!(rig.backend.selected_device(Kind::Mic).is_none());
}

#[test]
fn an_energy_recorder_gets_the_settings_numbers() {
    let rig = rig(&[("MIC_THRESHOLD", json!(1200)), ("MIC_AUTOMATIC_THRESHOLD", json!(false)), ("MIC_RECORD_TIMEOUT", json!(2))], false, false, false);
    let device = rig.backend.selected_device(Kind::Mic).unwrap();
    rig.backend.open_recorder(Kind::Mic, &device).unwrap();
    assert_eq!(
        *rig.machine.opened.lock().unwrap(),
        [Opened::Energy(
            Kind::Mic,
            "Mic A".to_string(),
            EnergyParams { energy_threshold: 1200.0, dynamic_energy_threshold: false, phrase_time_limit: 2.0, record_timeout: 2.0 }
        )]
    );
}

#[test]
fn a_vad_recorder_cuts_speech_after_seven_seconds() {
    let rig = rig(&[("SPEAKER_ENABLE_VAD", json!(true))], false, false, false);
    let device = rig.backend.selected_device(Kind::Speaker).unwrap();
    rig.backend.open_recorder(Kind::Speaker, &device).unwrap();
    // round(7000 ms / 32 ms) = 219 frames
    assert_eq!(*rig.machine.opened.lock().unwrap(), [Opened::Vad(Kind::Speaker, "Speaker A [Loopback]".to_string(), 219, "speaker".to_string())]);
    assert_eq!(vad_config(Kind::Mic).max_speech_frames, Some(219));
    assert_eq!(vad_config(Kind::Mic).label, "mic");
}

#[test]
fn a_device_that_cannot_be_opened_is_an_error() {
    let rig = rig(&[], false, false, true);
    let device = rig.backend.selected_device(Kind::Mic).unwrap();
    assert!(rig.backend.open_recorder(Kind::Mic, &device).is_err());
}

#[test]
fn the_engines_are_built_from_their_settings() {
    for engine in ["Google", "Groq_Whisper", "OpenAI_Whisper", "Custom_Whisper", "Deepgram", "Something_Else"] {
        let rig = rig(&[("SELECTED_TRANSCRIPTION_ENGINE", json!(engine))], false, false, false);
        let transcriber = rig.backend.create_transcriber(Kind::Mic, FORMAT).unwrap_or_else(|e| panic!("{engine}: {e}"));
        assert!(!transcriber.has_transcript());
        assert!(rig.whisper.loaded.lock().unwrap().is_empty(), "{engine} does not use the local model");
    }
}

#[test]
fn local_whisper_is_loaded_from_its_folder_with_the_chosen_device() {
    let rig = rig(
        &[
            ("SELECTED_TRANSCRIPTION_ENGINE", json!("Whisper")),
            ("SELECTED_TRANSCRIPTION_COMPUTE_DEVICE", json!({"device": "cpu", "device_index": 0})),
            ("SELECTED_TRANSCRIPTION_COMPUTE_TYPE", json!("int8")),
            ("WHISPER_WEIGHT_TYPE", json!("small")),
        ],
        true,
        false,
        false,
    );
    rig.backend.create_transcriber(Kind::Speaker, FORMAT).unwrap();
    assert_eq!(*rig.whisper.loaded.lock().unwrap(), [("C:/vrct/weights/whisper/small".to_string(), "cpu".to_string(), 0, "int8".to_string())]);
}

#[test]
fn a_model_that_will_not_load_is_a_failure_not_a_switch_to_the_web() {
    let rig = rig(&[("SELECTED_TRANSCRIPTION_ENGINE", json!("Whisper"))], true, true, false);
    assert!(rig.backend.create_transcriber(Kind::Mic, FORMAT).is_err());
}

#[test]
fn without_model_files_whisper_falls_back_to_google_like_python() {
    let rig = rig(&[("SELECTED_TRANSCRIPTION_ENGINE", json!("Whisper"))], false, false, false);
    assert!(rig.backend.create_transcriber(Kind::Mic, FORMAT).is_ok());
    assert!(rig.whisper.loaded.lock().unwrap().is_empty());
}

#[cfg(not(feature = "ct2"))]
#[test]
fn a_build_without_the_local_model_refuses_it_instead_of_sending_audio_to_google() {
    use vrct_core::transcription::native::FileWhisper;
    let dir = std::env::temp_dir().join(format!("vrct-native-{}", std::process::id()));
    let model = dir.join("weights/whisper/base");
    std::fs::create_dir_all(&model).unwrap();
    for file in ["model.bin", "config.json", "tokenizer.json"] {
        std::fs::write(model.join(file), b"x").unwrap();
    }
    let golden = golden();
    let mut config = config_of(&golden["scenarios"][0]);
    config.insert("SELECTED_TRANSCRIPTION_ENGINE".into(), json!("Whisper"));
    config.insert("PATH_LOCAL".into(), json!(dir.to_string_lossy()));
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
    let machine = Arc::new(FakeMachine { devices: DeviceList::default(), opened: Mutex::new(Vec::new()), refuse: false });
    let backend = NativeBackend::new(Arc::new(config), machine, Arc::new(FileWhisper), runtime.handle().clone());
    let error = backend.create_transcriber(Kind::Mic, FORMAT).err().expect("the model files are there, and this build cannot run them");
    assert!(error.contains("not part of this build"), "{error}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn phrases_are_gathered_as_the_engine_and_the_settings_ask() {
    let golden = golden();
    let scenario = |name: &str| golden["scenarios"].as_array().unwrap().iter().find(|s| s["name"] == name).unwrap().clone();
    let settings = |scenario: &Value, kind: Kind, whisper: bool| {
        let config = config_of(scenario);
        phrase_settings(&transcriber_plan(kind, &config, &Files(whisper)), FORMAT)
    };

    let defaults = scenario("defaults");
    let mic = settings(&defaults, Kind::Mic, false);
    assert_eq!((mic.speaker, mic.format, mic.phrase_timeout, mic.max_phrases, mic.engine, mic.segmented), (false, FORMAT, 3, 10, Engine::Google, false));
    assert!(settings(&defaults, Kind::Speaker, false).speaker);

    // The engine decides how a phrase is sent (the Google endpoint gets the growing buffer again).
    let engine = |name: &str, whisper: bool| settings(&scenario(name), Kind::Mic, whisper).engine;
    assert_eq!(engine("whisper_files_present", true), Engine::Whisper);
    assert_eq!(engine("whisper_files_missing_falls_back_to_google", false), Engine::Google);
    assert_eq!(engine("groq", false), Engine::Cloud);
    assert_eq!(engine("custom_server", false), Engine::Cloud);
    assert_eq!(engine("deepgram", false), Engine::Cloud);

    let other = scenario("max_phrases_and_timeouts");
    let (mic, speaker) = (settings(&other, Kind::Mic, false), settings(&other, Kind::Speaker, false));
    assert_eq!((mic.phrase_timeout, mic.max_phrases, speaker.phrase_timeout, speaker.max_phrases), (5, 4, 7, 20));

    // Only the VAD recorder's phrases arrive already cut at natural boundaries.
    let vad = scenario("vad_on_mic_only");
    assert!(settings(&vad, Kind::Mic, false).segmented);
    assert!(!settings(&vad, Kind::Speaker, false).segmented);
}

#[test]
fn no_device_is_not_a_prefix_of_a_real_device() {
    // A saved name is matched by prefix (an MME name is cut short), so "NoDevice" must be ruled out first.
    let devices = DeviceList {
        mics: vec![Device { name: "NoDevice Pro".into(), channels: 1, default_sample_rate: 16_000 }],
        speakers: vec![Device { name: "NoDevice Speakers [Loopback]".into(), channels: 2, default_sample_rate: 48_000 }],
        ..DeviceList::default()
    };
    let config: HashMap<String, Value> =
        [("SELECTED_MIC_DEVICE", json!("NoDevice")), ("SELECTED_SPEAKER_DEVICE", json!("NoDevice"))].into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    assert!(selected_device(Kind::Mic, &config, &devices).is_none());
    assert!(selected_device(Kind::Speaker, &config, &devices).is_none());
}
