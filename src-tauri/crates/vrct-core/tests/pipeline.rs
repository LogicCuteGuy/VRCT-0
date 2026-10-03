//! The message pipeline against what the real Python code did (`tests/fixtures/pipeline_golden.json`, made
//! by `regenerate_pipeline_golden.py`).
//!
//! * Scenarios: the real `Controller._processMessage` and its entry points, run against a scripted model,
//!   and the ordered log of everything they did. The same steps here must produce the same log.
//! * flashtext's keyword matching, `messageFormatter`, `detectVRAMError`, the per-direction table, the
//!   endpoints and the error texts, each against the Python value.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::NaiveDate;
use serde_json::{json, Value};

use vrct_core::pipeline::errors::{
    ErrorInfo, TRANSLATION_DISABLED_VRAM, TRANSLATION_ENGINE_LIMIT, TRANSLATION_VRAM_CHAT, TRANSLATION_VRAM_MIC, TRANSLATION_VRAM_SPEAKER,
};
use vrct_core::pipeline::format::{message_formatter, FormatType};
use vrct_core::pipeline::history::{isoformat, History, MAX_ITEMS};
use vrct_core::pipeline::keywords::KeywordFilter;
use vrct_core::pipeline::spec::{self, endpoints, Delivery, Direction, OwnTransliteration, Repeat, Spec};
use vrct_core::pipeline::{Host, LargeLog, Pipeline, PipelineError, SmallLog, TranslateError, Translated};
use vrct_core::transcription::native::Config;

fn golden() -> Value {
    serde_json::from_str(include_str!("fixtures/pipeline_golden.json")).expect("pipeline_golden.json is valid JSON")
}

fn strings(value: &Value) -> Vec<String> {
    value.as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect()
}

// ---- the world a scenario runs in ----------------------------------------------------------------------------

/// Python's fake `config` + `model` + `controller.run`: reads settings, answers like the scenario says and
/// logs every call in the golden's own event format.
struct World {
    config: Mutex<HashMap<String, Value>>,
    events: Mutex<Vec<Value>>,
    translate_script: Vec<Value>,
    translate_calls: Mutex<usize>,
    mute: Option<bool>,
    ws_alive: bool,
    overlay_ready: bool,
    /// How long the translator takes.
    translate_delay: std::time::Duration,
}

impl World {
    fn record(&self, event: Value) {
        self.events.lock().unwrap().push(event);
    }
}

impl Config for World {
    fn get(&self, name: &str) -> Option<Value> {
        self.config.lock().unwrap().get(name).cloned()
    }
}

/// `re.sub(r"\d+ms", "Nms", ...)`.
fn normalise_ms(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_digit() {
            let mut j = i;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            if chars[j..].starts_with(&['m', 's']) {
                out.push('N');
            } else {
                out.extend(&chars[i..j]);
            }
            i = j;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// `transliteration()` of the generator.
fn transliteration(message: &str, hiragana: bool, romaji: bool) -> Vec<Value> {
    if !hiragana && !romaji {
        return Vec::new();
    }
    let mut item = serde_json::Map::new();
    item.insert("orig".into(), json!(message));
    if hiragana {
        item.insert("hira".into(), json!(format!("h:{message}")));
    }
    if romaji {
        item.insert("hepburn".into(), json!(format!("r:{message}")));
    }
    vec![Value::Object(item)]
}

impl Host for World {
    fn run(&self, status: u16, endpoint: &str, payload: Value) {
        self.record(json!(["run", status, endpoint, payload]));
    }

    fn log(&self, text: &str) {
        self.record(json!(["print", normalise_ms(text)]));
    }

    fn translate(&self, direction: Direction, message: &str, source_language: Option<&str>) -> Result<Translated, TranslateError> {
        let method = if direction == Direction::Input { "input" } else { "output" };
        self.record(json!(["translate", method, message, source_language]));
        let index = {
            let mut calls = self.translate_calls.lock().unwrap();
            *calls += 1;
            *calls - 1
        };
        std::thread::sleep(self.translate_delay);
        let response = &self.translate_script[index];
        if let Some(raised) = response.get("raise") {
            let kind = raised[0].as_str().unwrap();
            let args = strings(&raised[1]);
            return Err(if kind == "ValueError" && args[0] == "VRAM_OUT_OF_MEMORY" {
                TranslateError::VramOutOfMemory(args.get(1).cloned())
            } else {
                TranslateError::Failed(args[0].clone())
            });
        }
        Ok(Translated {
            translation: strings(&response["ok"]),
            success: response["success"].as_array().unwrap().iter().map(|v| v.as_bool().unwrap()).collect(),
        })
    }

    fn transliterate(&self, message: &str, hiragana: bool, romaji: bool) -> Vec<Value> {
        self.record(json!(["transliterate", message, hiragana, romaji]));
        transliteration(message, hiragana, romaji)
    }

    fn send_osc(&self, message: &str) {
        self.record(json!(["osc", message]));
    }

    fn overlay_available(&self) -> bool {
        self.overlay_ready
    }

    fn overlay_small_log(&self, log: &SmallLog<'_>) {
        self.record(json!(["overlay_small", [log.message, log.language, log.translation, log.your_languages, log.transliteration_message, log.transliteration_translation]]),
        );
    }

    fn overlay_large_log(&self, log: &LargeLog<'_>) {
        self.record(json!([
                "overlay_large",
                [log.direction, log.message, log.language, log.translation, log.languages, log.transliteration_message, log.transliteration_translation]
            ]),
        );
    }

    fn set_clipboard(&self, text: &str) {
        self.record(json!(["clipboard", text]));
    }

    fn websocket_alive(&self) -> bool {
        self.ws_alive
    }

    fn websocket_send(&self, message: Value) {
        self.record(json!(["websocket", message]));
    }

    fn log_info(&self, text: &str) {
        self.record(json!(["logger", text]));
    }

    fn mic_mute_status(&self) -> Option<bool> {
        self.mute
    }

    fn set_setting(&self, name: &str, value: Value) {
        self.config.lock().unwrap().insert(name.to_string(), value.clone());
        self.record(json!(["config_set", name, value]));
    }

    fn fall_back_to_ctranslate2(&self) {
        self.record(json!(["fall_back_to_ctranslate2"]));
    }

    fn disable_translation(&self) {
        self.record(json!(["disable_translation"]));
    }
}

fn world_for(golden: &Value, scenario: &Value) -> (Arc<World>, Pipeline) {
    world_with_delay(golden, scenario, std::time::Duration::ZERO)
}

fn world_with_delay(golden: &Value, scenario: &Value, translate_delay: std::time::Duration) -> (Arc<World>, Pipeline) {
    let mut config: HashMap<String, Value> =
        golden["base_config"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    for (key, value) in scenario["config"].as_object().unwrap() {
        config.insert(key.clone(), value.clone());
    }
    let world = Arc::new(World {
        config: Mutex::new(config),
        events: Mutex::new(Vec::new()),
        translate_script: scenario["translate"].as_array().unwrap().clone(),
        translate_calls: Mutex::new(0),
        mute: scenario["mute"].as_bool(),
        ws_alive: scenario["ws_alive"].as_bool().unwrap(),
        overlay_ready: scenario["overlay"] == "ready",
        translate_delay,
    });
    let pipeline = Pipeline::new(world.clone(), world.clone());
    pipeline.set_word_filter(&strings(&scenario["word_filter"]));
    (world, pipeline)
}

/// Runs the scenario's calls and returns the events in the golden's format.
fn run(golden: &Value, scenario: &Value) -> Vec<Value> {
    let (world, pipeline) = world_for(golden, scenario);
    for step in scenario["steps"].as_array().unwrap() {
        let kind = step["call"].as_str().unwrap();
        let arg = &step["arg"];
        world.record(json!(["call", kind]));
        let outcome = match kind {
            "mic" => pipeline.mic_message(arg).map(|()| Value::Null),
            "speaker" => pipeline.speaker_message(arg).map(|()| Value::Null),
            "ocr" => pipeline.ocr_message(arg).map(|()| Value::Null),
            "chat" => pipeline.chat_message(arg),
            other => panic!("unknown call {other}"),
        };
        match outcome {
            Ok(value) => world.record(json!(["returned", value])),
            Err(PipelineError::Translate(message)) => world.record(json!(["raised", "translate", message])),
            Err(PipelineError::MissingTranslation(_)) => world.record(json!(["raised", "index", Value::Null])),
            Err(other) => panic!("{other}"),
        }
        let history: Vec<Value> = pipeline.history().iter().map(|item| json!([item["source"], item["text"]])).collect();
        for item in pipeline.history() {
            let stamp = item["timestamp"].as_str().unwrap();
            assert!(matches!(stamp.len(), 19 | 26) && stamp.as_bytes()[10] == b'T', "timestamp {stamp}");
        }
        world.record(json!(["history", history]));
    }
    let events = world.events.lock().unwrap().clone();
    events
}

#[test]
fn every_recorded_scenario_does_what_python_did() {
    let golden = golden();
    let scenarios = golden["scenarios"].as_array().unwrap();
    assert!(scenarios.len() > 150);
    let mut failures = Vec::new();
    for scenario in scenarios {
        let name = scenario["name"].as_str().unwrap();
        let expected = scenario["events"].as_array().unwrap();
        let actual = run(&golden, scenario);
        if &actual != expected {
            let at = actual.iter().zip(expected).position(|(a, e)| a != e).unwrap_or(actual.len().min(expected.len()));
            failures.push(format!(
                "{name}: event {at}\n  rust:   {}\n  python: {}",
                actual.get(at).map(Value::to_string).unwrap_or_else(|| "(none)".into()),
                expected.get(at).map(Value::to_string).unwrap_or_else(|| "(none)".into()),
            ));
        }
    }
    assert!(failures.is_empty(), "{} scenario(s) differ:\n{}", failures.len(), failures.join("\n"));
}

// ---- the pieces -----------------------------------------------------------------------------------------------

#[test]
fn the_word_filter_matches_like_flashtext() {
    let golden = golden();
    let cases = golden["keywords"].as_array().unwrap();
    assert!(cases.len() > 600);
    let mut failures = Vec::new();
    for case in cases {
        let mut filter = KeywordFilter::new();
        for keyword in strings(&case["keywords"]) {
            filter.add(&keyword);
        }
        let sentence = case["sentence"].as_str().unwrap();
        let expected = strings(&case["found"]);
        let found = filter.extract(sentence);
        if found != expected {
            failures.push(format!("{:?} in {sentence:?}: rust {found:?}, flashtext {expected:?}", case["keywords"]));
        }
        assert_eq!(filter.matches(sentence), !expected.is_empty());
    }
    assert!(failures.is_empty(), "{} case(s) differ:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn an_empty_filter_matches_nothing() {
    let mut filter = KeywordFilter::new();
    assert!(filter.is_empty() && !filter.matches("anything"));
    filter.add("");
    assert!(filter.is_empty(), "an empty keyword is ignored");
    filter.add("Word");
    assert!(!filter.is_empty() && filter.matches("a word here") && !filter.matches("wordy"));
}

#[test]
fn the_chatbox_text_is_formatted_like_message_formatter() {
    let golden = golden();
    let cases = golden["formatter"].as_array().unwrap();
    assert!(cases.len() > 250);
    let mut checked = 0;
    for case in cases {
        // Python raised for an unknown format name; here a name cannot be unknown.
        let Some(expected) = case["expected"].as_str() else { continue };
        let translation = strings(&case["translation"]);
        let text = message_formatter(&case["parts"], &translation, case["message"].as_str().unwrap());
        assert_eq!(text, expected, "{case}");
        checked += 1;
    }
    assert!(checked > 250);
    assert_eq!(FormatType::Send.setting(), "SEND_MESSAGE_FORMAT_PARTS");
    assert_eq!(FormatType::Received.setting(), "RECEIVED_MESSAGE_FORMAT_PARTS");
}

#[test]
fn out_of_memory_errors_are_recognised_like_detect_vram_error() {
    let golden = golden();
    for case in golden["vram"].as_array().unwrap() {
        let kind = case["error"][0].as_str().unwrap();
        let args = strings(&case["error"][1]);
        let error = if kind == "ValueError" && args[0] == "VRAM_OUT_OF_MEMORY" {
            TranslateError::VramOutOfMemory(args.get(1).cloned())
        } else {
            TranslateError::Failed(args[0].clone())
        };
        let expected = if case["result"][0] == true { Some(case["result"][1].as_str().unwrap().to_string()) } else { None };
        assert_eq!(error.vram_message(), expected, "{case}");
    }
}

fn error_code(info: &ErrorInfo) -> (&'static str, Value) {
    let (status, reply) = info.response(Value::Null);
    assert_eq!(status, 400);
    (info.code, reply)
}

#[test]
fn the_error_replies_are_the_ones_python_builds() {
    let golden = golden();
    for info in [&TRANSLATION_ENGINE_LIMIT, &TRANSLATION_VRAM_MIC, &TRANSLATION_VRAM_SPEAKER, &TRANSLATION_VRAM_CHAT, &TRANSLATION_DISABLED_VRAM] {
        let (code, reply) = error_code(info);
        let expected = &golden["errors"][code];
        assert_eq!(
            reply,
            json!({"error_code": code, "message": expected["message"], "data": null, "details": {}, "category": expected["category"], "severity": expected["severity"]})
        );
    }
}

#[test]
fn the_endpoints_are_the_ones_in_run_mapping() {
    let golden = golden();
    let mapping = &golden["run_mapping"];
    for (key, endpoint) in [
        ("word_filter", endpoints::WORD_FILTER),
        ("transcription_mic", endpoints::TRANSCRIPTION_MIC),
        ("transcription_speaker", endpoints::TRANSCRIPTION_SPEAKER),
        ("transcription_ocr", endpoints::TRANSCRIPTION_OCR),
        ("transcription_recognition_error", endpoints::RECOGNITION_ERROR),
        ("error_device", endpoints::ERROR_DEVICE),
        ("error_translation_engine", endpoints::ERROR_TRANSLATION_ENGINE),
        ("error_translation_mic_vram_overflow", endpoints::ERROR_TRANSLATION_MIC_VRAM),
        ("error_translation_speaker_vram_overflow", endpoints::ERROR_TRANSLATION_SPEAKER_VRAM),
        ("error_translation_chat_vram_overflow", endpoints::ERROR_TRANSLATION_CHAT_VRAM),
        ("enable_translation", endpoints::ENABLE_TRANSLATION),
        ("disable_transcription_send", endpoints::DISABLE_TRANSCRIPTION_SEND),
        ("disable_transcription_receive", endpoints::DISABLE_TRANSCRIPTION_RECEIVE),
    ] {
        assert_eq!(mapping[key], endpoint, "{key}");
    }
}

#[test]
fn the_direction_table_is_the_one_in_message_pipeline_py() {
    let golden = golden();
    let mapping = &golden["run_mapping"];
    for spec in [&spec::MIC, &spec::SPEAKER, &spec::OCR, &spec::CHAT] {
        let expected = &golden["specs"][spec.kind];
        check_spec(spec, expected, mapping);
    }
    assert_eq!(golden["specs"].as_object().unwrap().len(), 4);
}

fn check_spec(spec: &Spec, expected: &Value, mapping: &Value) {
    let name = spec.kind;
    assert_eq!(expected["kind"], name);
    assert_eq!(expected["has_word_filter"], spec.has_word_filter, "{name}");
    let repeat = match spec.repeat {
        Some(Repeat::Send) => json!("detectRepeatSendMessage"),
        Some(Repeat::Receive) => json!("detectRepeatReceiveMessage"),
        None => Value::Null,
    };
    assert_eq!(expected["repeat_detector_attr"], repeat, "{name}");
    let translate = if spec.translate == Direction::Input { "getInputTranslate" } else { "getOutputTranslate" };
    assert_eq!(expected["translate_attr"], translate, "{name}");
    assert_eq!(expected["multi_target"], spec.multi_target, "{name}");
    let own = if spec.own_transliteration == OwnTransliteration::YourLanguage { "your_language" } else { "detected_language" };
    assert_eq!(expected["own_transliteration_source"], own, "{name}");
    assert_eq!(expected["vram_error_code"], spec.vram_error.code, "{name}");
    assert_eq!(mapping[expected["vram_run_mapping_key"].as_str().unwrap()], spec.vram_endpoint, "{name}");
    assert_eq!(expected["feature_gate_attr"], json!(spec.feature_gate), "{name}");
    assert_eq!(expected["osc_send_gate_attr"], json!(spec.osc_gate), "{name}");
    let format = if spec.osc_format == FormatType::Send { "SEND" } else { "RECEIVED" };
    assert_eq!(expected["osc_format_type"], format, "{name}");
    assert_eq!(expected["overlay_direction"], spec.overlay_direction, "{name}");
    assert_eq!(expected["overlay_small_log"], spec.overlay_small_log, "{name}");
    assert_eq!(expected["clipboard"], spec.clipboard, "{name}");
    assert_eq!(expected["ws_type"], spec.ws_type, "{name}");
    assert_eq!(expected["ws_src_languages_attr"], spec.ws_src_languages, "{name}");
    assert_eq!(expected["ws_dst_languages_attr"], spec.ws_dst_languages, "{name}");
    assert_eq!(expected["logger_prefix"], spec.logger_prefix, "{name}");
    match spec.delivery {
        Delivery::Push(endpoint) => {
            assert_eq!(expected["delivery"], "push", "{name}");
            assert_eq!(mapping[expected["run_mapping_key"].as_str().unwrap()], endpoint, "{name}");
        }
        Delivery::Return => assert_eq!(expected["delivery"], "return", "{name}"),
    }
    assert_eq!(expected["payload_source"], json!(spec.payload_source), "{name}");
}

// ---- what only Rust has to get right ------------------------------------------------------------------------

#[test]
fn a_timestamp_is_isoformat() {
    let day = NaiveDate::from_ymd_opt(2026, 10, 3).unwrap();
    assert_eq!(isoformat(&day.and_hms_opt(1, 2, 3).unwrap()), "2026-10-03T01:02:03", "whole seconds have no fraction");
    assert_eq!(isoformat(&day.and_hms_micro_opt(1, 2, 3, 45).unwrap()), "2026-10-03T01:02:03.000045");
    assert_eq!(isoformat(&day.and_hms_micro_opt(23, 59, 59, 999_999).unwrap()), "2026-10-03T23:59:59.999999");
}

#[test]
fn the_history_keeps_the_newest_messages_stripped() {
    let mut history = History::default();
    history.add("chat", "  hi \n", || "t0".into());
    history.add("mic", "   ", || panic!("a blank message does not even ask for the time"));
    history.add("mic", "", || panic!("nor does an empty one"));
    assert_eq!(history.items().len(), 1);
    assert_eq!((history.items()[0].source.as_str(), history.items()[0].text.as_str()), ("chat", "hi"));
    for i in 0..MAX_ITEMS + 5 {
        history.add("speaker", &format!("m{i}"), || "t".into());
    }
    assert_eq!(history.items().len(), MAX_ITEMS);
    assert_eq!(history.items()[0].text, "m5");
    assert_eq!(history.items()[MAX_ITEMS - 1].text, format!("m{}", MAX_ITEMS + 4));
    assert_eq!(history.to_values()[0], json!({"source": "speaker", "text": "m5", "timestamp": "t"}));
    history.clear();
    assert!(history.items().is_empty());
}

fn quiet_world() -> (Arc<World>, Pipeline) {
    let golden = golden();
    let scenario = json!({"config": {}, "translate": [], "mute": null, "ws_alive": false, "overlay": "none", "word_filter": []});
    world_for(&golden, &scenario)
}

#[test]
fn a_request_missing_what_python_indexed_is_refused() {
    let (_, pipeline) = quiet_world();
    assert!(matches!(pipeline.chat_message(&json!({"message": "hi"})), Err(PipelineError::Malformed("id"))));
    assert!(matches!(pipeline.chat_message(&json!({"id": "x"})), Err(PipelineError::Malformed("message"))));
    assert!(matches!(pipeline.chat_message(&json!({"id": "x", "message": 5})), Err(PipelineError::Malformed("message"))));
    assert!(matches!(pipeline.mic_message(&json!({"language": "Japanese"})), Err(PipelineError::Malformed("text"))));
    assert!(matches!(pipeline.speaker_message(&json!({"text": "hi"})), Err(PipelineError::Malformed("language"))));
}

#[test]
fn replacing_the_word_filter_forgets_the_old_words() {
    let (world, pipeline) = quiet_world();
    pipeline.set_word_filter(&["bad".to_string()]);
    pipeline.mic_message(&json!({"text": "so bad", "language": "English"})).unwrap();
    assert!(world.events.lock().unwrap().iter().any(|e| e[2] == "/run/word_filter"));
    world.events.lock().unwrap().clear();
    pipeline.set_word_filter(&[]);
    pipeline.mic_message(&json!({"text": "so bad two", "language": "English"})).unwrap();
    assert!(world.events.lock().unwrap().iter().all(|e| e[2] != "/run/word_filter"));
    assert_eq!(pipeline.history().len(), 1);
    pipeline.clear_history();
    assert!(pipeline.history().is_empty());
}

#[test]
fn the_latency_line_splits_the_time_into_translation_output_and_speech_recognition() {
    let golden = golden();
    let scenario = json!({
        "config": {"ENABLE_TRANSLATION": true}, "translate": [{"ok": ["x"], "success": [true]}],
        "mute": null, "ws_alive": false, "overlay": "none", "word_filter": [],
    });
    let raw = raw_latency(&golden, &scenario);
    let numbers: Vec<i64> = raw.split(|c: char| !c.is_ascii_digit()).filter(|n| !n.is_empty()).map(|n| n.parse().unwrap()).collect();
    // The digits of "[latency][mic] asr=100ms translate=41ms output=0ms total=141ms": asr, translate, output, total.
    assert_eq!(numbers.len(), 4, "{raw}");
    assert_eq!(numbers[0], 100);
    assert!(numbers[1] >= 40, "the translator took 40 ms: {raw}");
    assert_eq!(numbers[3], numbers[1] + numbers[2] + 100, "total is everything including recognition: {raw}");
}

/// The latency line as `Host::log` received it, before the numbers are blanked.
fn raw_latency(golden: &Value, scenario: &Value) -> String {
    struct Capture(Arc<World>, Mutex<String>);
    impl Config for Capture {
        fn get(&self, name: &str) -> Option<Value> {
            self.0.get(name)
        }
    }
    macro_rules! forward {
        ($($name:ident($($arg:ident: $ty:ty),*) $(-> $ret:ty)?;)*) => {
            $(fn $name(&self, $($arg: $ty),*) $(-> $ret)? { self.0.$name($($arg),*) })*
        };
    }
    impl Host for Capture {
        fn log(&self, text: &str) {
            *self.1.lock().unwrap() = text.to_string();
        }
        forward! {
            run(status: u16, endpoint: &str, payload: Value);
            translate(direction: Direction, message: &str, source_language: Option<&str>) -> Result<Translated, TranslateError>;
            transliterate(message: &str, hiragana: bool, romaji: bool) -> Vec<Value>;
            send_osc(message: &str);
            overlay_available() -> bool;
            overlay_small_log(log: &SmallLog<'_>);
            overlay_large_log(log: &LargeLog<'_>);
            set_clipboard(text: &str);
            websocket_alive() -> bool;
            websocket_send(message: Value);
            log_info(text: &str);
            mic_mute_status() -> Option<bool>;
            set_setting(name: &str, value: Value);
            fall_back_to_ctranslate2();
            disable_translation();
        }
    }
    let (world, _) = world_with_delay(golden, scenario, std::time::Duration::from_millis(40));
    let capture = Arc::new(Capture(world, Mutex::new(String::new())));
    let pipeline = Pipeline::new(capture.clone(), capture.clone());
    pipeline.mic_message(&json!({"text": "hello", "language": "English", "asr_ms": 100})).unwrap();
    let line = capture.1.lock().unwrap().clone();
    line
}
