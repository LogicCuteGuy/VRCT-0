//! Audio capture as the Python sidecar sees it: RPC methods to start and stop a listening
//! session, and speech segments pushed back as lines on the sidecar's stdin.
//!
//! Python keeps the transcription (Whisper, cloud engines) for now and keeps its message
//! pipeline; what moves to Rust is everything up to the finished speech segment. A session
//! is a device, the normaliser and the VAD, named by the caller (`mic` or `speaker`), so
//! restarting one never touches the other.
//!
//! Lines written to Python (audio is raw 16 kHz mono PCM16, base64 inside the JSON):
//!   `/internal/audio/segment {session, segment_id, reason, audio}`
//!   `/internal/audio/event   {session, kind, message}` with kind `diagnostic` (a VAD log line),
//!   `engine_error` (a frame was dropped, listening goes on) or `capture_failed` (the device
//!   failed; the session is finished and Python should start it again).
//! Segments carry what the user says: these lines are never logged.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use serde_json::{json, Value};

use super::devices::DeviceList;
use super::vad::SpeechSegment;
use super::FRAME_SAMPLES;
use crate::protocol::sidecar_line;
use crate::rpc::LineWriter;

pub const SEGMENT: &str = "/internal/audio/segment";
pub const EVENT: &str = "/internal/audio/event";

/// The RPC methods this module serves.
pub const METHODS: &[&str] = &["audio.devices", "audio.start", "audio.stop"];

/// Python's own safety valve for speech that never pauses.
const DEFAULT_MAX_SPEECH_MS: u64 = 7000;
const FRAME_MS: f64 = FRAME_SAMPLES as f64 * 1000.0 / super::TARGET_SAMPLE_RATE as f64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    #[serde(rename = "mic")]
    Microphone,
    Speaker,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StartSpec {
    /// What Python calls this listener; the key for `audio.stop` and in every line pushed back.
    pub session: String,
    pub source: SourceKind,
    /// The device name as the UI shows it (see `audio.devices`); a name MME cut short still finds its device.
    pub device: String,
    #[serde(default = "default_max_speech_ms")]
    pub max_speech_ms: u64,
}

fn default_max_speech_ms() -> u64 {
    DEFAULT_MAX_SPEECH_MS
}

impl StartSpec {
    /// Python: `max(1, round(max_speech_ms / FRAME_DURATION_MS))`, rounding halves to even.
    pub fn max_speech_frames(&self) -> usize {
        ((self.max_speech_ms as f64 / FRAME_MS).round_ties_even() as usize).max(1)
    }
}

/// What a running session reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    Segment(SpeechSegment),
    Diagnostic(String),
    EngineError(String),
    CaptureFailed(String),
}

pub trait Session: Send {
    /// Stops the device and delivers what is still pending (the open segment, flushed) before returning.
    fn stop(self: Box<Self>);
}

/// How sessions are really made: the WASAPI + Silero one on Windows, a fake in tests.
pub trait Factory: Send + Sync {
    fn devices(&self) -> Result<DeviceList, String>;
    /// Starts listening; returns the session and the name of the device that was opened.
    fn start(&self, spec: &StartSpec, out: Arc<dyn Fn(Output) + Send + Sync>) -> Result<(Box<dyn Session>, String), String>;
}

pub struct AudioHost {
    factory: Arc<dyn Factory>,
    writer: Arc<dyn LineWriter>,
    sessions: Mutex<HashMap<String, Box<dyn Session>>>,
}

impl AudioHost {
    pub fn new(factory: Arc<dyn Factory>, writer: Arc<dyn LineWriter>) -> Self {
        Self { factory, writer, sessions: Mutex::new(HashMap::new()) }
    }

    pub fn devices(&self) -> Result<Value, String> {
        let list = self.factory.devices()?;
        let describe = |devices: &[super::devices::Device]| {
            devices
                .iter()
                .map(|d| json!({"name": d.name, "channels": d.channels, "sample_rate": d.default_sample_rate}))
                .collect::<Vec<_>>()
        };
        Ok(json!({
            "hosts": list.hosts(),
            "mics": describe(&list.mics),
            "default_mic": list.default_mic,
            "speakers": describe(&list.speakers),
            "default_speaker": list.default_speaker,
        }))
    }

    /// Starts a session, replacing (and stopping) one with the same name. Blocks while the old
    /// one stops and the device opens: call it from a blocking thread.
    pub fn start(&self, params: Value) -> Result<Value, String> {
        let spec: StartSpec = serde_json::from_value(params).map_err(|e| format!("bad params: {e}"))?;
        if spec.session.is_empty() {
            return Err("bad params: empty session name".to_string());
        }
        self.stop_session(&spec.session);
        let session_name = spec.session.clone();
        let writer = Arc::clone(&self.writer);
        let out: Arc<dyn Fn(Output) + Send + Sync> = Arc::new(move |output| push(writer.as_ref(), &session_name, output));
        let (session, device) = self.factory.start(&spec, out)?;
        self.sessions.lock().unwrap().insert(spec.session, session);
        Ok(json!({"device": device}))
    }

    /// Stops a session. False when none by that name was running.
    pub fn stop(&self, params: Value) -> Result<Value, String> {
        let name = params.get("session").and_then(Value::as_str).ok_or("bad params: missing session")?;
        Ok(Value::Bool(self.stop_session(name)))
    }

    /// Stops everything (the app is closing).
    pub fn stop_all(&self) {
        let names: Vec<String> = self.sessions.lock().unwrap().keys().cloned().collect();
        for name in names {
            self.stop_session(&name);
        }
    }

    fn stop_session(&self, name: &str) -> bool {
        // Out of the map before stopping, so a slow stop never holds the lock.
        let session = self.sessions.lock().unwrap().remove(name);
        match session {
            Some(session) => {
                session.stop();
                true
            }
            None => false,
        }
    }
}

fn push(writer: &dyn LineWriter, session: &str, output: Output) {
    let (endpoint, body) = match output {
        Output::Segment(segment) => (
            SEGMENT,
            json!({
                "session": session,
                "segment_id": segment.segment_id,
                "reason": segment.reason.as_str(),
                "audio": STANDARD.encode(&segment.audio),
            }),
        ),
        Output::Diagnostic(message) => (EVENT, json!({"session": session, "kind": "diagnostic", "message": message})),
        Output::EngineError(message) => (EVENT, json!({"session": session, "kind": "engine_error", "message": message})),
        Output::CaptureFailed(message) => (EVENT, json!({"session": session, "kind": "capture_failed", "message": message})),
    };
    let line = sidecar_line(endpoint, Some(&STANDARD.encode(body.to_string())));
    if let Err(error) = writer.write_line(&line) {
        // Never print the line itself: it carries the user's voice.
        eprintln!("[audio] cannot send to the sidecar: {error}");
    }
}

#[cfg(windows)]
pub use wasapi_factory::WasapiFactory;

#[cfg(windows)]
mod wasapi_factory {
    use std::path::PathBuf;

    use super::{Arc, DeviceList, Factory, Output, Session, SourceKind, StartSpec};
    use crate::audio::capture::Source;
    use crate::audio::pipeline::{CapturePipeline, Event};
    use crate::audio::silero::{OnnxRuntime, SileroFrameProbability};
    use crate::audio::vad::{SegmentIds, VadConfig, VadSegmenter};
    use crate::audio::wasapi::list_devices;

    /// Capture through WASAPI, speech detection with Silero on the ONNX Runtime found next to the app.
    pub struct WasapiFactory {
        library: PathBuf,
    }

    impl WasapiFactory {
        /// None when no ONNX Runtime library can be found: audio is then left to Python.
        pub fn locate() -> Option<Self> {
            OnnxRuntime::locate().map(|library| Self { library })
        }

        pub fn with_library(library: PathBuf) -> Self {
            Self { library }
        }
    }

    struct Running(CapturePipeline);

    impl Session for Running {
        fn stop(self: Box<Self>) {
            self.0.stop();
        }
    }

    impl Factory for WasapiFactory {
        fn devices(&self) -> Result<DeviceList, String> {
            list_devices()
        }

        fn start(&self, spec: &StartSpec, out: Arc<dyn Fn(Output) + Send + Sync>) -> Result<(Box<dyn Session>, String), String> {
            let devices = list_devices()?;
            let (device, source) = match spec.source {
                SourceKind::Microphone => (devices.resolve_mic(&spec.device), Source::Microphone),
                SourceKind::Speaker => (devices.resolve_speaker(&spec.device), Source::Speaker),
            };
            let device = device.ok_or_else(|| format!("no audio device {:?}", spec.device))?.name.clone();

            let label = match spec.source {
                SourceKind::Microphone => "mic",
                SourceKind::Speaker => "speaker",
            };
            let config = VadConfig {
                max_speech_frames: Some(spec.max_speech_frames()),
                label: label.to_string(),
                ..VadConfig::default()
            };
            let engine = SileroFrameProbability::with_library(&self.library)?;
            let mut segmenter = VadSegmenter::with_ids(engine, config, SegmentIds::global());
            let diagnostics = Arc::clone(&out);
            segmenter.on_diagnostic(move |line| diagnostics(Output::Diagnostic(line.to_string())));

            let events = Arc::clone(&out);
            let pipeline = CapturePipeline::start(source, &device, segmenter, move |event| {
                events(match event {
                    Event::Segment(segment) => Output::Segment(segment),
                    Event::EngineError(message) => Output::EngineError(message),
                    Event::CaptureFailed(message) => Output::CaptureFailed(message),
                })
            })?;
            Ok((Box::new(Running(pipeline)), device))
        }
    }
}
