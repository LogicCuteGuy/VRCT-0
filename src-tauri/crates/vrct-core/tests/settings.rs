//! Settings replay the historical Python `Config` descriptor/setter/load contract
//! in `fixtures/config_golden.json`, frozen at `16cb286c`; see `fixtures/README.md`.
//! Rust-only cases additionally cover debounce, subscribers and atomic writes.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use vrct_core::settings::defaults::initial_state;
use vrct_core::settings::schema::{self, Rule, PROPS};
use vrct_core::settings::{config_text, Devices, Env, Paths, SetError, Settings};

fn golden() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config_golden.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// The devices the generator's stub `device_manager` reported.
struct StubDevices {
    mics: BTreeMap<String, Vec<String>>,
    speakers: Vec<String>,
}

impl Devices for StubDevices {
    fn mic_hosts(&self) -> Vec<String> {
        self.mics.keys().cloned().collect()
    }
    fn mic_device_names(&self, host: &str) -> Vec<String> {
        self.mics.get(host).cloned().unwrap_or_default()
    }
    fn speaker_device_names(&self) -> Vec<String> {
        self.speakers.clone()
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

fn env_in(dir: &Path, golden: &Value) -> Env {
    let e = &golden["env"];
    let mics = e["mic_devices"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(host, devices)| (host.clone(), devices.as_array().unwrap().iter().map(|d| d["name"].as_str().unwrap().to_string()).collect()))
        .collect();
    Env {
        version: golden["statics"]["VERSION"].as_str().unwrap().to_string(),
        paths: Paths::in_dir(dir),
        devices: Arc::new(StubDevices { mics, speakers: e["speaker_devices"].as_array().unwrap().iter().map(|d| d["name"].as_str().unwrap().to_string()).collect() }),
        compute_devices: e["compute_devices"].as_array().unwrap().clone(),
        transcription_languages: e["transcription_languages"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(language, countries)| (language.clone(), strings(countries)))
            .collect(),
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
    let dir = std::env::temp_dir().join(format!("vrct-settings-test-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Settings opened (and so loaded) in an empty directory.
fn open_empty(golden: &Value) -> (Settings, PathBuf) {
    let dir = scratch_dir();
    let (settings, report) = Settings::open_with(env_in(&dir, golden), Duration::from_millis(50));
    assert_eq!(report, None);
    (settings, dir)
}

// ---- the properties ----

#[test]
fn the_properties_and_their_order_are_pythons() {
    let golden = golden();
    let python: Vec<&str> = golden["props"].as_object().unwrap().keys().map(String::as_str).collect();
    let native_host = schema::find("SELECTED_SPEAKER_HOST").unwrap();
    assert!(native_host.persisted && matches!(native_host.rule, Rule::Validated(_)));
    let legacy_props: Vec<_> = PROPS.iter().filter(|p| p.name != "SELECTED_SPEAKER_HOST").collect();
    let mut ours: Vec<&str> = legacy_props.iter().map(|p| p.name).collect();
    ours.sort_unstable();
    let mut python_sorted = python.clone();
    python_sorted.sort_unstable();
    assert_eq!(ours, python_sorted, "same set of settings");

    let persisted: Vec<&str> = legacy_props.iter().filter(|p| p.persisted).map(|p| p.name).collect();
    let order: Vec<&str> = golden["order"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(persisted, order, "config.json key order");

    for prop in legacy_props {
        let py = &golden["props"][prop.name];
        assert_eq!(py["persisted"].as_bool().unwrap(), prop.persisted, "{} persisted", prop.name);
        match prop.rule {
            Rule::ReadOnly => assert_eq!(py["readonly"], true, "{} read only", prop.name),
            Rule::Managed(..) => {
                assert_eq!(py["kind"], "managed", "{}", prop.name);
                assert_eq!(py["readonly"], false, "{}", prop.name);
                assert_eq!(py["immediate_save"].as_bool().unwrap(), prop.immediate, "{} immediate", prop.name);
            }
            Rule::Validated(_) => {
                assert_eq!(py["kind"], "validated", "{}", prop.name);
                assert_eq!(py["immediate_save"].as_bool().unwrap(), prop.immediate, "{} immediate", prop.name);
            }
        }
    }
}

// ---- values pushed through the setters ----

#[test]
fn every_probe_has_the_outcome_python_gave_it() {
    let golden = golden();
    let dir = scratch_dir();
    let env = env_in(&dir, &golden);
    // What the generator restored before each probe: the defaults after a load, which is the
    // release channel following the version.
    let mut pristine = initial_state(&env);
    pristine.insert("SELECTED_RELEASE_CHANNEL".to_string(), Value::String(vrct_core::settings::channel_for_version(&env.version).to_string()));

    let mut probes = 0;
    let mut failures = Vec::new();
    for (name, py) in golden["props"].as_object().unwrap() {
        let Some(list) = py.get("probes").and_then(Value::as_array) else { continue };
        let prop = schema::find(name).unwrap();
        for probe in list {
            probes += 1;
            let input = &probe["input"];
            let ours = prop.check(input, &pristine, &env);
            let expected = &probe["outcome"];
            let same = match (&ours, expected.get("ok")) {
                (Ok(stored), Some(python)) => stored == python,
                (Err(_), None) => expected["error"] == "ConfigValidationError",
                _ => false,
            };
            if !same {
                failures.push(format!("{name} <- {input}: Python {expected}, Rust {ours:?}"));
            }
        }
    }
    assert!(probes > 6000, "{probes} probes");
    assert!(failures.is_empty(), "{} of {probes} probes differ:\n{}", failures.len(), failures.iter().take(15).cloned().collect::<Vec<_>>().join("\n"));
}

/// Several changes in a row, where a later one is checked against what an earlier one left.
#[test]
fn every_sequence_of_changes_ends_as_python_did() {
    let golden = golden();
    let dir = scratch_dir();
    let env = env_in(&dir, &golden);
    let mut pristine = initial_state(&env);
    pristine.insert("SELECTED_RELEASE_CHANNEL".to_string(), Value::String(vrct_core::settings::channel_for_version(&env.version).to_string()));

    let mut steps_checked = 0;
    for sequence in golden["sequences"].as_array().unwrap() {
        let title = sequence["title"].as_str().unwrap();
        let mut state = pristine.clone();
        let outcomes = sequence["outcomes"].as_array().unwrap();
        for (index, step) in sequence["steps"].as_array().unwrap().iter().enumerate() {
            steps_checked += 1;
            let (name, value) = (step[0].as_str().unwrap(), &step[1]);
            let prop = schema::find(name).unwrap();
            let expected = &outcomes[index];
            match (prop.check(value, &state, &env), expected.get("ok")) {
                (Ok(stored), Some(python)) => {
                    assert_eq!(&stored, python, "{title}, step {index}: {name} <- {value}");
                    state.insert(name.to_string(), stored);
                }
                (Err(_), None) => {}
                (ours, _) => panic!("{title}, step {index}: {name} <- {value}: Python {expected}, Rust {ours:?}"),
            }
        }
    }
    assert!(steps_checked > 50, "{steps_checked} steps");
}

#[test]
fn the_release_channel_follows_the_version_suffix() {
    for case in golden()["channels"].as_array().unwrap() {
        let version = case["version"].as_str().unwrap();
        assert_eq!(vrct_core::settings::channel_for_version(version), case["channel"].as_str().unwrap(), "{version:?}");
    }
}

// ---- defaults and load_config ----

#[test]
fn a_first_start_has_pythons_defaults_and_statics() {
    let golden = golden();
    let (settings, dir) = open_empty(&golden);
    for (name, python) in golden["defaults"].as_object().unwrap() {
        assert_eq!(settings.get(name).as_ref(), Some(python), "default of {name}");
    }
    let machine_specific = ["PATH_LOCAL", "PATH_CONFIG", "PATH_LOGS"];
    // Class constants and a Hugging Face download URL that other modules (or nothing) will own.
    let elsewhere = ["GROQ_WHISPER_BASE_URL", "OPENAI_WHISPER_BASE_URL", "SETUP_DOWNLOAD_URL_stable", "SETUP_DOWNLOAD_URL_beta"];
    for (name, python) in golden["statics"].as_object().unwrap() {
        if machine_specific.contains(&name.as_str()) || elsewhere.contains(&name.as_str()) {
            continue;
        }
        // VRCT-0 adds Thai to the frozen upstream language list.
        let expected = if name == "SELECTABLE_UI_LANGUAGE_LIST" {
            serde_json::json!(["en", "th", "ja", "ko", "zh-Hant", "zh-Hans"])
        } else { python.clone() };
        assert_eq!(settings.get(name).as_ref(), Some(&expected), "static {name}");
    }
    assert_eq!(settings.get_str("PATH_CONFIG").unwrap(), dir.join("config.json").to_string_lossy());
    assert!(dir.join("logs").is_dir(), "the logs directory is created");
}

#[test]
fn every_load_case_ends_as_python_did() {
    let golden = golden();
    for case in golden["loads"].as_array().unwrap() {
        let title = case["title"].as_str().unwrap();
        let dir = scratch_dir();
        if let Some(text) = case["file"].as_str() {
            std::fs::write(dir.join("config.json"), text).unwrap();
        }
        if let Some(marker) = case["marker"].as_str() {
            std::fs::write(dir.join("installer_language.txt"), format!("{marker}
")).unwrap();
        }
        let (settings, report) = Settings::open_with(env_in(&dir, &golden), Duration::from_millis(50));

        let mut snapshot: HashMap<String, Value> = settings.snapshot().into_iter().collect();
        // Speaker host selection is a new native setting, absent from the frozen Python fixture.
        assert_eq!(snapshot.remove("SELECTED_SPEAKER_HOST"), Some(Value::String("Windows WASAPI".into())));
        let python: HashMap<String, Value> = case["snapshot"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        assert_eq!(snapshot, python, "{title}: settings after loading");
        for (name, value) in case["runtime"].as_object().unwrap() {
            let expected = if name == "SELECTABLE_UI_LANGUAGE_LIST" {
                serde_json::json!(["en", "th", "ja", "ko", "zh-Hant", "zh-Hans"])
            } else { value.clone() };
            assert_eq!(settings.get(name).as_ref(), Some(&expected), "{title}: run-time {name}");
        }

        let written = std::fs::read_to_string(dir.join("config.json")).ok();
        let python_written = case["written"].as_str();
        // A file that could not be read as an object is reported and left exactly as it was.
        let broken = case["file"].as_str().is_some_and(|text| !text.is_empty() && !serde_json::from_str::<Value>(text).is_ok_and(|v| v.is_object()));
        assert_eq!(report.is_some(), broken, "{title}: load report {report:?}");
        if broken {
            assert_eq!(written.as_deref(), case["file"].as_str(), "{title}: left alone");
        } else {
            // Python's text, with the keys inside nested objects sorted (serde_json sorts them and
            // nothing reads them by position): byte for byte what Rust wrote.
            let legacy_written = written.as_ref().unwrap().lines().filter(|line| !line.contains("\"SELECTED_SPEAKER_HOST\""))
                .collect::<Vec<_>>().join("\n");
            assert_eq!(legacy_written, case["formatted"].as_str().unwrap(), "{title}: file text");
            let top_level = |text: &str| -> Vec<String> {
                text.lines().filter(|l| l.starts_with("    \"")).filter_map(|l| l[5..].split('"').next()).map(str::to_string).collect()
            };
            assert_eq!(top_level(&legacy_written), top_level(python_written.unwrap()), "{title}: key order");
        }
        assert_eq!(dir.join("installer_language.txt").exists(), case["marker_left"].as_bool().unwrap(), "{title}: marker file");
    }
}

#[test]
fn json_text_is_formatted_as_pythons_json_dump() {
    for sample in golden()["formats"].as_array().unwrap() {
        let input = sample["input"].as_object().unwrap();
        // The generator sorted nothing here: the samples have at most one key per object, or sorted keys.
        let entries: Vec<(String, Value)> = input.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        assert_eq!(config_text(&entries), sample["text"].as_str().unwrap(), "{}", sample["input"]);
    }
}

#[test]
fn revalidating_the_model_lists_matches_python() {
    let golden = golden();
    let (settings, _) = open_empty(&golden);
    let r = &golden["revalidate"];
    let names = [("openai", "OPENAI"), ("groq", "GROQ"), ("gemini", "GEMINI"), ("plamo", "PLAMO")];
    for (key, upper) in names {
        settings.set(&format!("SELECTABLE_{upper}_MODEL_LIST"), r["lists"][key].clone()).unwrap();
    }
    // The selected model is set the way Python's test did: past the check (the list is not populated yet).
    for (key, upper) in [("gemini", "GEMINI"), ("plamo", "PLAMO")] {
        let _ = settings.set(&format!("SELECTED_{upper}_MODEL"), r["before"][key].clone());
    }
    settings.set("SELECTABLE_OPENAI_MODEL_LIST", serde_json::json!([])).unwrap();
    settings.set("SELECTABLE_GROQ_MODEL_LIST", serde_json::json!([])).unwrap();
    settings.set("SELECTED_OPENAI_MODEL", r["before"]["openai"].clone()).unwrap();
    settings.set("SELECTED_GROQ_MODEL", r["before"]["groq"].clone()).unwrap();
    settings.set("SELECTABLE_OPENAI_MODEL_LIST", r["lists"]["openai"].clone()).unwrap();
    settings.set("SELECTABLE_GROQ_MODEL_LIST", r["lists"]["groq"].clone()).unwrap();
    settings.set("SELECTABLE_GEMINI_MODEL_LIST", serde_json::json!([])).unwrap();
    settings.revalidate_selected_models();
    for (key, upper) in names {
        assert_eq!(settings.get(&format!("SELECTED_{upper}_MODEL")).unwrap(), r["after"][key], "{key}");
    }
}

// ---- behaviour without a Python counterpart ----

#[test]
fn unknown_and_read_only_settings_are_refused() {
    let golden = golden();
    let (settings, _) = open_empty(&golden);
    assert_eq!(settings.set("NOT_A_SETTING", Value::Null), Err(SetError::UnknownProperty));
    assert_eq!(settings.set("VERSION", Value::String("9".into())), Err(SetError::ReadOnly));
    assert_eq!(settings.set("OSC_PORT", Value::String("9000".into())), Err(SetError::Invalid));
    assert_eq!(settings.get("OSC_PORT"), Some(Value::from(9000)), "a refused value changes nothing");
}

fn file_value(dir: &Path, key: &str) -> Option<Value> {
    let text = std::fs::read_to_string(dir.join("config.json")).ok()?;
    serde_json::from_str::<Value>(&text).ok()?.get(key).cloned()
}

fn wait_until(what: &str, condition: impl Fn() -> bool) {
    for _ in 0..200 {
        if condition() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}");
}

#[test]
fn a_change_is_written_after_the_debounce_and_a_burst_is_one_write() {
    let golden = golden();
    let dir = scratch_dir();
    let (settings, _) = Settings::open_with(env_in(&dir, &golden), Duration::from_millis(300));
    for port in 9001..9011 {
        settings.set("OSC_PORT", Value::from(port)).unwrap();
    }
    assert_eq!(file_value(&dir, "OSC_PORT"), Some(Value::from(9000)), "not written yet");
    wait_until("the debounced write", || file_value(&dir, "OSC_PORT") == Some(Value::from(9010)));
    assert!(!dir.join("config.json.tmp").exists(), "no temp file is left behind");
}

#[test]
fn an_immediate_setting_is_written_without_waiting() {
    let golden = golden();
    let dir = scratch_dir();
    let (settings, _) = Settings::open_with(env_in(&dir, &golden), Duration::from_secs(60));
    settings.set("MESSAGE_BOX_RATIO", Value::from(12.5)).unwrap();
    wait_until("the immediate write", || file_value(&dir, "MESSAGE_BOX_RATIO") == Some(Value::from(12.5)));
}

#[test]
fn dropping_settings_writes_a_pending_change() {
    let golden = golden();
    let dir = scratch_dir();
    {
        let (settings, _) = Settings::open_with(env_in(&dir, &golden), Duration::from_secs(60));
        settings.set("FONT_FAMILY", Value::from("Meiryo")).unwrap();
        assert_ne!(file_value(&dir, "FONT_FAMILY"), Some(Value::from("Meiryo")));
    }
    assert_eq!(file_value(&dir, "FONT_FAMILY"), Some(Value::from("Meiryo")));
    let (again, _) = Settings::open_with(env_in(&dir, &golden), Duration::from_millis(50));
    assert_eq!(again.get_str("FONT_FAMILY").as_deref(), Some("Meiryo"), "and it is read back");
}

#[test]
fn subscribers_hear_persisted_changes_with_the_stored_value() {
    let golden = golden();
    let (settings, _) = open_empty(&golden);
    let heard = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&heard);
    settings.subscribe(move |name, value| sink.lock().unwrap().push((name.to_string(), value.clone())));

    settings.set("MIC_WORD_FILTER", serde_json::json!(["a", "a", 1, "b"])).unwrap(); // normalised
    settings.set("ENABLE_TRANSLATION", Value::Bool(true)).unwrap(); // not persisted
    let _ = settings.set("OSC_PORT", Value::from("bad")); // refused
    settings.set("OSC_PORT", Value::from(9100)).unwrap();

    assert_eq!(
        *heard.lock().unwrap(),
        vec![("MIC_WORD_FILTER".to_string(), serde_json::json!(["a", "b"])), ("OSC_PORT".to_string(), Value::from(9100))]
    );
    assert_eq!(settings.get_bool("ENABLE_TRANSLATION"), Some(true), "run-time settings still change");
}

#[test]
fn run_time_state_is_never_written_to_the_file() {
    let golden = golden();
    let (settings, dir) = open_empty(&golden);
    settings.set("ENABLE_TRANSLATION", Value::Bool(true)).unwrap();
    settings.set("SELECTABLE_OPENAI_MODEL_LIST", serde_json::json!(["m"])).unwrap();
    settings.save_now().unwrap();
    let text = std::fs::read_to_string(dir.join("config.json")).unwrap();
    assert!(!text.contains("ENABLE_TRANSLATION") && !text.contains("SELECTABLE_") && !text.contains("\"VERSION\""));
}
