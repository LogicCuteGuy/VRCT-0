//! End-to-end wiring with no hardware or HTTP.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use vrct_core::audio::devices::Device;
use vrct_core::config::ConfigReplica;
use vrct_core::protocol::Response;
use vrct_core::router::{ResponseSink, Router};
use vrct_core::runtime::{self, Outputs, Runtime};
use vrct_core::settings::{system::production_env, Devices, Settings};
use vrct_core::transcription::phrases::{AsrFailure, Chunk, Format, Stamp, Transcript};
use vrct_core::transcription::recorder::{
    AudioQueue, DeviceError, EnergyQueue, RecordError, Recorder,
};
use vrct_core::transcription::session::{Backend, Kind, Transcriber};
use vrct_core::translation::llm;
use vrct_core::translation::native::{NativeTranslator, Remote};

#[derive(Default)]
struct Events(Mutex<Vec<Response>>);
impl ResponseSink for Events {
    fn emit(&self, response: Response) {
        self.0.lock().unwrap().push(response);
    }
}
impl Events {
    fn count(&self, endpoint: &str) -> usize {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.endpoint == endpoint)
            .count()
    }
    fn last(&self, endpoint: &str) -> Response {
        self.0
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|r| r.endpoint == endpoint)
            .unwrap()
            .clone()
    }
}
#[derive(Default)]
struct Out(Mutex<Vec<Value>>);
impl Outputs for Out {
    fn osc(&self, message: &str, notification: bool) -> Result<(), String> {
        self.0
            .lock()
            .unwrap()
            .push(json!(["osc", message, notification]));
        Ok(())
    }
    fn clipboard(&self, text: &str) -> Result<(), String> {
        self.0.lock().unwrap().push(json!(["clipboard", text]));
        Ok(())
    }
    fn websocket_alive(&self) -> bool {
        true
    }
    fn websocket_send(&self, message: Value) {
        self.0.lock().unwrap().push(json!(["ws", message]));
    }
    fn log_info(&self, text: &str) -> Result<(), String> {
        self.0.lock().unwrap().push(json!(["log", text]));
        Ok(())
    }
}
#[derive(Default)]
struct RemoteCalls(Mutex<Vec<llm::Request>>);
impl Remote for RemoteCalls {
    fn deepl(&self, key: &str, _: &str, _: &str, _: &str) -> Result<String, String> {
        Ok(format!("DeepL:{key}"))
    }
    fn llm(&self, request: llm::Request) -> Result<String, String> {
        let result = format!("{}:{}", request.model, request.text);
        self.0.lock().unwrap().push(request);
        Ok(result)
    }
}
struct FakeDevices;
impl Devices for FakeDevices {
    fn mic_hosts(&self) -> Vec<String> {
        vec!["Windows WASAPI".into()]
    }
    fn mic_device_names(&self, _: &str) -> Vec<String> {
        vec!["Mic A".into(), "Mic B".into()]
    }
    fn speaker_device_names(&self) -> Vec<String> {
        vec!["Speaker A [Loopback]".into()]
    }
    fn default_mic(&self) -> Option<(String, String)> {
        Some(("Windows WASAPI".into(), "Mic A".into()))
    }
    fn default_speaker(&self) -> Option<String> {
        Some("Speaker A [Loopback]".into())
    }
}
#[derive(Default)]
struct FakeRecorder {
    live: AtomicBool,
    errors: DeviceError,
    audio: Mutex<Option<AudioQueue>>,
    energy: Mutex<Option<EnergyQueue>>,
}
impl Recorder for FakeRecorder {
    fn format(&self) -> Format {
        Format {
            sample_rate: 16_000,
            sample_width: 2,
            channels: 1,
        }
    }
    fn record_into(
        &self,
        audio: AudioQueue,
        energy: Option<EnergyQueue>,
    ) -> Result<(), RecordError> {
        *self.audio.lock().unwrap() = Some(audio);
        *self.energy.lock().unwrap() = energy;
        self.live.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn pause(&self) {}
    fn resume(&self) {}
    fn stop(&self) {
        self.live.store(false, Ordering::SeqCst);
    }
    fn is_listening(&self) -> bool {
        self.live.load(Ordering::SeqCst)
    }
    fn device_error(&self) -> &DeviceError {
        &self.errors
    }
}
struct FakeTranscriber(VecDeque<Transcript>);
impl Transcriber for FakeTranscriber {
    fn transcribe(&mut self, queue: &AudioQueue) -> Result<bool, AsrFailure> {
        let Some(chunk) = queue.try_pop() else {
            return Ok(false);
        };
        let text = String::from_utf8(chunk.data).unwrap();
        if text == "<fail>" {
            return Err(AsrFailure {
                source: "mic".into(),
                exception_type: "RuntimeError".into(),
            });
        }
        self.0.push_back(Transcript {
            text,
            confidence: 0.9,
            language: Some("Japanese".into()),
            asr_ms: Some(12),
        });
        Ok(true)
    }
    fn has_transcript(&self) -> bool {
        !self.0.is_empty()
    }
    fn take_transcript(&mut self) -> Transcript {
        self.0.pop_front().unwrap()
    }
    fn recognition_error(&self) -> bool {
        false
    }
}
#[derive(Default)]
struct Machine {
    opens: Mutex<Vec<Arc<FakeRecorder>>>,
    missing: AtomicBool,
    fail_open: AtomicBool,
}
impl Backend for Machine {
    fn selected_device(&self, kind: Kind) -> Option<Device> {
        (!self.missing.load(Ordering::SeqCst)).then(|| Device {
            name: if kind == Kind::Mic {
                "Mic A"
            } else {
                "Speaker A [Loopback]"
            }
            .into(),
            channels: 1,
            default_sample_rate: 16_000,
        })
    }
    fn open_recorder(&self, _: Kind, _: &Device) -> Result<Arc<dyn Recorder>, String> {
        if self.fail_open.load(Ordering::SeqCst) {
            return Err("cannot open".into());
        }
        let recorder = Arc::new(FakeRecorder::default());
        self.opens.lock().unwrap().push(recorder.clone());
        Ok(recorder)
    }
    fn create_transcriber(&self, _: Kind, _: Format) -> Result<Box<dyn Transcriber>, String> {
        Ok(Box::new(FakeTranscriber(VecDeque::new())))
    }
}
impl Machine {
    fn recorder(&self) -> Arc<FakeRecorder> {
        self.opens.lock().unwrap().last().unwrap().clone()
    }
    fn count(&self) -> usize {
        self.opens.lock().unwrap().len()
    }
    fn feed(&self, text: &str) {
        self.recorder()
            .audio
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .put_dropping_oldest(Chunk {
                data: text.as_bytes().to_vec(),
                at: Stamp(1),
                end: None,
            });
    }
}
struct Rig {
    runtime: Arc<Runtime>,
    settings: Arc<Settings>,
    machine: Arc<Machine>,
    events: Arc<Events>,
    out: Arc<Out>,
    remote: Arc<RemoteCalls>,
    router: Arc<Router>,
}
impl Rig {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/runtime-tests")
            .join(format!(
                "{}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::SeqCst),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        let mut env = production_env("3.5.1-beta.1", directory);
        env.devices = Arc::new(FakeDevices);
        let (settings, error) = Settings::open(env);
        assert!(error.is_none());
        let settings = Arc::new(settings);
        for key in [
            "CONVERT_MESSAGE_TO_HIRAGANA",
            "CONVERT_MESSAGE_TO_ROMAJI",
            "OVERLAY_SMALL_LOG",
            "OVERLAY_LARGE_LOG",
            "VRC_MIC_MUTE_SYNC",
            "ENABLE_CLIPBOARD",
        ] {
            settings.set(key, json!(false)).unwrap();
        }
        let machine = Arc::new(Machine::default());
        let events = Arc::new(Events::default());
        let out = Arc::new(Out::default());
        let remote = Arc::new(RemoteCalls::default());
        let translator = Arc::new(NativeTranslator::new(remote.clone(), None));
        let runtime = Runtime::new(
            settings.clone(),
            machine.clone(),
            translator,
            out.clone(),
            events.clone(),
        );
        let router = Arc::new(runtime::register(Router::new(events.clone()), &runtime));
        Self {
            runtime,
            settings,
            machine,
            events,
            out,
            remote,
            router,
        }
    }
    async fn request(&self, endpoint: &str, payload: Option<Value>) -> Response {
        let before = self.events.count(endpoint);
        self.router.dispatch(
            endpoint.to_string(),
            payload.map(|v| STANDARD.encode(v.to_string())),
        );
        until(|| self.events.count(endpoint) > before).await;
        self.events.last(endpoint)
    }
    fn enable_llm(&self) {
        let mut keys = self.settings.get("AUTH_KEYS").unwrap();
        keys["OpenAI_API"] = json!("test-key");
        self.settings.set("AUTH_KEYS", keys).unwrap();
        self.settings
            .set(
                "SELECTABLE_OPENAI_MODEL_LIST",
                json!(["test-model", "second-model"]),
            )
            .unwrap();
        self.settings
            .set("SELECTED_OPENAI_MODEL", json!("test-model"))
            .unwrap();
        let mut engines = self.settings.get("SELECTED_TRANSLATION_ENGINES").unwrap();
        let tab = self.settings.get_str("SELECTED_TAB_NO").unwrap();
        engines[&tab] = json!("OpenAI_API");
        self.settings
            .set("SELECTED_TRANSLATION_ENGINES", engines)
            .unwrap();
        self.settings
            .set("ENABLE_TRANSLATION", json!(true))
            .unwrap();
    }
}
impl Drop for Rig {
    fn drop(&mut self) {
        self.runtime.shutdown();
    }
}
async fn until(test: impl Fn() -> bool) {
    let started = Instant::now();
    while !test() {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "condition did not become true"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn startup_and_bad_requests_never_open_a_device() {
    let rig = Rig::new();
    assert_eq!(rig.machine.count(), 0);
    assert_eq!(
        rig.request("/set/enable/transcription_send", None)
            .await
            .status,
        503
    );
    rig.runtime.activate();
    assert_eq!(
        rig.request("/run/send_message_box", Some(json!({})))
            .await
            .status,
        500
    );
    assert_eq!(
        rig.request(
            "/run/send_message_box",
            Some(json!({"id": "empty", "message": ""}))
        )
        .await
        .result["original"]["message"],
        ""
    );
    assert_eq!(rig.machine.count(), 0);
}

#[tokio::test]
async fn router_chat_runs_native_translation_and_outputs_with_live_credentials() {
    let rig = Rig::new();
    rig.runtime.activate();
    rig.enable_llm();
    rig.settings.set("LOGGER_FEATURE", json!(true)).unwrap();
    let response = rig
        .request(
            "/run/send_message_box",
            Some(json!({"id": "a", "message": "こんにちは"})),
        )
        .await;
    assert_eq!(response.status, 200);
    assert_eq!(
        response.result["translations"][0]["message"],
        "test-model:こんにちは"
    );
    rig.settings
        .set("SELECTED_OPENAI_MODEL", json!("second-model"))
        .unwrap();
    rig.request(
        "/run/send_message_box",
        Some(json!({"id": "b", "message": "ต่อ"})),
    )
    .await;
    let calls = rig.remote.0.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].api_key.as_deref(), Some("test-key"));
    assert_eq!(calls[1].model, "second-model");
    assert!(
        !calls[1].history.is_empty(),
        "pipeline and flow must share history"
    );
    drop(calls);
    assert!(rig.out.0.lock().unwrap().iter().any(|e| e[0] == "ws"));
    assert!(rig.out.0.lock().unwrap().iter().any(|e| e[0] == "log"));
    assert_eq!(rig.machine.count(), 0);
}

#[tokio::test]
async fn mic_session_feeds_pipeline_and_meter_union_survives_transcript_stop() {
    let rig = Rig::new();
    rig.runtime.activate();
    assert_eq!(
        rig.request("/set/enable/transcription_send", None)
            .await
            .result,
        true
    );
    assert_eq!(
        rig.request("/set/enable/transcription_send", None)
            .await
            .result,
        true
    );
    assert_eq!(rig.machine.count(), 1, "idempotent enable opens once");
    rig.machine.feed("mic phrase");
    until(|| rig.events.count("/run/transcription_send_mic_message") == 1).await;
    assert_eq!(
        rig.events
            .last("/run/transcription_send_mic_message")
            .result["original"]["message"],
        "mic phrase"
    );
    rig.request("/set/enable/check_mic_threshold", None).await;
    let recorder = rig.machine.recorder();
    recorder
        .energy
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .put_dropping_oldest(123);
    until(|| rig.events.count("/run/check_mic_volume") > 0).await;
    assert_eq!(rig.events.last("/run/check_mic_volume").result, 123);
    assert_eq!(
        rig.request("/set/disable/transcription_send", None)
            .await
            .result,
        false
    );
    assert!(rig.machine.recorder().live.load(Ordering::SeqCst));
    assert_eq!(
        rig.request("/set/disable/check_mic_threshold", None)
            .await
            .result,
        false
    );
    assert!(!rig.machine.recorder().live.load(Ordering::SeqCst));
}

#[tokio::test]
async fn missing_device_and_start_failure_reset_status_and_notify() {
    let rig = Rig::new();
    rig.runtime.activate();
    rig.machine.missing.store(true, Ordering::SeqCst);
    assert_eq!(
        rig.request("/set/enable/transcription_receive", None)
            .await
            .result,
        false
    );
    assert_eq!(rig.events.last("/run/error_device").status, 400);
    assert_eq!(rig.machine.count(), 0);
    rig.machine.missing.store(false, Ordering::SeqCst);
    rig.machine.fail_open.store(true, Ordering::SeqCst);
    assert_eq!(
        rig.request("/set/enable/transcription_send", None)
            .await
            .result,
        false
    );
    assert_eq!(
        rig.events
            .last("/run/transcription_recognition_error")
            .result["error_code"],
        "AUDIO_OPEN_ERROR"
    );
    assert_eq!(rig.events.count("/set/disable/transcription_send"), 1);
    assert_eq!(
        rig.settings.get_bool("ENABLE_TRANSCRIPTION_SEND"),
        Some(false)
    );
}

#[tokio::test]
async fn asynchronous_failure_disables_transcript_and_meter_once() {
    let rig = Rig::new();
    rig.runtime.activate();
    rig.request("/set/enable/transcription_send", None).await;
    rig.request("/set/enable/check_mic_threshold", None).await;
    rig.machine.feed("<fail>");
    until(|| rig.events.count("/run/transcription_recognition_error") == 1).await;
    assert_eq!(
        rig.settings.get_bool("ENABLE_TRANSCRIPTION_SEND"),
        Some(false)
    );
    assert_eq!(
        rig.settings.get_bool("ENABLE_CHECK_ENERGY_SEND"),
        Some(false)
    );
    assert_eq!(rig.events.count("/set/disable/transcription_send"), 1);
    assert_eq!(rig.events.count("/set/disable/check_mic_threshold"), 1);
    rig.request("/set/disable/transcription_send", None).await;
    assert_eq!(rig.events.count("/run/transcription_recognition_error"), 1);
}

#[tokio::test]
async fn adopted_settings_restart_active_session_and_inactive_settings_do_not_open() {
    let rig = Rig::new();
    rig.runtime.activate();
    rig.settings.set("MIC_THRESHOLD", json!(321)).unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(rig.machine.count(), 0);
    rig.request("/set/enable/transcription_send", None).await;
    let old = rig.machine.recorder();
    let before = rig.machine.count();
    rig.settings.adopt("MIC_THRESHOLD", json!(322)).unwrap();
    until(|| rig.machine.count() > before).await;
    assert!(!old.live.load(Ordering::SeqCst));
    assert_eq!(
        rig.settings.get_bool("ENABLE_TRANSCRIPTION_SEND"),
        Some(true)
    );
    rig.runtime.shutdown();
    let before = rig.machine.count();
    rig.settings.set("MIC_THRESHOLD", json!(323)).unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(rig.machine.count(), before);
    assert_eq!(
        rig.request("/set/enable/transcription_send", None)
            .await
            .status,
        503
    );
}

#[tokio::test]
async fn unported_features_are_rejected_before_capture_or_message_delivery() {
    let rig = Rig::new();
    rig.runtime.activate();
    rig.settings
        .set("CONVERT_MESSAGE_TO_ROMAJI", json!(true))
        .unwrap();
    assert_eq!(
        rig.request("/set/enable/transcription_send", None)
            .await
            .status,
        409
    );
    assert_eq!(
        rig.request(
            "/run/send_message_box",
            Some(json!({"id": 1, "message": "hello"}))
        )
        .await
        .status,
        409
    );
    assert_eq!(rig.machine.count(), 0);
    assert!(rig.out.0.lock().unwrap().is_empty());

    rig.settings
        .set("CONVERT_MESSAGE_TO_ROMAJI", json!(false))
        .unwrap();
    rig.settings
        .set("SELECTED_TRANSCRIPTION_ENGINE", json!("Deepgram"))
        .unwrap();
    assert_eq!(
        rig.request("/set/enable/transcription_send", None)
            .await
            .status,
        409,
        "unpublished Deepgram language metadata must not start capture"
    );
    assert_eq!(rig.machine.count(), 0);
    assert_eq!(
        rig.request("/set/enable/check_mic_threshold", None)
            .await
            .result,
        true,
        "a meter does not require transcription metadata"
    );
}

#[tokio::test]
async fn config_bridge_mirrors_runtime_translation_but_preserves_native_audio_flags() {
    let rig = Rig::new();
    let replica =
        ConfigReplica::over(rig.settings.clone()).with_host_owned(&["ENABLE_TRANSCRIPTION_SEND"]);
    rig.settings
        .set("ENABLE_TRANSCRIPTION_SEND", json!(true))
        .unwrap();
    replica.ingest(&Response::new(
        200,
        "/internal/config/snapshot",
        json!({"ENABLE_TRANSLATION": true, "ENABLE_TRANSCRIPTION_SEND": false}),
    ));
    assert_eq!(rig.settings.get_bool("ENABLE_TRANSLATION"), Some(true));
    assert_eq!(
        rig.settings.get_bool("ENABLE_TRANSCRIPTION_SEND"),
        Some(true)
    );
    replica.ingest(&Response::new(
        200,
        "/internal/config/changed",
        json!({"key": "ENABLE_TRANSLATION", "value": false}),
    ));
    assert_eq!(rig.settings.get_bool("ENABLE_TRANSLATION"), Some(false));
}

#[tokio::test]
async fn native_settings_drive_translation_and_model_catalog_without_capture() {
    let rig = Rig::new();
    rig.runtime.activate();
    rig.enable_llm();
    rig.settings
        .set("ENABLE_TRANSLATION", json!(false))
        .unwrap();
    let off = rig
        .request(
            "/run/send_message_box",
            Some(json!({"id":1,"message":"off"})),
        )
        .await;
    assert_eq!(off.result["translations"], json!([]));
    assert!(rig.remote.0.lock().unwrap().is_empty());
    rig.settings.set("ENABLE_TRANSLATION", json!(true)).unwrap();
    let on = rig
        .request(
            "/run/send_message_box",
            Some(json!({"id":2,"message":"on"})),
        )
        .await;
    assert_eq!(on.result["translations"][0]["message"], "test-model:on");
    rig.settings
        .set("SELECTABLE_OPENAI_MODEL_LIST", json!(["changed-model"]))
        .unwrap();
    assert_eq!(
        rig.settings.get("SELECTABLE_OPENAI_MODEL_LIST"),
        Some(json!(["changed-model"]))
    );
    assert_eq!(rig.machine.count(), 0);
}
#[tokio::test]
async fn credential_deletion_does_not_keep_an_authenticated_native_client() {
    let rig = Rig::new();
    rig.runtime.activate();
    rig.enable_llm();
    rig.request(
        "/run/send_message_box",
        Some(json!({"id": 1, "message": "first"})),
    )
    .await;
    let mut keys = rig.settings.get("AUTH_KEYS").unwrap();
    keys["OpenAI_API"] = Value::Null;
    rig.settings.set("AUTH_KEYS", keys).unwrap();
    let failed = rig
        .request(
            "/run/send_message_box",
            Some(json!({"id": 2, "message": "second"})),
        )
        .await;
    assert_eq!(failed.result["translations"][0]["message"], "second");
    assert_eq!(rig.remote.0.lock().unwrap().len(), 1);
    assert_eq!(rig.events.count("/run/error_translation_engine"), 1);
    let tab = rig.settings.get_str("SELECTED_TAB_NO").unwrap();
    assert_eq!(
        rig.settings.get("SELECTED_TRANSLATION_ENGINES").unwrap()[&tab],
        "CTranslate2"
    );
}

#[tokio::test]
async fn speaker_and_mic_lifecycles_are_independent_and_concurrent_enable_opens_once() {
    let rig = Rig::new();
    rig.runtime.activate();
    for _ in 0..10 {
        rig.router
            .dispatch("/set/enable/transcription_send".into(), None);
    }
    until(|| rig.events.count("/set/enable/transcription_send") == 10).await;
    assert_eq!(rig.machine.count(), 1);
    rig.request("/set/enable/transcription_receive", None).await;
    assert_eq!(rig.machine.count(), 2);
    let speaker = rig.machine.recorder();
    rig.machine.feed("speaker phrase");
    until(|| {
        rig.events
            .count("/run/transcription_receive_speaker_message")
            == 1
    })
    .await;
    assert_eq!(
        rig.events
            .last("/run/transcription_receive_speaker_message")
            .result["original"]["message"],
        "speaker phrase"
    );
    rig.request("/set/disable/transcription_send", None).await;
    assert!(speaker.live.load(Ordering::SeqCst));
    rig.request("/set/disable/transcription_receive", None)
        .await;
    assert!(!speaker.live.load(Ordering::SeqCst));
}

#[tokio::test]
async fn restart_failure_sends_one_disable_notification_per_feature() {
    let rig = Rig::new();
    rig.runtime.activate();
    rig.request("/set/enable/transcription_send", None).await;
    rig.request("/set/enable/check_mic_threshold", None).await;
    rig.machine.fail_open.store(true, Ordering::SeqCst);
    rig.settings.set("MIC_THRESHOLD", json!(456)).unwrap();
    until(|| rig.events.count("/run/transcription_recognition_error") == 1).await;
    assert_eq!(rig.events.count("/set/disable/transcription_send"), 1);
    assert_eq!(rig.events.count("/set/disable/check_mic_threshold"), 1);
    assert_eq!(
        rig.settings.get_bool("ENABLE_TRANSCRIPTION_SEND"),
        Some(false)
    );
    assert_eq!(
        rig.settings.get_bool("ENABLE_CHECK_ENERGY_SEND"),
        Some(false)
    );
}

#[tokio::test]
async fn output_settings_do_not_restart_capture_while_services_prepare() {
    let rig = Rig::new();
    rig.runtime.activate();
    rig.request("/set/enable/transcription_send", None).await;
    rig.request("/set/enable/check_mic_threshold", None).await;
    let old = rig.machine.recorder();
    let opens = rig.machine.count();
    for flag in [
        "CONVERT_MESSAGE_TO_ROMAJI",
        "OVERLAY_SMALL_LOG",
        "OVERLAY_LARGE_LOG",
    ] {
        rig.settings.set(flag, json!(true)).unwrap();
    }
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert!(old.live.load(Ordering::SeqCst));
    assert_eq!(rig.machine.count(), opens);
    assert_eq!(
        rig.settings.get_bool("ENABLE_TRANSCRIPTION_SEND"),
        Some(true)
    );
    assert_eq!(
        rig.settings.get_bool("ENABLE_CHECK_ENERGY_SEND"),
        Some(true)
    );
    for flag in [
        "CONVERT_MESSAGE_TO_ROMAJI",
        "OVERLAY_SMALL_LOG",
        "OVERLAY_LARGE_LOG",
    ] {
        rig.settings.set(flag, json!(false)).unwrap();
    }
    rig.machine.feed("still recording");
    until(|| rig.events.count("/run/transcription_send_mic_message") == 1).await;
    assert_eq!(rig.machine.count(), opens);
}

#[tokio::test]
async fn clipboard_bridge_toggle_reaches_mic_output_without_restarting_capture() {
    let rig = Rig::new();
    rig.runtime.activate();
    rig.enable_llm();
    let replica = ConfigReplica::over(rig.settings.clone());
    assert!(replica.ingest(&Response::new(
        200,
        "/internal/config/changed",
        json!({
            "key": "ENABLE_CLIPBOARD", "value": true
        })
    )));
    // Chat doesn't have clipboard output in the original pipeline, but enabling
    // the setting must no longer reject the chat endpoint.
    assert_eq!(
        rig.request(
            "/run/send_message_box",
            Some(json!({"id": "chat", "message": "chat"}))
        )
        .await
        .status,
        200
    );
    assert!(!rig
        .out
        .0
        .lock()
        .unwrap()
        .iter()
        .any(|event| event[0] == "clipboard"));
    assert_eq!(rig.machine.count(), 0);
    assert_eq!(
        rig.request("/set/enable/transcription_send", None)
            .await
            .result,
        true
    );
    rig.machine.feed("clipboard phrase");
    until(|| {
        rig.out
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|event| event[0] == "clipboard")
    })
    .await;
    let copied = rig
        .out
        .0
        .lock()
        .unwrap()
        .iter()
        .find(|event| event[0] == "clipboard")
        .unwrap()[1]
        .as_str()
        .unwrap()
        .to_string();
    assert!(copied.contains("clipboard phrase"));
    assert!(
        copied.contains("test-model"),
        "copy the formatted translation, not raw ASR text"
    );
    assert_eq!(rig.machine.count(), 1);
    replica.ingest(&Response::new(
        200,
        "/internal/config/changed",
        json!({
            "key": "ENABLE_CLIPBOARD", "value": false
        }),
    ));
    rig.machine.feed("second phrase");
    until(|| rig.events.count("/run/transcription_send_mic_message") == 2).await;
    assert_eq!(
        rig.out
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|event| event[0] == "clipboard")
            .count(),
        1
    );
    assert_eq!(
        rig.machine.count(),
        1,
        "output-only settings must not restart capture"
    );
}

#[tokio::test]
async fn speaker_transcripts_keep_the_original_no_clipboard_behavior() {
    let rig = Rig::new();
    rig.runtime.activate();
    rig.settings.set("ENABLE_CLIPBOARD", json!(true)).unwrap();
    assert_eq!(
        rig.request("/set/enable/transcription_receive", None)
            .await
            .result,
        true
    );
    rig.machine.feed("speaker clipboard phrase");
    until(|| {
        rig.events
            .count("/run/transcription_receive_speaker_message")
            == 1
    })
    .await;
    assert!(!rig
        .out
        .0
        .lock()
        .unwrap()
        .iter()
        .any(|event| event[0] == "clipboard"));
}
