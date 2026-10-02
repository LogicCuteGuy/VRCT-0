//! Language-name resolution and whole-translation dispatch.

mod common;

use common::mock;
use serde_json::{json, Value};
use vrct_core::translation::languages::language_codes;
use vrct_core::translation::text::{handles, translate, Outcome, Request};

#[test]
fn language_codes_match_what_python_resolves() {
    let golden: Vec<Value> = serde_json::from_str(include_str!("fixtures/language_codes_golden.json")).unwrap();
    assert!(golden.len() > 1000, "the corpus should cover the whole table");
    let mut unsupported = 0;
    for case in &golden {
        // [engine, weight_type, country, source, target, expected]
        let field = |at: usize| case[at].as_str().unwrap().to_string();
        let got = language_codes(&field(0), &field(1), &field(2), &field(3), &field(4));
        match (&case[5], got) {
            (Value::Null, Err(_)) => unsupported += 1,
            (Value::Array(expected), Ok((source, target))) => {
                assert_eq!(expected, &vec![json!(source), json!(target)], "{case}");
            }
            (expected, got) => panic!("{case}: Python gave {expected}, Rust gave {got:?}"),
        }
    }
    assert!(unsupported > 10, "the corpus should include refusals");
}

fn request(engine: &str, source: &str, target: &str, base: &str) -> Request {
    serde_json::from_value(json!({
        "engine": engine, "source_language": source, "target_language": target,
        "target_country": "United States", "text": "こんにちは",
        "api_key": "key-1", "base_url": base, "model": "m-1",
    }))
    .unwrap()
}

const CHAT_OK: &str = r#"{"choices":[{"message":{"content":"Hello"}}]}"#;
const DEEPL_OK: &str = r#"{"translations":[{"text":"Hello","billed_characters":5}]}"#;

#[tokio::test]
async fn an_llm_engine_gets_the_resolved_codes_in_its_prompt() {
    let server = mock(vec![(200, CHAT_OK.into())]).await;
    let outcome = translate(request("OpenAI_Compatible", "Japanese", "English", &server.base())).await.unwrap();
    assert_eq!(outcome, Outcome::Text("Hello".into()));

    let (source, target) = language_codes("OpenAI_Compatible", "", "United States", "Japanese", "English").unwrap();
    let seen = server.requests();
    assert_eq!(seen.len(), 1);
    let system = seen[0].body["messages"][0]["content"].as_str().unwrap();
    assert!(system.contains(&source) && system.contains(&target), "{system}");
    assert_eq!(seen[0].body["messages"][1]["content"], "こんにちは");
    assert_eq!(seen[0].body["model"], "m-1");
}

#[tokio::test]
async fn deepl_gets_its_variant_from_the_country() {
    for (country, expected) in [("United States", "EN-US"), ("Canada", "EN-US"), ("United Kingdom", "EN-GB"), ("Japan", "EN-GB")] {
        let server = mock(vec![(200, DEEPL_OK.into())]).await;
        let mut req = request("DeepL_API", "Japanese", "English", &server.base());
        req.target_country = country.into();
        assert_eq!(translate(req).await.unwrap(), Outcome::Text("Hello".into()));
        let seen = server.requests();
        assert_eq!(seen[0].body["target_lang"], expected, "{country}");
        assert_eq!(seen[0].body["source_lang"], "JA");
        assert_eq!(seen[0].header("authorization"), Some("DeepL-Auth-Key key-1"));
    }
    let server = mock(vec![(200, DEEPL_OK.into())]).await;
    let mut req = request("DeepL_API", "Japanese", "Portuguese", &server.base());
    req.target_country = "Portugal".into();
    translate(req).await.unwrap();
    assert_eq!(server.requests()[0].body["target_lang"], "PT-PT");
}

#[tokio::test]
async fn an_unsupported_language_is_an_outcome_not_a_failure_and_sends_nothing() {
    let server = mock(vec![(200, CHAT_OK.into())]).await;
    let outcome = translate(request("OpenAI_API", "Klingon", "English", &server.base())).await.unwrap();
    assert!(matches!(outcome, Outcome::Unsupported(_)), "{outcome:?}");
    assert!(server.requests().is_empty());
    assert_eq!(outcome.to_json()["kind"], "unsupported");
}

#[tokio::test]
async fn the_same_language_comes_back_untouched() {
    let server = mock(vec![(200, CHAT_OK.into())]).await;
    let outcome = translate(request("OpenAI_API", "Japanese", "Japanese", &server.base())).await.unwrap();
    assert_eq!(outcome, Outcome::Text("こんにちは".into()));
    assert_eq!(outcome.to_json(), json!({"kind": "text", "text": "こんにちは"}));
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn failures_and_missing_pieces_are_errors() {
    let server = mock(vec![(403, r#"{"message":"Forbidden"}"#.into())]).await;
    let error = translate(request("DeepL_API", "Japanese", "English", &server.base())).await.unwrap_err();
    assert_eq!(error, "HTTP 403: Forbidden");

    let mut keyless = request("DeepL_API", "Japanese", "English", &server.base());
    keyless.api_key = None;
    assert_eq!(translate(keyless).await.unwrap_err(), "no API key");
    assert_eq!(server.requests().len(), 1, "no request without a key");

    // Not an engine this build performs.
    assert!(!handles("Google") && !handles("CTranslate2"));
    assert!(translate(request("Google", "Japanese", "English", &server.base())).await.is_err());
}
