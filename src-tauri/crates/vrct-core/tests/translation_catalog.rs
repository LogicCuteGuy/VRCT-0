//! Key checks and model lists for the LLM engines.

mod common;

use common::mock;
use serde_json::{json, Value};
use vrct_core::translation::catalog::{auth_check, models, Target};

fn target(engine: &str, key: Option<&str>, base: Option<String>) -> Target {
    Target { engine: engine.into(), api_key: key.map(str::to_string), base_url: base }
}

/// An address nothing listens on.
async fn closed_port() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    format!("http://127.0.0.1:{port}")
}

#[tokio::test]
async fn model_lists_match_what_python_keeps() {
    let golden: Value = serde_json::from_str(include_str!("fixtures/models_golden.json")).unwrap();
    for (engine, case) in golden.as_object().unwrap() {
        let reply = case["reply"].to_string();
        let expected: Vec<String> = serde_json::from_value(case["expected"].clone()).unwrap();
        // Ollama asks "are you there" before listing.
        let replies = if engine == "Ollama" { vec![(200, "Ollama is running".into()), (200, reply)] } else { vec![(200, reply)] };
        let server = mock(replies).await;
        let key = (!matches!(engine.as_str(), "LMStudio" | "Ollama")).then_some("key");
        let found = models(target(engine, key, Some(server.base()))).await.unwrap_or_else(|e| panic!("{engine}: {e}"));
        assert_eq!(found, expected, "{engine}");
        assert!(!expected.is_empty(), "{engine}: the corpus should keep something");
    }
}

#[tokio::test]
async fn requests_carry_the_key_the_way_each_provider_wants() {
    let server = mock(vec![(200, json!({"data": [{"id": "gpt-4o"}]}).to_string())]).await;
    models(target("OpenAI_API", Some("sk-1"), Some(server.base()))).await.unwrap();
    let seen = server.requests();
    assert!(seen[0].request_line.starts_with("GET /models "));
    assert_eq!(seen[0].header("authorization"), Some("Bearer sk-1"));

    let gemini = mock(vec![(200, json!({"models": []}).to_string())]).await;
    models(target("Gemini_API", Some("g-1"), Some(gemini.base()))).await.unwrap();
    let seen = gemini.requests();
    assert!(seen[0].request_line.starts_with("GET /models "));
    assert_eq!(seen[0].header("x-goog-api-key"), Some("g-1"));
    assert_eq!(seen[0].header("authorization"), None);
}

#[tokio::test]
async fn geminis_pages_are_all_read() {
    let page = |names: &[&str], next: Option<&str>| {
        let models: Vec<Value> = names
            .iter()
            .map(|name| json!({"name": format!("models/{name}"), "supportedGenerationMethods": ["generateContent"]}))
            .collect();
        let mut body = json!({"models": models});
        if let Some(next) = next {
            body["nextPageToken"] = json!(next);
        }
        (200, body.to_string())
    };
    let server = mock(vec![page(&["gemini-b"], Some("tok en/1")), page(&["gemini-a"], None)]).await;
    let found = models(target("Gemini_API", Some("k"), Some(server.base()))).await.unwrap();
    assert_eq!(found, vec!["gemini-a", "gemini-b"]);
    let seen = server.requests();
    assert_eq!(seen.len(), 2);
    assert!(seen[1].request_line.starts_with("GET /models?pageToken=tok%20en%2F1 "), "{}", seen[1].request_line);
}

#[tokio::test]
async fn a_rejected_key_is_an_error_with_the_status_and_never_the_key() {
    let server = mock(vec![(401, json!({"error": {"message": "Incorrect API key"}}).to_string())]).await;
    let error = auth_check(target("OpenAI_API", Some("sk-secret"), Some(server.base()))).await.unwrap_err();
    assert_eq!(error, "HTTP 401: Incorrect API key");
    assert!(!error.contains("sk-secret"));
    assert_eq!(server.requests().len(), 1);

    let error = models(target("Groq_API", Some("gsk-secret"), Some(server.base()))).await.unwrap_err();
    assert!(error.starts_with("HTTP 401"), "{error}");
}

#[tokio::test]
async fn a_valid_key_is_true_and_a_missing_key_never_leaves_the_machine() {
    let server = mock(vec![(200, json!({"data": []}).to_string())]).await;
    for engine in ["OpenAI_API", "OpenAI_Compatible", "Groq_API", "Plamo_API", "Gemini_API"] {
        assert!(auth_check(target(engine, Some("k"), Some(server.base()))).await.unwrap(), "{engine}");
    }
    assert_eq!(server.requests().len(), 5);

    for key in [None, Some("")] {
        let error = auth_check(target("OpenAI_API", key, Some(server.base()))).await.unwrap_err();
        assert_eq!(error, "no API key");
    }
    assert_eq!(server.requests().len(), 5, "no request without a key");
}

#[tokio::test]
async fn openrouter_checks_its_key_endpoint_and_a_refusal_is_just_false() {
    let ok = mock(vec![(200, "{}".into())]).await;
    assert!(auth_check(target("OpenRouter_API", Some("or-1"), Some(ok.base()))).await.unwrap());
    let seen = ok.requests();
    assert!(seen[0].request_line.starts_with("GET /auth/key "));
    assert_eq!(seen[0].header("authorization"), Some("Bearer or-1"));

    let refused = mock(vec![(401, "{}".into())]).await;
    assert!(!auth_check(target("OpenRouter_API", Some("bad"), Some(refused.base()))).await.unwrap());
    assert_eq!(refused.requests().len(), 1, "not retried");

    // Python lets a network failure escape here, unlike the other engines.
    assert!(auth_check(target("OpenRouter_API", Some("k"), Some(closed_port().await))).await.is_err());
}

#[tokio::test]
async fn local_servers_answer_false_or_empty_when_nothing_listens() {
    let down = closed_port().await;
    for engine in ["LMStudio", "Ollama"] {
        assert!(!auth_check(target(engine, None, Some(down.clone()))).await.unwrap(), "{engine}");
        assert!(models(target(engine, None, Some(down.clone()))).await.unwrap().is_empty(), "{engine}");
    }
    // LM Studio has no default address.
    assert!(!auth_check(target("LMStudio", None, None)).await.unwrap());
    assert!(models(target("LMStudio", None, None)).await.unwrap().is_empty());
}

#[tokio::test]
async fn local_servers_are_checked_by_their_own_endpoints() {
    let lm = mock(vec![(200, json!({"data": [{"id": "m"}]}).to_string())]).await;
    assert!(auth_check(target("LMStudio", None, Some(lm.base()))).await.unwrap());
    assert!(lm.requests()[0].request_line.starts_with("GET /models "));

    let lm_bad = mock(vec![(500, "{}".into())]).await;
    assert!(!auth_check(target("LMStudio", None, Some(lm_bad.base()))).await.unwrap());

    let ollama = mock(vec![(200, "Ollama is running".into())]).await;
    assert!(auth_check(target("Ollama", None, Some(ollama.base()))).await.unwrap());
    assert!(ollama.requests()[0].request_line.starts_with("GET / "));
}

#[tokio::test]
async fn ollama_lists_nothing_when_its_greeting_is_not_ok_or_its_tags_fail() {
    let sick = mock(vec![(503, "busy".into())]).await;
    assert!(models(target("Ollama", None, Some(sick.base()))).await.unwrap().is_empty());
    assert_eq!(sick.requests().len(), 1, "no tags request after a failed greeting");

    let broken = mock(vec![(200, "Ollama is running".into()), (500, "oops".into())]).await;
    assert!(models(target("Ollama", None, Some(broken.base()))).await.unwrap().is_empty());
    assert!(broken.requests()[1].request_line.starts_with("GET /api/tags "));
}

#[tokio::test]
async fn a_busy_provider_is_retried() {
    let server = mock(vec![(429, "{}".into()), (200, json!({"data": [{"id": "gpt-4o"}]}).to_string())]).await;
    let found = models(target("OpenAI_API", Some("k"), Some(server.base()))).await.unwrap();
    assert_eq!(found, vec!["gpt-4o"]);
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn an_unknown_engine_is_refused() {
    assert!(auth_check(target("Nope", Some("k"), None)).await.unwrap_err().contains("unknown LLM engine"));
    assert!(models(target("Nope", Some("k"), None)).await.unwrap_err().contains("unknown LLM engine"));
}
