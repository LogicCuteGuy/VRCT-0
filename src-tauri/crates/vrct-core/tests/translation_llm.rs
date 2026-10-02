//! LLM translation against what the real Python clients built and against a
//! scripted provider. `fixtures/translation_golden.json` is captured from the
//! real clients by `fixtures/regenerate_translation_golden.py`.

mod common;

use common::mock;
use serde_json::{json, Value};
use vrct_core::translation::llm::{build, translate, Request};
use vrct_core::translation::prompt::{hour_minute, reply_text, system_prompt};

const GOLDEN: &str = include_str!("fixtures/translation_golden.json");

fn golden() -> Value {
    serde_json::from_str(GOLDEN).expect("golden fixture is JSON")
}

fn request(engine: &str, base: Option<String>) -> Request {
    serde_json::from_value(json!({
        "engine": engine,
        "base_url": base,
        "api_key": "sk-secret",
        "model": "some-model",
        "text": "hello",
        "input_lang": "Japanese",
        "output_lang": "English",
        "history": [],
    }))
    .unwrap()
}

#[test]
fn the_system_prompt_is_what_every_python_client_built() {
    let golden = golden();
    let cases = golden["cases"].as_array().unwrap();
    assert!(cases.len() >= 50, "fixture looks empty");
    for case in cases {
        let history = case["history"].as_array().unwrap();
        let built = system_prompt(
            case["engine"].as_str().unwrap(),
            case["input_lang"].as_str().unwrap(),
            case["output_lang"].as_str().unwrap(),
            history,
        )
        .unwrap();
        let expected = case["messages"][0]["content"].as_str().unwrap();
        assert!(
            built == expected,
            "prompt differs for {} {}->{} with {} history items\n--- rust\n{built}\n--- python\n{expected}",
            case["engine"],
            case["input_lang"],
            case["output_lang"],
            history.len()
        );
        assert_eq!(case["messages"][0]["role"], "system");
        assert_eq!(case["messages"][1], json!({"role": "user", "content": case["text"]}));
    }
}

#[test]
fn the_openai_body_carries_pythons_exact_messages() {
    let golden = golden();
    for case in golden["cases"].as_array().unwrap() {
        let engine = case["engine"].as_str().unwrap();
        if engine == "Ollama" || engine == "Gemini_API" {
            continue;
        }
        let mut req = request(engine, None);
        req.text = case["text"].as_str().unwrap().into();
        req.input_lang = case["input_lang"].as_str().unwrap().into();
        req.output_lang = case["output_lang"].as_str().unwrap().into();
        req.history = case["history"].as_array().unwrap().clone();
        let call = build(&req).unwrap();
        assert_eq!(call.body["messages"], case["messages"], "{engine}");
    }
}

#[test]
fn model_replies_are_read_like_python_read_them() {
    for case in golden()["replies"].as_array().unwrap() {
        assert_eq!(reply_text(&case["content"]), case["expected"].as_str().unwrap(), "{}", case["name"]);
    }
}

#[test]
fn history_timestamps_match_fromisoformat() {
    for case in golden()["stamps"].as_array().unwrap() {
        let value = case["value"].as_str().unwrap();
        assert_eq!(hour_minute(&json!(value)), case["expected"].as_str().unwrap(), "{value:?}");
    }
    // Python raises for these, which the pipeline renders as an empty stamp.
    for value in [json!(null), json!(12), json!(["2026-10-02"])] {
        assert_eq!(hour_minute(&value), "");
    }
}

#[test]
fn each_provider_gets_its_own_url_headers_and_body() {
    let openai = build(&request("OpenAI_API", None)).unwrap();
    assert_eq!(openai.url, "https://api.openai.com/v1/chat/completions");
    assert_eq!(openai.headers, vec![("authorization", "Bearer sk-secret".to_string())]);
    assert_eq!(openai.body["model"], "some-model");
    assert_eq!(openai.body["stream"], false);
    assert!(openai.body.get("temperature").is_none(), "langchain-openai sends no temperature");

    let groq = build(&request("Groq_API", Some("https://api.groq.com/openai/v1/".into()))).unwrap();
    assert_eq!(groq.url, "https://api.groq.com/openai/v1/chat/completions");

    let lmstudio = build(&request("LMStudio", Some("http://127.0.0.1:1234/v1".into()))).unwrap();
    assert_eq!(lmstudio.url, "http://127.0.0.1:1234/v1/chat/completions");

    let ollama = build(&request("Ollama", None)).unwrap();
    assert_eq!(ollama.url, "http://localhost:11434/api/chat");
    assert!(ollama.headers.is_empty());
    assert_eq!(ollama.body["stream"], false);

    let gemini = build(&request("Gemini_API", None)).unwrap();
    assert_eq!(
        gemini.url,
        "https://generativelanguage.googleapis.com/v1beta/models/some-model:generateContent"
    );
    assert_eq!(gemini.headers, vec![("x-goog-api-key", "sk-secret".to_string())]);
    assert_eq!(gemini.body["contents"], json!([{"role": "user", "parts": [{"text": "hello"}]}]));
    assert_eq!(gemini.body["generationConfig"]["temperature"], 0.7);
    let system = gemini.body["systemInstruction"]["parts"][0]["text"].as_str().unwrap();
    assert!(system.contains("from Japanese to English"));
}

#[test]
fn bad_requests_are_refused_before_any_network_call() {
    assert!(build(&request("DeepL_API", None)).is_err(), "not an LLM engine");
    let mut no_key = request("OpenAI_API", None);
    no_key.api_key = None;
    assert!(build(&no_key).is_err());
    no_key.api_key = Some(String::new());
    assert!(build(&no_key).is_err());
    let mut no_model = request("OpenAI_API", None);
    no_model.model = String::new();
    assert!(build(&no_model).is_err());
    // Ollama needs no key.
    let mut ollama = request("Ollama", None);
    ollama.api_key = None;
    assert!(build(&ollama).is_ok());
}

#[tokio::test]
async fn openai_round_trip_sends_the_call_and_strips_the_reply() {
    let server = mock(vec![(
        200,
        json!({"choices": [{"message": {"role": "assistant", "content": "  Hello there \n"}}]}).to_string(),
    )])
    .await;
    let mut req = request("OpenAI_Compatible", Some(format!("{}/v1", server.base())));
    req.history = vec![json!({"source": "chat", "text": "earlier", "timestamp": "2026-10-02T12:34:56"})];
    let text = translate(req).await.unwrap();
    assert_eq!(text, "Hello there");

    let seen = server.requests();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].request_line, "POST /v1/chat/completions HTTP/1.1");
    assert_eq!(seen[0].header("authorization"), Some("Bearer sk-secret"));
    assert_eq!(seen[0].body["messages"][1], json!({"role": "user", "content": "hello"}));
    assert!(seen[0].body["messages"][0]["content"].as_str().unwrap().contains("[12:34][chat] earlier"));
}

#[tokio::test]
async fn ollama_round_trip() {
    let server = mock(vec![(200, json!({"message": {"role": "assistant", "content": "Bonjour"}}).to_string())]).await;
    let text = translate(request("Ollama", Some(server.base()))).await.unwrap();
    assert_eq!(text, "Bonjour");
    assert_eq!(server.requests()[0].request_line, "POST /api/chat HTTP/1.1");
}

#[tokio::test]
async fn gemini_round_trip_joins_parts_and_skips_thoughts() {
    let reply = json!({"candidates": [{"content": {"parts": [
        {"text": "private reasoning", "thought": true},
        {"text": " Hola "},
        {"text": "mundo\n"},
    ]}}]});
    let server = mock(vec![(200, reply.to_string())]).await;
    let text = translate(request("Gemini_API", Some(server.base()))).await.unwrap();
    assert_eq!(text, "Hola mundo");
    let seen = server.requests();
    assert_eq!(seen[0].request_line, "POST /models/some-model:generateContent HTTP/1.1");
    assert_eq!(seen[0].header("x-goog-api-key"), Some("sk-secret"));
}

#[tokio::test]
async fn a_blocked_gemini_prompt_is_an_empty_translation_not_an_error() {
    let server = mock(vec![(200, json!({"promptFeedback": {"blockReason": "SAFETY"}}).to_string())]).await;
    assert_eq!(translate(request("Gemini_API", Some(server.base()))).await.unwrap(), "");
}

#[tokio::test]
async fn auth_and_bad_request_errors_are_not_retried_and_name_the_provider_message() {
    for status in [400u16, 401, 404] {
        let server = mock(vec![(status, json!({"error": {"message": "nope, bad key"}}).to_string())]).await;
        let error = translate(request("OpenAI_API", Some(server.base()))).await.unwrap_err();
        assert_eq!(error, format!("HTTP {status}: nope, bad key"));
        assert_eq!(server.requests().len(), 1, "status {status} must not be retried");
    }
    let server = mock(vec![(400, "not json".to_string())]).await;
    assert_eq!(translate(request("Ollama", Some(server.base()))).await.unwrap_err(), "HTTP 400");
}

#[tokio::test]
async fn the_error_never_contains_the_api_key() {
    let server = mock(vec![(401, json!({"error": {"message": "bad"}}).to_string())]).await;
    let error = translate(request("OpenAI_API", Some(server.base()))).await.unwrap_err();
    assert!(!error.contains("sk-secret"));
    // A refused connection reports no URL either.
    let error = translate(request("OpenAI_API", Some("http://127.0.0.1:1/v1".into()))).await.unwrap_err();
    assert!(!error.contains("127.0.0.1") && !error.contains("sk-secret"), "{error}");
}

#[tokio::test]
async fn server_errors_and_rate_limits_are_retried() {
    let ok = json!({"choices": [{"message": {"content": "done"}}]}).to_string();
    let server = mock(vec![(503, "{}".into()), (429, "{}".into()), (200, ok)]).await;
    let text = translate(request("Groq_API", Some(server.base()))).await.unwrap();
    assert_eq!(text, "done");
    assert_eq!(server.requests().len(), 3);
}

#[tokio::test]
async fn retries_stop_after_three_attempts() {
    let server = mock(vec![(503, json!({"error": {"message": "overloaded"}}).to_string())]).await;
    let error = translate(request("Groq_API", Some(server.base()))).await.unwrap_err();
    assert_eq!(error, "HTTP 503: overloaded");
    assert_eq!(server.requests().len(), 3);
}

#[tokio::test]
async fn a_reply_without_content_is_an_error() {
    let server = mock(vec![(200, json!({"choices": []}).to_string())]).await;
    assert!(translate(request("OpenAI_API", Some(server.base()))).await.is_err());
}
