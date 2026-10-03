//! `NativeTranslator` against what the real `Translator.translate` did (`tests/fixtures/adapter_golden.json`,
//! made by `regenerate_adapter_golden.py`): the same answers, and the same calls to the clients with the same
//! codes, histories, keys and models.
//!
//! The web engines (Google, Bing, Papago) are not ported: Python asked its web library and the script made that
//! fail, so what is compared is the answer ("failed"), not the call.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use vrct_core::translation::flow::{Reply, Request, Translator};
use vrct_core::translation::llm;
use vrct_core::translation::native::{EngineClient, LocalModel, NativeTranslator, Remote};

fn golden() -> Value {
    serde_json::from_str(include_str!("fixtures/adapter_golden.json")).expect("adapter_golden.json is valid JSON")
}

/// The answers a scenario scripts, the last repeating.
struct Script {
    replies: HashMap<String, Vec<Value>>,
    asked: Mutex<HashMap<String, usize>>,
}

impl Script {
    fn next(&self, key: &str) -> Result<String, String> {
        let default = vec![json!("false")];
        let replies = self.replies.get(key).unwrap_or(&default);
        let index = {
            let mut asked = self.asked.lock().unwrap();
            let slot = asked.entry(key.to_string()).or_insert(0);
            *slot += 1;
            *slot - 1
        };
        match &replies[index.min(replies.len() - 1)] {
            Value::String(_) => Err("scripted failure".into()),
            reply => Ok(reply["text"].as_str().unwrap().to_string()),
        }
    }
}

struct World {
    script: Script,
    events: Mutex<Vec<Value>>,
    loaded: bool,
}

struct FakeRemote(Arc<World>);

impl Remote for FakeRemote {
    fn deepl(&self, auth_key: &str, text: &str, source: &str, target: &str) -> Result<String, String> {
        self.0.events.lock().unwrap().push(json!(["deepl", auth_key, text, source, target]));
        self.0.script.next("DeepL_API")
    }

    fn llm(&self, request: llm::Request) -> Result<String, String> {
        self.0.events.lock().unwrap().push(json!([
            "llm",
            request.engine,
            request.api_key,
            request.base_url,
            request.model,
            request.text,
            request.input_lang,
            request.output_lang,
            request.history.len()
        ]));
        self.0.script.next(&request.engine)
    }
}

struct FakeLocal(Arc<World>);

impl LocalModel for FakeLocal {
    fn loaded(&self) -> bool {
        self.0.loaded
    }

    fn translate(&self, message: &str, source: &str, target: &str, weight_type: &str) -> Result<String, String> {
        self.0.events.lock().unwrap().push(json!(["ct2", message, source, target, weight_type]));
        if !self.0.loaded {
            return Err("no model is loaded".into());
        }
        self.0.script.next("CTranslate2")
    }
}

fn translator_for(scenario: &Value) -> (NativeTranslator, Arc<World>) {
    let world = Arc::new(World {
        script: Script {
            replies: scenario["replies"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_array().unwrap().clone())).collect(),
            asked: Mutex::new(HashMap::new()),
        },
        events: Mutex::new(Vec::new()),
        loaded: scenario["loaded"].as_bool().unwrap(),
    });
    let translator = NativeTranslator::new(Arc::new(FakeRemote(world.clone())), Some(Arc::new(FakeLocal(world.clone()))));
    translator.set_deepl_key(scenario["deepl"].as_str().map(str::to_string));
    for (engine, config) in scenario["clients"].as_object().unwrap() {
        translator.set_client(
            engine,
            EngineClient {
                api_key: config["api_key"].as_str().map(str::to_string),
                base_url: config["base_url"].as_str().map(str::to_string),
                model: config["model"].as_str().unwrap().to_string(),
            },
        );
    }
    (translator, world)
}

fn ask(translator: &NativeTranslator, call: &Value) -> Reply {
    let history: Option<Vec<Value>> = call["history"]
        .as_u64()
        .map(|n| (0..n).map(|i| json!({"source": "mic", "text": format!("h{i}"), "timestamp": "t"})).collect());
    translator.translate(&Request {
        engine: call["engine"].as_str().unwrap(),
        weight_type: call["weight"].as_str().unwrap(),
        source_language: call["source"].as_str(),
        target_language: call["target"].as_str(),
        target_country: call["country"].as_str(),
        message: call["message"].as_str().unwrap(),
        history: history.as_deref(),
    })
}

#[test]
fn every_recorded_scenario_does_what_python_did() {
    let golden = golden();
    let scenarios = golden["scenarios"].as_array().unwrap();
    assert!(scenarios.len() > 200);
    let mut failures = Vec::new();
    for scenario in scenarios {
        let name = scenario["name"].as_str().unwrap();
        let (translator, world) = translator_for(scenario);
        assert_eq!(translator.ctranslate2_loaded(), scenario["loaded"].as_bool().unwrap(), "{name}");
        for (i, call) in scenario["calls"].as_array().unwrap().iter().enumerate() {
            let expected = &scenario["results"][i];
            let reply = ask(&translator, call);
            let effects: Vec<Value> = std::mem::take(&mut *world.events.lock().unwrap());
            let mut python_effects: Vec<Value> = expected["effects"].as_array().unwrap().clone();
            let asked_web = python_effects.iter().any(|e| e[0] == "web");
            python_effects.retain(|e| e[0] != "web");
            let outcome = match &reply {
                Reply::Text(text) => json!({"text": text}),
                Reply::Unsupported => json!("none"),
                Reply::Failed => json!("false"),
            };
            if outcome != expected["outcome"] || effects != python_effects {
                failures.push(format!(
                    "{name} call {i}: rust {outcome} {}, python {} {}",
                    json!(effects),
                    expected["outcome"],
                    json!(python_effects)
                ));
            }
            if asked_web {
                assert_eq!(expected["outcome"], "false", "{name}: a failed web call is a failure");
            }
        }
    }
    assert!(failures.is_empty(), "{} call(s) differ:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn clients_can_be_replaced_removed_and_given_another_model() {
    let scenario = json!({"replies": {"OpenAI_API": [{"text": "ok"}]}, "loaded": true, "deepl": null, "clients": {}});
    let (translator, world) = translator_for(&scenario);
    let call = json!({"engine": "OpenAI_API", "weight": "w", "source": "Japanese", "target": "English", "country": "United States", "message": "hi", "history": 2});
    assert_eq!(ask(&translator, &call), Reply::Failed, "nobody authenticated it");
    translator.set_client("OpenAI_API", EngineClient { api_key: Some("k".into()), base_url: None, model: "a".into() });
    assert_eq!(ask(&translator, &call), Reply::Text("ok".into()));
    assert!(translator.set_model("OpenAI_API", "b"));
    assert!(!translator.set_model("Groq_API", "b"), "no such client");
    ask(&translator, &call);
    let models: Vec<Value> = world.events.lock().unwrap().iter().map(|e| e[4].clone()).collect();
    assert_eq!(models, [json!("a"), json!("b")]);
    translator.remove_client("OpenAI_API");
    assert_eq!(ask(&translator, &call), Reply::Failed);
    // A fresh client starts without the old one's history.
    translator.set_client("OpenAI_API", EngineClient::default());
    let no_history = json!({"engine": "OpenAI_API", "weight": "w", "source": "Japanese", "target": "English", "country": "", "message": "hi", "history": null});
    world.events.lock().unwrap().clear();
    ask(&translator, &no_history);
    assert_eq!(world.events.lock().unwrap()[0][8], 0);
}

#[test]
fn without_a_local_model_the_local_engine_fails_and_is_not_loaded() {
    let world = Arc::new(World { script: Script { replies: HashMap::new(), asked: Mutex::new(HashMap::new()) }, events: Mutex::new(Vec::new()), loaded: true });
    let translator = NativeTranslator::new(Arc::new(FakeRemote(world)), None);
    assert!(!translator.ctranslate2_loaded());
    let reply = translator.translate(&Request {
        engine: "CTranslate2",
        weight_type: "m2m100_418M-ct2-int8",
        source_language: Some("Japanese"),
        target_language: Some("English"),
        target_country: None,
        message: "hi",
        history: None,
    });
    assert_eq!(reply, Reply::Failed);
}
