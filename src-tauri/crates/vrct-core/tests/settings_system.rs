//! The production `Env` (tables, devices, token), `Settings::adopt` and the replica that reads
//! through `Settings` while the sidecar still runs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use vrct_core::config::ConfigReplica;
use vrct_core::protocol::Response;
use vrct_core::settings::system::{compute_devices, production_env, random_token, SystemDevices};
use vrct_core::settings::tables;
use vrct_core::settings::{Devices, SetError, Settings};

fn golden_env() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config_golden.json");
    let golden: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    golden["env"].clone()
}

fn scratch_dir() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!("vrct-settings-system-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn open(dir: &PathBuf) -> Settings {
    let (settings, report) = Settings::open_with(production_env("3.5.1-beta.1", dir), Duration::from_millis(30));
    assert_eq!(report, None);
    settings
}

fn texts(value: &Value) -> Vec<String> {
    value.as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect()
}

#[test]
fn thai_ui_language_is_valid_persisted_and_restored_from_installer_marker() {
    let dir = scratch_dir();
    let settings = open(&dir);
    settings.set("UI_LANGUAGE", json!("th")).unwrap();
    settings.save_now().unwrap();
    assert_eq!(open(&dir).get_str("UI_LANGUAGE").as_deref(), Some("th"));
    assert!(settings.set("UI_LANGUAGE", json!("not-a-language")).is_err());
    assert_eq!(settings.get_str("UI_LANGUAGE").as_deref(), Some("th"));
    settings.set("UI_LANGUAGE", json!("en")).unwrap();
    settings.save_now().unwrap();
    std::fs::write(dir.join("installer_language.txt"), "th\n").unwrap();
    assert_eq!(open(&dir).get_str("UI_LANGUAGE").as_deref(), Some("th"));
}

fn config_file(dir: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(dir.join("config.json")).unwrap()).unwrap()
}

fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn the_tables_are_what_python_read_from_its_model_modules() {
    let env = golden_env();
    let as_vec = |table: &[&str]| table.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(as_vec(tables::CTRANSLATE2_WEIGHT_TYPES), texts(&env["ctranslate2_weight_types"]));
    assert_eq!(as_vec(tables::WHISPER_WEIGHT_TYPES), texts(&env["whisper_weight_types"]));
    assert_eq!(as_vec(tables::TRANSLATION_ENGINES), texts(&env["translation_engines"]));
    assert_eq!(as_vec(tables::TRANSCRIPTION_ENGINES), texts(&env["transcription_engines"]));
    assert_eq!(as_vec(tables::OCR_SOURCE_LANGUAGES), texts(&env["ocr_source_languages"]));

    let languages: Vec<(String, Vec<String>)> =
        tables::TRANSCRIPTION_LANGUAGES.iter().map(|(l, c)| (l.to_string(), as_vec(c))).collect();
    let golden: Vec<(String, Vec<String>)> =
        env["transcription_languages"].as_object().unwrap().iter().map(|(l, c)| (l.clone(), texts(c))).collect();
    assert_eq!(languages, golden, "languages, their countries and their order");
}

#[test]
fn production_env_carries_the_tables_and_the_folder() {
    let dir = scratch_dir();
    let env = production_env("3.5.1-beta.1", &dir);
    assert_eq!(env.version, "3.5.1-beta.1");
    assert_eq!(env.paths.config, dir.join("config.json"));
    assert_eq!(env.paths.logs, dir.join("logs"));
    assert_eq!(env.paths.local, dir);
    assert_eq!(env.translation_engines.len(), tables::TRANSLATION_ENGINES.len());
    assert_eq!(env.transcription_languages.len(), tables::TRANSCRIPTION_LANGUAGES.len());
    let japanese: &Vec<String> = &env.transcription_languages["Japanese"];
    assert!(japanese.iter().any(|country| country == "Japan"));
    let _: &BTreeMap<String, Vec<String>> = &env.transcription_languages;
}

#[test]
fn the_cpu_is_the_one_compute_device() {
    let devices = compute_devices();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0]["device"], "cpu");
    assert_eq!(devices[0]["device_index"], 0);
    assert_eq!(devices[0]["compute_types"][0], "auto");
    assert!(devices[0]["compute_types"].as_array().unwrap().iter().any(|t| t == "int8"));
}

#[test]
fn tokens_are_43_url_safe_characters_and_never_repeat() {
    let first = random_token().unwrap();
    let second = random_token().unwrap();
    assert_eq!(first.len(), 43);
    assert!(first.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'), "{first}");
    assert_ne!(first, second);
}

#[test]
fn a_first_start_gets_a_token_the_file_then_keeps() {
    let dir = scratch_dir();
    let token = {
        let settings = open(&dir);
        let token = settings.get_str("WEBSOCKET_AUTH_TOKEN").unwrap();
        assert_eq!(token.len(), 43);
        token
    };
    // The next start gets a new random one from `Env`, but the saved value wins.
    let settings = open(&dir);
    assert_eq!(settings.get_str("WEBSOCKET_AUTH_TOKEN"), Some(token));
}

#[test]
fn a_first_start_offers_the_cpu_and_picks_the_first_engine() {
    let dir = scratch_dir();
    let settings = open(&dir);
    let selected = settings.get("SELECTED_TRANSLATION_COMPUTE_DEVICE").unwrap();
    assert_eq!(selected["device"], "cpu");
    assert_eq!(settings.get("SELECTABLE_COMPUTE_DEVICE_LIST").unwrap(), Value::Array(compute_devices()));
    assert_eq!(settings.get_str("VERSION").as_deref(), Some("3.5.1-beta.1"));
    assert_eq!(settings.get_str("SELECTED_RELEASE_CHANNEL").as_deref(), Some("beta"));
}

#[test]
fn the_system_devices_answer_with_one_host_or_the_placeholder() {
    let devices = SystemDevices;
    let hosts = devices.mic_hosts();
    assert_eq!(hosts.len(), 1);
    assert!(hosts[0] == "Windows WASAPI" || hosts[0] == "NoHost", "{hosts:?}");
    assert!(!devices.mic_device_names(&hosts[0]).is_empty());
    // Any other host (a saved MME choice) has no devices, so it is not accepted.
    assert!(devices.mic_device_names("MME").is_empty());
    assert!(!devices.speaker_device_names().is_empty());
}

#[cfg(windows)]
#[test]
fn wasapi_enumeration_survives_sequential_thread_exit() {
    // Each caller exits before the next starts. A process-wide STA enumerator
    // becomes invalid after the first caller's COM apartment is destroyed.
    for _ in 0..16 {
        std::thread::spawn(|| {
            use cpal::traits::HostTrait;
            let host = cpal::default_host();
            let _ = host.devices().expect("enumerate WASAPI endpoints").count();
            let _ = host.default_input_device();
            let _ = host.default_output_device();
        }).join().expect("WASAPI enumeration thread");
    }
}

#[test]
fn adopt_takes_a_value_the_rules_would_refuse() {
    let dir = scratch_dir();
    let settings = open(&dir);
    // "MME" is no host of this machine, so `set` says no; the sidecar is still using it.
    assert_eq!(settings.set("SELECTED_MIC_HOST", json!("MME")), Err(SetError::Invalid));
    assert_eq!(settings.adopt("SELECTED_MIC_HOST", json!("MME")), Ok(()));
    assert_eq!(settings.get_str("SELECTED_MIC_HOST").as_deref(), Some("MME"));
    drop(settings);
    assert_eq!(config_file(&dir)["SELECTED_MIC_HOST"], "MME");
}

#[test]
fn adopt_refuses_what_is_not_a_saved_setting() {
    let dir = scratch_dir();
    let settings = open(&dir);
    assert_eq!(settings.adopt("NOT_A_SETTING", json!(1)), Err(SetError::UnknownProperty));
    assert_eq!(settings.adopt("VERSION", json!("9.9.9")), Err(SetError::ReadOnly));
    assert_eq!(settings.adopt("ENABLE_TRANSLATION", json!(true)), Err(SetError::ReadOnly));
    assert_eq!(settings.get_str("VERSION").as_deref(), Some("3.5.1-beta.1"));
    assert_eq!(settings.get_bool("ENABLE_TRANSLATION"), Some(false));
}

#[test]
fn adopt_writes_the_file_and_tells_subscribers_only_about_changes() {
    let dir = scratch_dir();
    let settings = open(&dir);
    let heard: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
    let sink = Arc::clone(&heard);
    settings.subscribe(move |name, value| sink.lock().unwrap().push((name.to_string(), value.clone())));

    let current = settings.get("UI_LANGUAGE").unwrap();
    settings.adopt("UI_LANGUAGE", current).unwrap();
    assert!(heard.lock().unwrap().is_empty(), "an unchanged value is not news");

    settings.adopt("UI_LANGUAGE", json!("ko")).unwrap();
    assert_eq!(*heard.lock().unwrap(), vec![("UI_LANGUAGE".to_string(), json!("ko"))]);
    wait_until("the debounced write", || config_file(&dir)["UI_LANGUAGE"] == "ko");
}

#[test]
fn an_adopted_change_is_written_after_the_sidecars_own_write() {
    let dir = scratch_dir();
    // The sidecar writes config.json two seconds after its own change; an adopted change is written
    // after that (the debounce plus a settle time), never at once, even for an immediate setting.
    let debounce = Duration::from_millis(300);
    let (settings, _) = Settings::open_with(production_env("3.5.1-beta.1", &dir), debounce);
    let adopted_at = Instant::now();
    settings.adopt("MESSAGE_BOX_RATIO", json!(33.5)).unwrap();
    std::thread::sleep(debounce + Duration::from_millis(200));
    assert_ne!(config_file(&dir)["MESSAGE_BOX_RATIO"], json!(33.5), "written before the sidecar's own write could have happened");
    wait_until("the write behind the sidecar's", || config_file(&dir)["MESSAGE_BOX_RATIO"] == json!(33.5));
    assert!(adopted_at.elapsed() >= debounce + vrct_core::settings::SIDECAR_SETTLE);
}

fn bridge(endpoint: &str, result: Value) -> Response {
    Response::new(200, endpoint, result)
}

#[test]
fn the_replica_reads_through_the_settings_and_adopts_what_the_sidecar_reports() {
    let dir = scratch_dir();
    let settings = Arc::new(open(&dir));
    let replica = ConfigReplica::over(Arc::clone(&settings));

    assert!(replica.contains("OSC_PORT"));
    assert_eq!(replica.get("OSC_PORT"), settings.get("OSC_PORT"));
    assert!(!replica.contains("NOT_A_SETTING"));

    let changed = bridge("/internal/config/changed", json!({"key": "OSC_PORT", "value": 9100}));
    assert!(replica.ingest(&changed));
    assert_eq!(settings.get("OSC_PORT"), Some(json!(9100)));
    assert_eq!(replica.get("OSC_PORT"), Some(json!(9100)));

    let snapshot = bridge(
        "/internal/config/snapshot",
        json!({"UI_LANGUAGE": "ja", "SELECTED_MIC_HOST": "MME", "VERSION": "x", "NOT_A_SETTING": 1}),
    );
    assert!(replica.ingest(&snapshot));
    assert_eq!(replica.get_str("UI_LANGUAGE").as_deref(), Some("ja"));
    assert_eq!(replica.get_str("SELECTED_MIC_HOST").as_deref(), Some("MME"));
    assert_eq!(replica.get_str("VERSION").as_deref(), Some("3.5.1-beta.1"), "a read-only value is not overwritten");

    assert!(!replica.ingest(&bridge("/run/something", json!(1))), "other lines are not bridge traffic");
}

#[test]
fn a_replica_without_settings_still_keeps_its_own_copy() {
    let replica = ConfigReplica::default();
    assert!(!replica.contains("OSC_PORT"));
    replica.ingest(&bridge("/internal/config/snapshot", json!({"OSC_PORT": 1})));
    assert_eq!(replica.get("OSC_PORT"), Some(json!(1)));
}
