//! The translation flow against what the real `Model.getTranslate` / `getInputTranslate` /
//! `getOutputTranslate` did (`tests/fixtures/flow_golden.json`, made by `regenerate_flow_golden.py`).
//!
//! The translator is scripted by (engine, target language, country) and how often it was asked, so the
//! targets that run side by side give the same calls in any order; the calls are compared as a sorted list.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use vrct_core::pipeline::spec::Direction;
use vrct_core::pipeline::SharedHistory;
use vrct_core::translation::flow::{Reply, Request, TranslationFlow, Translator};

fn golden() -> Value {
    serde_json::from_str(include_str!("fixtures/flow_golden.json")).expect("flow_golden.json is valid JSON")
}

struct Scripted {
    replies: HashMap<String, Vec<Value>>,
    counters: Mutex<HashMap<String, usize>>,
    loaded: bool,
    events: Mutex<Vec<Value>>,
}

impl Translator for Scripted {
    fn translate(&self, request: &Request<'_>) -> Reply {
        let name = |value: Option<&str>| value.unwrap_or("None").to_string();
        let key = format!("{}|{}|{}", request.engine, name(request.target_language), name(request.target_country));
        self.events.lock().unwrap().push(json!([
            "translate",
            request.engine,
            request.weight_type,
            request.source_language,
            request.target_language,
            request.target_country,
            request.message,
            request.history.map(<[Value]>::len),
        ]));
        let index = {
            let mut counters = self.counters.lock().unwrap();
            let counter = counters.entry(key.clone()).or_insert(0);
            *counter += 1;
            *counter - 1
        };
        let default = vec![json!("false")];
        let replies = self.replies.get(&key).unwrap_or(&default);
        match &replies[index.min(replies.len() - 1)] {
            Value::String(kind) if kind == "none" => Reply::Unsupported,
            Value::String(_) => Reply::Failed,
            reply => Reply::Text(reply["text"].as_str().unwrap().to_string()),
        }
    }

    fn ctranslate2_loaded(&self) -> bool {
        self.loaded
    }

    fn pause(&self, duration: Duration) {
        self.events.lock().unwrap().push(json!(["sleep", duration.as_secs_f64()]));
    }

    fn report_failure(&self) {
        self.events.lock().unwrap().push(json!(["error_logging"]));
    }
}

fn sorted(mut events: Vec<Value>) -> Vec<String> {
    let mut keys: Vec<String> = events.drain(..).map(|e| e.to_string()).collect();
    keys.sort();
    keys
}

fn run(scenario: &Value) -> (Value, Vec<Value>) {
    let slots = |items: &Value| -> Value {
        let mut slots = items.as_array().unwrap().clone();
        while slots.len() < 3 {
            slots.push(json!({"language": "English", "country": "United States", "enable": false}));
        }
        Value::Object(slots.into_iter().enumerate().map(|(i, slot)| ((i + 1).to_string(), slot)).collect())
    };
    let config: HashMap<String, Value> = HashMap::from([
        ("SELECTED_TAB_NO".to_string(), json!("1")),
        ("SELECTED_TRANSLATION_ENGINES".to_string(), json!({"1": scenario["engine"]})),
        ("CTRANSLATE2_WEIGHT_TYPE".to_string(), scenario["weight"].clone()),
        ("SELECTED_YOUR_LANGUAGES".to_string(), json!({"1": {"1": scenario["yours"]}})),
        ("SELECTED_TARGET_LANGUAGES".to_string(), json!({"1": slots(&scenario["targets"])})),
    ]);
    let translator = Arc::new(Scripted {
        replies: scenario["replies"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_array().unwrap().clone())).collect(),
        counters: Mutex::new(HashMap::new()),
        loaded: scenario["loaded"].as_bool().unwrap(),
        events: Mutex::new(Vec::new()),
    });
    let history = SharedHistory::default();
    for i in 0..scenario["history"].as_u64().unwrap() {
        history.add("mic", &format!("h{i}"));
    }
    let flow = TranslationFlow::new(Arc::new(config), translator.clone(), history);
    let direction = if scenario["call"] == "input" { Direction::Input } else { Direction::Output };
    let translated = flow.translate(direction, scenario["message"].as_str().unwrap(), scenario["source"].as_str()).unwrap();
    let events = translator.events.lock().unwrap().clone();
    (json!({"translation": translated.translation, "success": translated.success}), events)
}

#[test]
fn every_recorded_scenario_does_what_python_did() {
    let golden = golden();
    let scenarios = golden["scenarios"].as_array().unwrap();
    assert!(scenarios.len() > 150);
    let mut failures = Vec::new();
    for scenario in scenarios {
        let name = scenario["name"].as_str().unwrap();
        let (outcome, events) = run(scenario);
        if outcome != scenario["outcome"] {
            failures.push(format!("{name}: rust {outcome}, python {}", scenario["outcome"]));
        } else if sorted(events.clone()) != sorted(scenario["events"].as_array().unwrap().clone()) {
            failures.push(format!("{name}: the calls differ\n  rust:   {}\n  python: {}", sorted(events).join(" "), sorted(scenario["events"].as_array().unwrap().clone()).join(" ")));
        }
    }
    assert!(failures.is_empty(), "{} scenario(s) differ:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn a_target_panicking_is_an_error_not_a_hang() {
    struct Panics;
    impl Translator for Panics {
        fn translate(&self, request: &Request<'_>) -> Reply {
            if request.target_language == Some("Korean") {
                panic!("engine blew up");
            }
            Reply::Text("ok".into())
        }
        fn ctranslate2_loaded(&self) -> bool {
            true
        }
    }
    let config: HashMap<String, Value> = HashMap::from([
        ("SELECTED_TAB_NO".to_string(), json!("1")),
        ("SELECTED_TRANSLATION_ENGINES".to_string(), json!({"1": "Google"})),
        ("CTRANSLATE2_WEIGHT_TYPE".to_string(), json!("m")),
        ("SELECTED_YOUR_LANGUAGES".to_string(), json!({"1": {"1": {"language": "Japanese", "country": "Japan", "enable": true}}})),
        ("SELECTED_TARGET_LANGUAGES".to_string(), json!({"1": {
            "1": {"language": "English", "country": "US", "enable": true},
            "2": {"language": "Korean", "country": "KR", "enable": true},
            "3": {"language": "French", "country": "FR", "enable": false}}})),
    ]);
    let flow = TranslationFlow::new(Arc::new(config), Arc::new(Panics), SharedHistory::default());
    let error = flow.input("hi", None).unwrap_err();
    assert!(error.to_string().contains("panicked"), "{error}");
}
