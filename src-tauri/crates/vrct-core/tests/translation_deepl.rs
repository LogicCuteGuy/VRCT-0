//! DeepL calls: what goes on the wire, and how failures read.

mod common;

use common::mock;
use serde_json::json;
use vrct_core::translation::deepl::{build, check, translate, Check, Request};

fn request(key: &str, text: &str, source: Option<&str>, target: &str, server: Option<String>) -> Request {
    Request {
        auth_key: key.into(),
        text: text.into(),
        source_lang: source.map(str::to_string),
        target_lang: target.into(),
        server_url: server,
    }
}

const OK: &str = r#"{"translations":[{"detected_source_language":"JA","text":"Hello","billed_characters":5}]}"#;

#[test]
fn the_server_follows_the_key_like_the_sdk() {
    let free = build(&request("abc:fx", "x", None, "DE", None)).unwrap();
    assert_eq!(free.url, "https://api-free.deepl.com/v2/translate");
    let pro = build(&request("abc", "x", None, "DE", None)).unwrap();
    assert_eq!(pro.url, "https://api.deepl.com/v2/translate");
    let custom = build(&request("abc", "x", None, "DE", Some("http://proxy.local/".into()))).unwrap();
    assert_eq!(custom.url, "http://proxy.local/v2/translate");
}

#[test]
fn the_body_matches_the_sdk() {
    let call = build(&request("k:fx", "こんにちは", Some("ja"), "en-us", None)).unwrap();
    assert_eq!(call.headers, vec![("authorization", "DeepL-Auth-Key k:fx".to_string())]);
    assert_eq!(
        call.body,
        json!({
            "target_lang": "EN-US",
            "source_lang": "JA",
            "text": ["こんにちは"],
            "show_billed_characters": true,
        })
    );
    // Source is left out entirely when DeepL should detect it.
    let detect = build(&request("k", "x", None, "DE", None)).unwrap();
    assert!(detect.body.get("source_lang").is_none());
}

#[test]
fn the_sdk_refusals_are_kept() {
    let refused = |key, text, target: &str| build(&request(key, text, None, target, None)).unwrap_err();
    assert!(refused("", "x", "DE").contains("auth_key"));
    assert!(refused("k", "", "DE").contains("text must not be empty"));
    assert!(refused("k", "x", "EN").contains("EN-GB"));
    assert!(refused("k", "x", "en").contains("EN-GB"));
    assert!(refused("k", "x", "PT").contains("PT-BR"));
    // Regional variants are fine.
    assert!(build(&request("k", "x", None, "PT-BR", None)).is_ok());
}

#[tokio::test]
async fn translates_over_http() {
    let server = mock(vec![(200, OK.into())]).await;
    let text = translate(request("secret:fx", "こんにちは", Some("JA"), "EN-US", Some(server.base()))).await.unwrap();
    assert_eq!(text, "Hello");

    let seen = server.requests();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].request_line.starts_with("POST /v2/translate "));
    assert_eq!(seen[0].header("authorization"), Some("DeepL-Auth-Key secret:fx"));
    assert_eq!(seen[0].body["text"], json!(["こんにちは"]));
    assert_eq!(seen[0].body["target_lang"], "EN-US");
}

#[tokio::test]
async fn a_reply_without_a_translation_is_an_error() {
    let server = mock(vec![(200, r#"{"translations":[]}"#.into())]).await;
    let error = translate(request("k", "x", None, "DE", Some(server.base()))).await.unwrap_err();
    assert!(error.contains("no translation"), "{error}");
}

#[tokio::test]
async fn failures_carry_the_status_and_the_providers_message_but_never_the_key() {
    let server = mock(vec![(403, r#"{"message":"Forbidden"}"#.into())]).await;
    let error = translate(request("secret-key", "x", None, "DE", Some(server.base()))).await.unwrap_err();
    assert_eq!(error, "HTTP 403: Forbidden");
    assert!(!error.contains("secret-key"));
    assert_eq!(server.requests().len(), 1, "an auth failure is not retried");
}

#[tokio::test]
async fn a_spent_quota_is_not_retried_but_a_busy_server_is() {
    let quota = mock(vec![(456, r#"{"message":"Quota Exceeded"}"#.into())]).await;
    let error = translate(request("k", "x", None, "DE", Some(quota.base()))).await.unwrap_err();
    assert_eq!(error, "HTTP 456: Quota Exceeded");
    assert_eq!(quota.requests().len(), 1);

    let busy = mock(vec![(429, r#"{"message":"Too many requests"}"#.into()), (200, OK.into())]).await;
    let text = translate(request("k", "x", None, "DE", Some(busy.base()))).await.unwrap();
    assert_eq!(text, "Hello");
    assert_eq!(busy.requests().len(), 2);
}

#[tokio::test]
async fn the_key_check_spends_a_blank_translation_like_python_did() {
    let server = mock(vec![(200, OK.into())]).await;
    let ok = check(Check { auth_key: "k".into(), server_url: Some(server.base()) }).await.unwrap();
    assert!(ok);
    let seen = server.requests();
    assert_eq!(seen[0].body["text"], json!([" "]));
    assert_eq!(seen[0].body["target_lang"], "EN-US");
    assert!(seen[0].body.get("source_lang").is_none());

    let rejecting = mock(vec![(403, r#"{"message":"Forbidden"}"#.into())]).await;
    let error = check(Check { auth_key: "bad".into(), server_url: Some(rejecting.base()) }).await.unwrap_err();
    assert!(error.starts_with("HTTP 403"));
}
