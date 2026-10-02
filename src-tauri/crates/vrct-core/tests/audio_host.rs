//! `audio::host`: the `audio.*` RPC methods and the lines pushed to the sidecar, with a fake
//! factory standing in for WASAPI + Silero (those are tested on their own).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use vrct_core::audio::devices::{Device, DeviceList};
use vrct_core::audio::host::{AudioHost, Factory, Output, Session, StartSpec, EVENT, METHODS, SEGMENT};
use vrct_core::audio::vad::{SegmentEnd, SpeechSegment};
use vrct_core::protocol::parse_sidecar_line;
use vrct_core::rpc::{LineWriter, Rpc};

#[derive(Default)]
struct Lines(Mutex<Vec<String>>);

impl LineWriter for Lines {
    fn write_line(&self, line: &str) -> Result<(), String> {
        self.0.lock().unwrap().push(line.to_string());
        Ok(())
    }
}

impl Lines {
    /// (endpoint, decoded JSON body) of every line written so far.
    fn decoded(&self) -> Vec<(String, Value)> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|line| {
                let envelope: Value = serde_json::from_str(line.trim_end()).unwrap();
                let bytes = STANDARD.decode(envelope["data"].as_str().unwrap()).unwrap();
                (envelope["endpoint"].as_str().unwrap().to_string(), serde_json::from_slice(&bytes).unwrap())
            })
            .collect()
    }
}

type Out = Arc<dyn Fn(Output) + Send + Sync>;

/// Records what it was asked and lets a test push output as if the device produced it.
#[derive(Default)]
struct FakeFactory {
    started: Mutex<Vec<StartSpec>>,
    outputs: Mutex<Vec<Out>>,
    stopped: Arc<Mutex<Vec<String>>>,
    fail_with: Mutex<Option<String>>,
}

struct FakeSession {
    name: String,
    out: Arc<dyn Fn(Output) + Send + Sync>,
    stopped: Arc<Mutex<Vec<String>>>,
}

impl Session for FakeSession {
    fn stop(self: Box<Self>) {
        // A real session delivers its open segment as it stops.
        (self.out)(Output::Segment(SpeechSegment { audio: vec![9, 9], segment_id: 99, reason: SegmentEnd::Flush }));
        self.stopped.lock().unwrap().push(self.name.clone());
    }
}

impl Factory for FakeFactory {
    fn devices(&self) -> Result<DeviceList, String> {
        let device = |name: &str| Device { name: name.to_string(), channels: 2, default_sample_rate: 48000 };
        Ok(DeviceList {
            mics: vec![device("Microphone (A)")],
            default_mic: Some("Microphone (A)".into()),
            speakers: vec![device("Speakers (B) [Loopback]")],
            default_speaker: Some("Speakers (B) [Loopback]".into()),
        })
    }

    fn start(&self, spec: &StartSpec, out: Arc<dyn Fn(Output) + Send + Sync>) -> Result<(Box<dyn Session>, String), String> {
        if let Some(error) = self.fail_with.lock().unwrap().clone() {
            return Err(error);
        }
        self.started.lock().unwrap().push(spec.clone());
        self.outputs.lock().unwrap().push(Arc::clone(&out));
        let session = FakeSession { name: spec.session.clone(), out, stopped: Arc::clone(&self.stopped) };
        Ok((Box::new(session), format!("opened {}", spec.device)))
    }
}

fn host() -> (AudioHost, Arc<FakeFactory>, Arc<Lines>) {
    let factory = Arc::new(FakeFactory::default());
    let lines = Arc::new(Lines::default());
    (AudioHost::new(factory.clone(), lines.clone()), factory, lines)
}

fn start_params(session: &str) -> Value {
    json!({"session": session, "source": "mic", "device": "Microphone (A)"})
}

#[test]
fn start_returns_the_device_that_was_opened_and_passes_the_spec_on() {
    let (host, factory, _) = host();
    let answer = host.start(json!({"session": "speaker", "source": "speaker", "device": "Speakers (B)", "max_speech_ms": 5000})).unwrap();
    assert_eq!(answer, json!({"device": "opened Speakers (B)"}));
    let started = factory.started.lock().unwrap();
    assert_eq!(started[0].session, "speaker");
    assert_eq!(started[0].max_speech_frames(), 156); // 5000 / 32 = 156.25
}

#[test]
fn the_default_max_speech_is_pythons_seven_seconds() {
    let (host, factory, _) = host();
    host.start(start_params("mic")).unwrap();
    // round(7000 / 32) = round(218.75) = 219
    assert_eq!(factory.started.lock().unwrap()[0].max_speech_frames(), 219);
}

#[test]
fn halves_round_to_even_like_pythons_round_and_the_minimum_is_one_frame() {
    let spec = |ms: u64| StartSpec { session: "s".into(), source: vrct_core::audio::host::SourceKind::Microphone, device: "d".into(), max_speech_ms: ms };
    assert_eq!(spec(48).max_speech_frames(), 2); // 1.5 -> 2
    assert_eq!(spec(80).max_speech_frames(), 2); // 2.5 -> 2 (Python's round)
    assert_eq!(spec(0).max_speech_frames(), 1);
}

#[test]
fn bad_params_are_errors_not_panics() {
    let (host, _, _) = host();
    assert!(host.start(json!({"session": "mic"})).unwrap_err().starts_with("bad params"));
    assert!(host.start(json!({"session": "", "source": "mic", "device": "d"})).unwrap_err().contains("empty session"));
    assert!(host.start(json!({"session": "x", "source": "line-in", "device": "d"})).unwrap_err().starts_with("bad params"));
    assert!(host.stop(json!({})).unwrap_err().contains("missing session"));
}

#[test]
fn a_factory_failure_is_the_calls_error_and_leaves_nothing_running() {
    let (host, factory, _) = host();
    *factory.fail_with.lock().unwrap() = Some("no audio device \"X\"".to_string());
    assert_eq!(host.start(start_params("mic")).unwrap_err(), "no audio device \"X\"");
    assert_eq!(host.stop(json!({"session": "mic"})).unwrap(), json!(false));
}

#[test]
fn segments_reach_the_sidecar_as_lines_with_the_session_name() {
    let (host, factory, lines) = host();
    host.start(start_params("mic")).unwrap();
    let out = factory.outputs.lock().unwrap()[0].clone();
    out(Output::Segment(SpeechSegment { audio: vec![1, 2, 3, 4], segment_id: 7, reason: SegmentEnd::MaxDuration }));
    out(Output::Diagnostic("[VAD][mic] speech_start".into()));
    out(Output::EngineError("model failed".into()));
    out(Output::CaptureFailed("device unplugged".into()));

    let decoded = lines.decoded();
    assert_eq!(decoded[0].0, SEGMENT);
    assert_eq!(
        decoded[0].1,
        json!({"session": "mic", "segment_id": 7, "reason": "max_duration", "audio": STANDARD.encode([1u8, 2, 3, 4])})
    );
    let kinds: Vec<(&str, &str)> = decoded[1..]
        .iter()
        .map(|(endpoint, body)| {
            assert_eq!(endpoint, EVENT);
            (body["kind"].as_str().unwrap(), body["message"].as_str().unwrap())
        })
        .collect();
    assert_eq!(kinds, vec![("diagnostic", "[VAD][mic] speech_start"), ("engine_error", "model failed"), ("capture_failed", "device unplugged")]);
    assert!(lines.0.lock().unwrap().iter().all(|line| line.ends_with('\n')), "one line each");
}

#[test]
fn every_reason_is_named_as_pythons_segment_reason() {
    let (host, factory, lines) = host();
    host.start(start_params("mic")).unwrap();
    let out = factory.outputs.lock().unwrap()[0].clone();
    for reason in [SegmentEnd::Silence, SegmentEnd::MaxDuration, SegmentEnd::Flush] {
        out(Output::Segment(SpeechSegment { audio: vec![], segment_id: 1, reason }));
    }
    let reasons: Vec<String> = lines.decoded().iter().map(|(_, body)| body["reason"].as_str().unwrap().to_string()).collect();
    assert_eq!(reasons, vec!["silence", "max_duration", "flush"]);
}

#[test]
fn stop_delivers_the_flushed_segment_before_it_answers() {
    let (host, factory, lines) = host();
    host.start(start_params("mic")).unwrap();
    assert_eq!(host.stop(json!({"session": "mic"})).unwrap(), json!(true));
    assert_eq!(factory.stopped.lock().unwrap().as_slice(), ["mic"]);
    assert_eq!(lines.decoded().len(), 1, "the flushed segment is already written");
    assert_eq!(host.stop(json!({"session": "mic"})).unwrap(), json!(false), "stopping twice is harmless");
}

#[test]
fn starting_a_session_again_stops_the_old_one_and_leaves_other_sessions_alone() {
    let (host, factory, _) = host();
    host.start(start_params("mic")).unwrap();
    host.start(json!({"session": "speaker", "source": "speaker", "device": "Speakers (B)"})).unwrap();
    host.start(start_params("mic")).unwrap();
    assert_eq!(factory.stopped.lock().unwrap().as_slice(), ["mic"]);
    host.stop_all();
    let mut stopped = factory.stopped.lock().unwrap().clone();
    stopped.sort();
    assert_eq!(stopped, ["mic", "mic", "speaker"]);
}

#[test]
fn devices_are_listed_for_the_ui() {
    let (host, _, _) = host();
    let devices = host.devices().unwrap();
    assert_eq!(devices["hosts"], json!(["Windows WASAPI"]));
    assert_eq!(devices["mics"], json!([{"name": "Microphone (A)", "channels": 2, "sample_rate": 48000}]));
    assert_eq!(devices["default_speaker"], "Speakers (B) [Loopback]");
}

#[tokio::test]
async fn the_methods_are_advertised_only_once_audio_is_added_and_answer_over_rpc() {
    let (host, factory, lines) = host();
    let rpc = Rpc::new(lines.clone() as Arc<dyn LineWriter>);
    for name in METHODS {
        assert!(!rpc.env_value().split(',').any(|m| m == *name), "{name} advertised without a host");
    }
    let rpc = rpc.with_audio(Arc::new(host));
    for name in METHODS {
        assert!(rpc.env_value().split(',').any(|m| m == *name), "{name} not advertised");
    }

    let request = |id: u64, method: &str, params: Value| {
        let text = json!({"status": 200, "endpoint": "/internal/rpc/request", "result": {"id": id, "method": method, "params": params}}).to_string();
        assert!(rpc.ingest(&parse_sidecar_line(&text).unwrap()));
    };
    request(1, "audio.start", start_params("mic"));
    for _ in 0..200 {
        if !lines.0.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(factory.started.lock().unwrap().len(), 1);
    let answers = lines.decoded();
    assert_eq!(answers[0].0, "/internal/rpc/response");
    assert_eq!(answers[0].1, json!({"id": 1, "ok": true, "result": {"device": "opened Microphone (A)"}}));
}

/// The real stack: WASAPI loopback of the default speaker, the real Silero model, through the host.
/// Needs `onnxruntime.dll` (ORT_DYLIB_PATH, or the Python package's); otherwise it says so and passes.
#[cfg(windows)]
#[test]
fn the_real_factory_runs_a_loopback_session_end_to_end() {
    use std::path::PathBuf;
    use std::process::Command;
    use vrct_core::audio::host::WasapiFactory;

    let library = std::env::var_os("ORT_DYLIB_PATH").map(PathBuf::from).or_else(|| {
        let script = "import onnxruntime, os; print(os.path.join(os.path.dirname(onnxruntime.__file__), 'capi', 'onnxruntime.dll'))";
        let out = Command::new("python").args(["-c", script]).output().ok()?;
        let path = PathBuf::from(String::from_utf8(out.stdout).ok()?.trim());
        path.is_file().then_some(path)
    });
    let Some(library) = library else {
        eprintln!("SKIPPED: no onnxruntime.dll (set ORT_DYLIB_PATH)");
        return;
    };
    let lines = Arc::new(Lines::default());
    let host = AudioHost::new(Arc::new(WasapiFactory::with_library(library)), lines.clone());

    let devices = host.devices().unwrap();
    let Some(speaker) = devices["default_speaker"].as_str() else {
        eprintln!("SKIPPED: no playback device");
        return;
    };
    let answer = host.start(json!({"session": "speaker", "source": "speaker", "device": speaker})).unwrap();
    assert_eq!(answer["device"], speaker);
    std::thread::sleep(Duration::from_millis(700));
    assert_eq!(host.stop(json!({"session": "speaker"})).unwrap(), json!(true));

    let decoded = lines.decoded();
    let problems: Vec<_> = decoded
        .iter()
        .filter(|(_, body)| matches!(body["kind"].as_str(), Some("engine_error" | "capture_failed")))
        .collect();
    assert!(problems.is_empty(), "{problems:?}");
    eprintln!("real loopback session wrote {} lines", decoded.len());
}

#[test]
fn each_line_names_the_session_it_came_from() {
    let (host, factory, lines) = host();
    host.start(start_params("mic")).unwrap();
    host.start(json!({"session": "speaker", "source": "speaker", "device": "Speakers (B)"})).unwrap();
    let outputs = factory.outputs.lock().unwrap().clone();
    outputs[1](Output::Diagnostic("from the speaker".into()));
    outputs[0](Output::Diagnostic("from the mic".into()));
    outputs[1](Output::Segment(SpeechSegment { audio: vec![], segment_id: 2, reason: SegmentEnd::Silence }));
    let seen: Vec<(String, String)> = lines
        .decoded()
        .iter()
        .map(|(_, body)| (body["session"].as_str().unwrap().to_string(), body["message"].as_str().unwrap_or("segment").to_string()))
        .collect();
    assert_eq!(
        seen,
        vec![
            ("speaker".to_string(), "from the speaker".to_string()),
            ("mic".to_string(), "from the mic".to_string()),
            ("speaker".to_string(), "segment".to_string()),
        ]
    );
}
