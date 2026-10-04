//! Mic/speaker sessions replay historical `_AudioDeviceSession` behavior.
//! `fixtures/session_golden.json`, frozen at `16cb286c`, records ordered logs
//! against scripted fakes. Rust replays the same recorder calls, queues,
//! deliveries, logs and session-state snapshots.
//! Provenance and native test commands: `fixtures/README.md`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use vrct_core::audio::devices::Device;
use vrct_core::transcription::failure::ErrorCode;
use vrct_core::transcription::phrases::{AsrFailure, Chunk, Format, Stamp, Transcript};
use vrct_core::transcription::recorder::{AudioQueue, DeviceError, EnergyQueue, RecordError, Recorder};
use vrct_core::transcription::session::{AudioSession, Backend, Kind, Level, Message, Transcriber};

// ---- the log both sides write -------------------------------------------------------------------------

#[derive(Default)]
struct Context {
    events: Mutex<Vec<Value>>,
    config: Value,
    selected: Mutex<Option<String>>,
    gate_closed: AtomicBool,
    released: (Mutex<bool>, Condvar),
    recorder: Mutex<Option<Arc<FakeRecorder>>>,
}

impl Context {
    fn log(&self, tag: &str, payload: Value) {
        self.events.lock().unwrap().push(json!([tag, payload]));
    }

    fn count(&self, tag: &str) -> usize {
        self.events.lock().unwrap().iter().filter(|event| event[0] == tag).count()
    }

    fn flag(&self, name: &str) -> bool {
        self.config[name].as_bool().unwrap_or(false)
    }

    fn millis(&self, name: &str) -> u64 {
        self.config[name].as_u64().unwrap_or(0)
    }
}

fn device(name: &str) -> Device {
    Device { name: name.to_string(), channels: 1, default_sample_rate: 16_000 }
}

// ---- the fakes (the same rules as the Python ones) ----------------------------------------------------

struct FakeRecorder {
    ctx: Arc<Context>,
    listening: AtomicBool,
    audio: Mutex<Option<AudioQueue>>,
    energy: Mutex<Option<EnergyQueue>>,
    errors: DeviceError,
}

impl Recorder for FakeRecorder {
    fn format(&self) -> Format {
        Format { sample_rate: 16_000, sample_width: 2, channels: 1 }
    }

    fn record_into(&self, audio: AudioQueue, energy: Option<EnergyQueue>) -> Result<(), RecordError> {
        let kind = match audio.capacity() {
            None => "discard".to_string(),
            Some(size) => format!("bounded:{size}"),
        };
        self.ctx.log("record_into", json!({"energy": energy.is_some(), "queue": kind}));
        *self.audio.lock().unwrap() = Some(audio);
        *self.energy.lock().unwrap() = energy;
        if self.ctx.flag("fail_record") {
            if self.ctx.flag("fail_record_info") {
                // The recorder knows why: the information stays, but nobody is waiting on an event.
                self.errors.set(ErrorCode::VadInferenceError, "vad", "mic", "RuntimeError");
                self.errors.clear();
            }
            return Err(RecordError("cannot record".into()));
        }
        if self.ctx.flag("info_at_record") {
            self.errors.set(ErrorCode::VadInferenceError, "vad", "mic", "RuntimeError");
            self.errors.clear();
        }
        self.listening.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn pause(&self) {
        self.ctx.log("pause", Value::Null);
    }

    fn resume(&self) {
        self.ctx.log("resume", Value::Null);
    }

    fn stop(&self) {
        self.ctx.log("stop", Value::Null);
        let hang = self.ctx.millis("stop_hang_ms");
        if hang > 0 {
            thread::sleep(Duration::from_millis(hang));
        }
    }

    fn is_listening(&self) -> bool {
        self.listening.load(Ordering::SeqCst)
    }

    fn device_error(&self) -> &DeviceError {
        &self.errors
    }
}

struct FakeTranscriber {
    ctx: Arc<Context>,
    pending: Vec<Transcript>,
    recognition_error: bool,
    /// After a batch the next recognition does not return until released (Rust-only scenarios).
    block_next: bool,
}

fn transcript(text: &str) -> Transcript {
    Transcript { text: text.to_string(), confidence: 0.9, language: Some("en".to_string()), asr_ms: None }
}

impl Transcriber for FakeTranscriber {
    fn transcribe(&mut self, queue: &AudioQueue) -> Result<bool, AsrFailure> {
        if self.ctx.gate_closed.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(10));
            return Ok(false);
        }
        if self.block_next {
            let (released, wake) = &self.ctx.released;
            let mut released = released.lock().unwrap();
            while !*released {
                released = wake.wait(released).unwrap();
            }
        }
        let mut popped = false;
        while let Some(chunk) = queue.try_pop() {
            popped = true;
            let text = String::from_utf8(chunk.data).unwrap();
            match text.as_str() {
                "<raise-pipeline>" => return Err(AsrFailure { source: "mic".into(), exception_type: "RuntimeError".into() }),
                "<raise-generic>" => panic!("boom"),
                "<batch>" => {
                    self.pending.extend(["b1", "b2", "b3"].map(transcript));
                    self.block_next = self.ctx.flag("block_after_batch");
                }
                "<hang>" => {
                    let (released, wake) = &self.ctx.released;
                    let mut released = released.lock().unwrap();
                    while !*released {
                        released = wake.wait(released).unwrap();
                    }
                }
                _ => {
                    self.pending.push(transcript(&text));
                    self.recognition_error = text.starts_with('!');
                }
            }
        }
        if !popped {
            thread::sleep(Duration::from_millis(10));
        }
        Ok(popped)
    }

    fn has_transcript(&self) -> bool {
        !self.pending.is_empty()
    }

    fn take_transcript(&mut self) -> Transcript {
        self.pending.remove(0)
    }

    fn recognition_error(&self) -> bool {
        self.recognition_error
    }
}

struct FakeBackend {
    ctx: Arc<Context>,
}

impl Backend for FakeBackend {
    fn selected_device(&self, _: Kind) -> Option<Device> {
        self.ctx.selected.lock().unwrap().as_deref().map(device)
    }

    fn open_recorder(&self, _: Kind, device: &Device) -> Result<Arc<dyn Recorder>, String> {
        if self.ctx.flag("fail_open") {
            self.ctx.log("open_failed", json!(device.name));
            return Err("cannot open".into());
        }
        self.ctx.log("open", json!(device.name));
        let recorder = Arc::new(FakeRecorder {
            ctx: Arc::clone(&self.ctx),
            listening: AtomicBool::new(false),
            audio: Mutex::new(None),
            energy: Mutex::new(None),
            errors: DeviceError::new(),
        });
        *self.ctx.recorder.lock().unwrap() = Some(Arc::clone(&recorder));
        Ok(recorder)
    }

    fn create_transcriber(&self, _: Kind, format: Format) -> Result<Box<dyn Transcriber>, String> {
        if self.ctx.flag("fail_transcriber") {
            self.ctx.log("transcriber_failed", Value::Null);
            return Err("no engine".into());
        }
        self.ctx.log("transcriber", json!({"sample_rate": format.sample_rate, "channels": format.channels}));
        Ok(Box::new(FakeTranscriber { ctx: Arc::clone(&self.ctx), pending: Vec::new(), recognition_error: false, block_next: false }))
    }
}

// ---- running a scenario ---------------------------------------------------------------------------------

fn wait_for(ctx: &Context, tag: &str, wanted: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while ctx.count(tag) < wanted {
        assert!(Instant::now() < deadline, "waiting for {wanted} x {tag}, have {}", ctx.count(tag));
        thread::sleep(Duration::from_millis(5));
    }
}

fn option_bool(step: &Value, key: &str) -> Option<bool> {
    step[key].as_bool()
}

fn run(scenario: &Value) -> Vec<Value> {
    let ctx = Arc::new(Context { config: scenario["config"].clone(), ..Context::default() });
    *ctx.selected.lock().unwrap() = ctx.config["selected"].as_str().map(str::to_string);
    let timeout = Duration::from_millis(ctx.config["stop_timeout_ms"].as_u64().unwrap_or(15_000));
    let session = AudioSession::new(Kind::Mic, Arc::new(FakeBackend { ctx: Arc::clone(&ctx) })).with_stop_timeout(timeout);

    let log_ctx = Arc::clone(&ctx);
    session.set_log(Arc::new(move |text| log_ctx.log("log", json!(text))));
    let message_ctx = Arc::clone(&ctx);
    session.set_message_callback(Some(Arc::new(move |message: Message| message_ctx.log("deliver", message.to_value()))));
    let level_ctx = Arc::clone(&ctx);
    session.set_level_callback(Arc::new(move |level| {
        level_ctx.log("level", match level {
            Level::Value(value) => json!(value),
            Level::NoDevice => json!(false),
        })
    }));

    for step in scenario["steps"].as_array().unwrap() {
        match step["op"].as_str().unwrap() {
            "reconfigure" => {
                let device = step["device"].as_str().map(device);
                if session.reconfigure(option_bool(step, "transcript"), option_bool(step, "energy"), device.as_ref()).is_err() {
                    ctx.log("raised", Value::Null);
                }
            }
            "pause" => session.pause(),
            "resume" => session.resume(),
            "select" => *ctx.selected.lock().unwrap() = step["name"].as_str().map(str::to_string),
            "audio" => {
                let recorder = ctx.recorder.lock().unwrap().clone().unwrap();
                let queue = recorder.audio.lock().unwrap().clone().unwrap();
                for text in step["chunks"].as_array().unwrap() {
                    queue.put_dropping_oldest(Chunk { data: text.as_str().unwrap().as_bytes().to_vec(), at: Stamp::now(), end: None });
                }
            }
            "energy" => {
                let recorder = ctx.recorder.lock().unwrap().clone().unwrap();
                let queue = recorder.energy.lock().unwrap().clone().unwrap();
                for value in step["values"].as_array().unwrap() {
                    queue.put_dropping_oldest(value.as_u64().unwrap() as u32);
                }
            }
            "device_error" => {
                let recorder = ctx.recorder.lock().unwrap().clone().unwrap();
                recorder.errors.set(ErrorCode::AudioReadError, "recording", "mic", "OSError");
            }
            "gate" => ctx.gate_closed.store(step["closed"].as_bool().unwrap(), Ordering::SeqCst),
            "release" => release(&ctx),
            "wait_for" => wait_for(&ctx, step["tag"].as_str().unwrap(), step["count"].as_u64().unwrap() as usize),
            "sleep" => thread::sleep(Duration::from_millis(step["ms"].as_u64().unwrap())),
            "snapshot" => ctx.log(
                "snapshot",
                json!({
                    "transcript": session.wants_transcript(),
                    "energy": session.wants_energy(),
                    "device": session.active_device().map(|device| device.name),
                }),
            ),
            other => panic!("unknown step {other}"),
        }
    }
    release(&ctx);
    let events = ctx.events.lock().unwrap().clone();
    events
}

fn release(ctx: &Context) {
    let (released, wake) = &ctx.released;
    *released.lock().unwrap() = true;
    wake.notify_all();
}

fn golden() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/session_golden.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn sessions_behave_as_the_python_ones_do() {
    let golden = golden();
    let scenarios = golden["scenarios"].as_array().unwrap();
    assert!(scenarios.len() >= 20);
    let mut failures = Vec::new();
    for scenario in scenarios {
        let name = scenario["name"].as_str().unwrap();
        let expected = scenario["events"].as_array().unwrap();
        let got = run(scenario);
        if got != *expected {
            let show = |events: &[Value]| events.iter().map(|event| event.to_string()).collect::<Vec<_>>().join("\n    ");
            failures.push(format!("{name}\n  expected:\n    {}\n  got:\n    {}", show(expected), show(&got)));
        }
    }
    assert!(failures.is_empty(), "{} scenario(s) differ:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn every_pending_result_goes_out_before_the_next_recognition_starts() {
    // The second recognition blocks, so a result that waited for it would never arrive.
    let scenario = json!({
        "config": {"selected": "Mic A", "block_after_batch": true},
        "steps": [
            {"op": "reconfigure", "transcript": true},
            {"op": "audio", "chunks": ["<batch>"]},
            {"op": "wait_for", "tag": "deliver", "count": 3},
        ],
    });
    let texts: Vec<Value> = run(&scenario).iter().filter(|event| event[0] == "deliver").map(|event| event[1]["text"].clone()).collect();
    assert_eq!(texts, [json!("b1"), json!("b2"), json!("b3")]);
}
