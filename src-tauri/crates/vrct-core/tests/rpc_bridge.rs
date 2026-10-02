//! The Python -> Rust call bridge: request lines in, answer lines out.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use vrct_core::protocol::{parse_sidecar_line, Response};
use vrct_core::rpc::{LineWriter, Rpc, IMPLEMENTED};

#[derive(Default)]
struct Lines(Mutex<Vec<String>>);

impl LineWriter for Lines {
    fn write_line(&self, line: &str) -> Result<(), String> {
        self.0.lock().unwrap().push(line.to_string());
        Ok(())
    }
}

impl Lines {
    /// The decoded answers written so far.
    fn answers(&self) -> Vec<Value> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|line| {
                let envelope: Value = serde_json::from_str(line.trim_end()).unwrap();
                assert_eq!(envelope["endpoint"], "/internal/rpc/response");
                let bytes = STANDARD.decode(envelope["data"].as_str().unwrap()).unwrap();
                serde_json::from_slice(&bytes).unwrap()
            })
            .collect()
    }

    async fn wait_for(&self, count: usize) -> Vec<Value> {
        for _ in 0..200 {
            if self.0.lock().unwrap().len() >= count {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        self.answers()
    }
}

fn request_line(result: Value) -> Response {
    let text = json!({"status": 200, "endpoint": "/internal/rpc/request", "result": result}).to_string();
    parse_sidecar_line(&text).unwrap()
}

fn rpc(lines: &Arc<Lines>) -> Rpc {
    Rpc::new(lines.clone() as Arc<dyn LineWriter>)
}

#[tokio::test]
async fn a_translate_call_is_answered_with_its_id() {
    let server = common::mock(vec![(
        200,
        json!({"choices": [{"message": {"content": "Hello"}}]}).to_string(),
    )])
    .await;
    let lines = Arc::new(Lines::default());
    let rpc = rpc(&lines);

    assert!(rpc.ingest(&request_line(json!({
        "id": 41,
        "method": "translate.llm",
        "params": {
            "engine": "OpenAI_Compatible", "base_url": format!("{}/v1", server.base()),
            "api_key": "sk-test", "model": "m", "text": "こんにちは",
            "input_lang": "Japanese", "output_lang": "English", "history": [],
        },
    }))));

    assert_eq!(lines.wait_for(1).await, vec![json!({"id": 41, "ok": true, "result": "Hello"})]);
}

#[tokio::test]
async fn failures_are_answered_not_dropped() {
    let lines = Arc::new(Lines::default());
    let rpc = rpc(&lines);

    for (id, method, params) in [
        (1, "no.such.method", json!({})),
        (2, "translate.llm", json!({"engine": "OpenAI_API"})), // missing fields
        (
            3,
            "translate.llm",
            json!({"engine": "DeepL_API", "model": "m", "text": "t", "input_lang": "a", "output_lang": "b"}),
        ),
    ] {
        assert!(rpc.ingest(&request_line(json!({"id": id, "method": method, "params": params}))));
    }
    let mut answers = lines.wait_for(3).await;
    answers.sort_by_key(|a| a["id"].as_u64());
    assert_eq!(answers.len(), 3);
    for answer in &answers {
        assert_eq!(answer["ok"], false, "{answer}");
        assert!(answer["error"].as_str().is_some_and(|e| !e.is_empty()));
    }
}

#[tokio::test]
async fn concurrent_calls_are_matched_to_their_own_ids() {
    let lines = Arc::new(Lines::default());
    let rpc = rpc(&lines).method("test.echo", |params| async move {
        // The slowest call finishes last, so answers come back out of order.
        let delay = params["delay_ms"].as_u64().unwrap();
        tokio::time::sleep(Duration::from_millis(delay)).await;
        Ok(params["value"].clone())
    });

    for (id, delay) in [(10u64, 150u64), (11, 10), (12, 80)] {
        rpc.ingest(&request_line(json!({
            "id": id, "method": "test.echo", "params": {"delay_ms": delay, "value": format!("v{id}")},
        })));
    }
    let answers = lines.wait_for(3).await;
    let order: Vec<u64> = answers.iter().map(|a| a["id"].as_u64().unwrap()).collect();
    assert_eq!(order, vec![11, 12, 10]);
    for answer in answers {
        let id = answer["id"].as_u64().unwrap();
        assert_eq!(answer["result"], json!(format!("v{id}")));
    }
}

#[tokio::test]
async fn a_panicking_method_still_gets_an_answer() {
    let lines = Arc::new(Lines::default());
    let rpc = rpc(&lines).method("test.boom", |_| async {
        panic!("method bug");
        #[allow(unreachable_code)]
        Ok(Value::Null)
    });
    rpc.ingest(&request_line(json!({"id": 7, "method": "test.boom", "params": null})));
    let answers = lines.wait_for(1).await;
    assert_eq!(answers[0]["id"], 7);
    assert_eq!(answers[0]["ok"], false);
}

#[tokio::test]
async fn other_lines_are_left_alone_and_a_request_without_an_id_is_consumed() {
    let lines = Arc::new(Lines::default());
    let rpc = rpc(&lines);
    let other = parse_sidecar_line(r#"{"status":200,"endpoint":"/run/enable_translation","result":true}"#).unwrap();
    assert!(!rpc.ingest(&other));
    assert!(rpc.ingest(&request_line(json!({"method": "translate.llm"}))));
    assert!(lines.0.lock().unwrap().is_empty());
}

#[test]
fn outside_a_runtime_the_caller_gets_an_error_not_a_panic() {
    let lines = Arc::new(Lines::default());
    let rpc = rpc(&lines);
    assert!(rpc.ingest(&request_line(json!({"id": 5, "method": "translate.llm", "params": {}}))));
    let answers = lines.answers();
    assert_eq!(answers[0]["ok"], false);
}

#[test]
fn the_advertised_methods_are_what_the_sidecar_is_told() {
    assert_eq!(vrct_core::rpc::rpc_env_value(), IMPLEMENTED.join(","));
    for method in ["translate.llm", "translate.deepl", "translate.deepl.check"] {
        assert!(IMPLEMENTED.contains(&method), "{method}");
    }
}

#[tokio::test]
async fn deepl_calls_are_answered_through_the_bridge() {
    let server = common::mock(vec![(
        200,
        json!({"translations": [{"text": "Hello", "billed_characters": 5}]}).to_string(),
    )])
    .await;
    let lines = Arc::new(Lines::default());
    let rpc = rpc(&lines);

    assert!(rpc.ingest(&request_line(json!({
        "id": 1, "method": "translate.deepl",
        "params": {"auth_key": "k", "text": "こんにちは", "source_lang": "JA", "target_lang": "EN-US",
                   "server_url": server.base()},
    }))));
    assert!(rpc.ingest(&request_line(json!({
        "id": 2, "method": "translate.deepl.check",
        "params": {"auth_key": "k", "server_url": server.base()},
    }))));
    assert!(rpc.ingest(&request_line(json!({
        "id": 3, "method": "translate.deepl",
        "params": {"auth_key": "k", "text": "x", "target_lang": "EN"},
    }))));

    let mut answers = lines.wait_for(3).await;
    answers.sort_by_key(|answer| answer["id"].as_u64());
    assert_eq!(answers[0], json!({"id": 1, "ok": true, "result": "Hello"}));
    assert_eq!(answers[1], json!({"id": 2, "ok": true, "result": true}));
    assert_eq!(answers[2]["ok"], false);
    assert!(answers[2]["error"].as_str().unwrap().contains("EN-GB"));
}
