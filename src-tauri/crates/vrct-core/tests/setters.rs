//! The `/set/data/*` handlers of `setters` against what the real controller answered
//! (`fixtures/setters_golden.json`, recorded once from the Python code): every payload, from the
//! states that matter to the endpoint, with the reply and every setting that differs afterwards.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::{json, Value};
use vrct_core::protocol::Response;
use vrct_core::router::{Fallback, ResponseSink, Router};
use vrct_core::settings::schema::PROPS;
use vrct_core::settings::{Devices, Env, Paths, Settings};
use vrct_core::setters;

fn fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// The devices the recorder's stub `device_manager` reported.
struct StubDevices;

impl Devices for StubDevices {
    fn mic_hosts(&self) -> Vec<String> {
        vec!["MME".to_string(), "Windows WASAPI".to_string()]
    }
    fn mic_device_names(&self, host: &str) -> Vec<String> {
        match host {
            "Windows WASAPI" => vec!["Mic A".to_string(), "Mic B".to_string()],
            "MME" => vec!["Mic M".to_string()],
            _ => Vec::new(),
        }
    }
    fn speaker_device_names(&self) -> Vec<String> {
        vec!["Spk A [Loopback]".to_string(), "Spk B [Loopback]".to_string()]
    }
    fn default_mic(&self) -> Option<(String, String)> {
        Some(("Windows WASAPI".to_string(), "Mic A".to_string()))
    }
    fn default_speaker(&self) -> Option<String> {
        Some("Spk A [Loopback]".to_string())
    }
}

fn strings(value: &Value) -> Vec<String> {
    value.as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect()
}

fn env_in(dir: &Path, config_golden: &Value) -> Env {
    let e = &config_golden["env"];
    Env {
        version: config_golden["statics"]["VERSION"].as_str().unwrap().to_string(),
        paths: Paths::in_dir(dir),
        devices: Arc::new(StubDevices),
        compute_devices: e["compute_devices"].as_array().unwrap().clone(),
        transcription_languages: e["transcription_languages"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(language, countries)| (language.clone(), strings(countries)))
            .collect::<BTreeMap<_, _>>(),
        ctranslate2_weight_types: strings(&e["ctranslate2_weight_types"]),
        whisper_weight_types: strings(&e["whisper_weight_types"]),
        translation_engines: strings(&e["translation_engines"]),
        transcription_engines: strings(&e["transcription_engines"]),
        ocr_source_languages: strings(&e["ocr_source_languages"]),
        websocket_token: e["token"].as_str().unwrap().to_string(),
    }
}

fn scratch_dir() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!("vrct-setters-test-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Settings in their start-up state, in a folder of their own. The debounce is so long that nothing is
/// written while a probe runs.
fn fresh(config_golden: &Value) -> Settings {
    let dir = scratch_dir();
    let (settings, report) = Settings::open_with(env_in(&dir, config_golden), Duration::from_secs(3600));
    assert_eq!(report, None);
    settings
}

fn apply_setup(settings: &Settings, steps: &Value) {
    for step in steps.as_array().unwrap() {
        let _ = settings.set(step[0].as_str().unwrap(), step[1].clone());
    }
}

fn state(settings: &Settings) -> BTreeMap<String, Value> {
    PROPS.iter().filter_map(|prop| settings.get(prop.name).map(|value| (prop.name.to_string(), value))).collect()
}

/// The settings whose value differs between the two states.
fn changed(before: &BTreeMap<String, Value>, after: &BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    after.iter().filter(|(name, value)| before.get(*name) != Some(*value)).map(|(n, v)| (n.clone(), v.clone())).collect()
}

fn reply_of(endpoint: &str, settings: &Settings, payload: &Value) -> Value {
    let (status, result) = setters::answer_for(endpoint, settings, payload.clone()).unwrap_or_else(|| panic!("{endpoint} is not ported"));
    json!({"status": status, "result": result})
}

fn golden_reply(reply: &Value) -> Value {
    json!({"status": reply["status"], "result": reply["result"]})
}

#[test]
fn every_endpoint_python_answers_as_a_pure_setter_is_ported() {
    let golden = fixture("setters_golden.json");
    let mut ported = setters::endpoints();
    let mut recorded: Vec<&str> = golden["endpoints"].as_array().unwrap().iter().map(|e| e["endpoint"].as_str().unwrap()).collect();
    ported.sort_unstable();
    recorded.sort_unstable();
    assert_eq!(ported, recorded);
    for native in setters::NATIVE {
        assert!(ported.contains(native), "{native}");
    }
}

#[test]
fn every_payload_gets_the_reply_and_the_state_python_gave() {
    let golden = fixture("setters_golden.json");
    let config_golden = fixture("config_golden.json");
    let mut probes = 0;
    for probe in golden["results"].as_array().unwrap() {
        let endpoint = probe["endpoint"].as_str().unwrap();
        let setup = probe["setup"].as_str().unwrap();
        let payload = &probe["payload"];
        let settings = fresh(&config_golden);
        apply_setup(&settings, &golden["setups"][setup]);
        let before = state(&settings);
        let reply = reply_of(endpoint, &settings, payload);
        let after = state(&settings);
        let context = format!("{endpoint} from `{setup}` <- {payload}");
        assert_eq!(reply, golden_reply(&probe["reply"]), "reply: {context}");
        let expected: BTreeMap<String, Value> = probe["changed"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        assert_eq!(changed(&before, &after), expected, "state: {context}");
        probes += 1;
    }
    assert!(probes > 3500, "{probes} probes");
}

#[test]
fn a_run_of_requests_leaves_what_python_left() {
    let golden = fixture("setters_golden.json");
    let config_golden = fixture("config_golden.json");
    for sequence in golden["sequences"].as_array().unwrap() {
        let title = sequence["title"].as_str().unwrap();
        let settings = fresh(&config_golden);
        apply_setup(&settings, &golden["setups"][sequence["setup"].as_str().unwrap()]);
        for (index, step) in sequence["steps"].as_array().unwrap().iter().enumerate() {
            let endpoint = step["endpoint"].as_str().unwrap();
            let before = state(&settings);
            let reply = reply_of(endpoint, &settings, &step["payload"]);
            let expected: BTreeMap<String, Value> = step["changed"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            let context = format!("{title}, step {index}: {endpoint} <- {}", step["payload"]);
            assert_eq!(reply, golden_reply(&step["reply"]), "reply: {context}");
            assert_eq!(changed(&before, &state(&settings)), expected, "state: {context}");
        }
    }
}

#[test]
fn the_error_texts_are_the_ones_errors_py_has() {
    let golden = fixture("setters_golden.json");
    let config_golden = fixture("config_golden.json");
    let settings = fresh(&config_golden);
    for (code, text) in golden["errors"].as_object().unwrap() {
        // Provoke each code through an endpoint that answers with it, as recorded.
        let probe = golden["results"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["reply"]["result"]["error_code"] == code.as_str())
            .unwrap_or_else(|| panic!("no probe ends in {code}"));
        let reply = reply_of(probe["endpoint"].as_str().unwrap(), &fresh(&config_golden), &probe["payload"]);
        assert_eq!(reply["result"]["message"], probe["reply"]["result"]["message"]);
        assert_eq!(reply["result"]["category"], text["category"], "{code}");
        assert_eq!(reply["result"]["severity"], text["severity"], "{code}");
    }
    let _ = settings;
}

// ---- the router ---------------------------------------------------------------------------------

#[derive(Default)]
struct Recorder {
    responses: Mutex<Vec<Response>>,
    forwarded: Mutex<Vec<String>>,
}

impl ResponseSink for Recorder {
    fn emit(&self, response: Response) {
        self.responses.lock().unwrap().push(response);
    }
}

impl Fallback for Recorder {
    fn forward(&self, endpoint: &str, _data: Option<&str>) -> Result<(), String> {
        self.forwarded.lock().unwrap().push(endpoint.to_string());
        Ok(())
    }
}

fn payload(value: &Value) -> Option<String> {
    Some(STANDARD.encode(value.to_string()))
}

#[tokio::test]
async fn the_router_answers_native_endpoints_and_leaves_the_rest_to_the_sidecar() {
    let config_golden = fixture("config_golden.json");
    let settings = Arc::new(fresh(&config_golden));
    let recorder = Arc::new(Recorder::default());
    let router = Router::new(recorder.clone()).with_fallback(recorder.clone());
    let router = Arc::new(setters::register(router, &settings));

    router.dispatch("/set/data/transparency".into(), payload(&json!("60")));
    router.dispatch("/set/data/mic_threshold".into(), payload(&json!(777)));
    for endpoint in setters::endpoints() {
        if endpoint != "/set/data/transparency" {
            router.dispatch(endpoint.into(), payload(&Value::Null));
        }
    }
    let native = setters::NATIVE.len();
    for _ in 0..300 {
        if recorder.responses.lock().unwrap().len() >= native {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let responses = recorder.responses.lock().unwrap();
    let answered: std::collections::BTreeSet<&str> = responses.iter().map(|r| r.endpoint.as_str()).collect();
    // Pinned by hand: an endpoint may only move here once nothing in the sidecar reads its setting.
    let expected: std::collections::BTreeSet<&str> = [
        "/set/data/release_channel",
        "/set/data/transparency",
        "/set/data/ui_scaling",
        "/set/data/textbox_ui_scaling",
        "/set/data/message_box_ratio",
        "/set/data/send_message_button_type",
        "/set/data/font_family",
        "/set/data/main_window_geometry",
        "/set/data/hotkeys",
    ]
    .into_iter()
    .collect();
    assert_eq!(answered, expected, "Rust answers exactly the native endpoints");
    let transparency = responses.iter().find(|r| r.endpoint == "/set/data/transparency").unwrap();
    assert_eq!((transparency.status, &transparency.result), (200, &json!(60)));
    assert_eq!(settings.get("TRANSPARENCY"), Some(json!(60)));

    let forwarded: std::collections::BTreeSet<String> = recorder.forwarded.lock().unwrap().iter().cloned().collect();
    let rest: std::collections::BTreeSet<String> =
        setters::endpoints().into_iter().filter(|e| !setters::NATIVE.contains(e)).map(str::to_string).collect();
    assert_eq!(forwarded, rest, "everything else goes to the sidecar");
    assert_ne!(settings.get("MIC_THRESHOLD"), Some(json!(777)), "a forwarded request changes nothing here");
}

#[tokio::test]
async fn a_payload_that_is_not_base64_json_is_a_400() {
    let config_golden = fixture("config_golden.json");
    let settings = Arc::new(fresh(&config_golden));
    let recorder = Arc::new(Recorder::default());
    let router = Arc::new(setters::register(Router::new(recorder.clone()), &settings));
    router.dispatch("/set/data/ui_scaling".into(), Some("not base64!".into()));
    for _ in 0..200 {
        if !recorder.responses.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(recorder.responses.lock().unwrap()[0].status, 400);
}
