//! Recorded protocol shapes exercise real HTTP serialization and parsing.
mod common;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use common::{mock, Captured};
use hmac::{Hmac, Mac};
use md5::Md5;
use reqwest::Url;
use serde_json::{json, Value};
use vrct_core::translation::web::{handles, translate, Request};

const GOOGLE_PAGE: &str = r#"<script>window.WIZ_global_data = {"FdrFJe":"session-1","cfb2h":"build-1","SNlM0e":"csrf-1"};</script>"#;
const BING_PAGE: &str = r#"<script>var params_AbusePreventionHelper = [12345,"token-1",3600000]; var state = {IG:"abc-ig"};</script><div data-iid='translator.123' id='tta_outGDCont'></div>"#;
const PAPAGO_PAGE: &str = r#"<script src="/_next/static/chunks/web.js"></script>"#;
const LEGACY_PAGE: &str = r#"<script src='/home.123.chunk.js'></script>"#;
const LEGACY_KEY: &str = "v1.8.99_abcdef0123";

fn request(engine: &str, base: &str) -> Request {
    Request {
        engine: engine.into(),
        text: "hello & สวัสดี\nsecond line".into(),
        source: "en".into(),
        target: "ja".into(),
        base_url: Some(base.into()),
    }
}

fn fields(request: &Captured) -> std::collections::HashMap<String, String> {
    Url::parse(&format!(
        "http://form.invalid/?{}",
        String::from_utf8_lossy(&request.raw)
    ))
    .unwrap()
    .query_pairs()
    .into_owned()
    .collect()
}

fn google_response() -> String {
    let payload = json!([
        null,
        [[[null, null, null, null, null, [["こんにちは"], ["二行目"]]]]]
    ]);
    let frame = json!([
        ["wrb.fr", "OtherRpc", "null"],
        ["wrb.fr", "MkEWBc", payload.to_string(), null]
    ])
    .to_string();
    format!(")]}}'\n\n{}\n{frame}\n", frame.len())
}

#[tokio::test]
async fn google_bootstrap_rpc_and_framed_sentences() {
    let server = mock(vec![(200, GOOGLE_PAGE.into()), (200, google_response())]).await;
    assert_eq!(
        translate(request("Google", &server.base())).await.unwrap(),
        "こんにちは 二行目"
    );
    let seen = server.requests();
    assert_eq!(seen.len(), 2);
    assert!(seen[0].request_line.starts_with("GET / "));
    let path = seen[1].request_line.split_whitespace().nth(1).unwrap();
    let url = Url::parse(&format!("{}{}", server.base(), path)).unwrap();
    assert_eq!(url.path(), "/_/TranslateWebserverUi/data/batchexecute");
    let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["f.sid"], "session-1");
    assert_eq!(query["bl"], "build-1");
    let form = fields(&seen[1]);
    assert_eq!(form["at"], "csrf-1");
    let rpc: Value = serde_json::from_str(&form["f.req"]).unwrap();
    assert_eq!(rpc[0][0][0], "MkEWBc");
    let payload: Value = serde_json::from_str(rpc[0][0][1].as_str().unwrap()).unwrap();
    assert_eq!(
        payload,
        json!([["hello & สวัสดี\nsecond line", "en", "ja", true], [1]])
    );
    assert_eq!(seen[1].header("origin"), Some(server.base().as_str()));
    assert_eq!(seen[1].header("x-same-domain"), Some("1"));
}

#[tokio::test]
async fn google_accepts_short_translation_payload_and_rejects_missing_rpc() {
    let payload = json!([null, [[["Hello"]]]]).to_string();
    let server = mock(vec![
        (200, GOOGLE_PAGE.into()),
        (200, json!([["wrb.fr", "MkEWBc", payload]]).to_string()),
    ])
    .await;
    assert_eq!(
        translate(request("Google", &server.base())).await.unwrap(),
        "Hello"
    );
    let server = mock(vec![
        (200, GOOGLE_PAGE.into()),
        (
            200,
            r#")]}'
[["wrb.fr","Else","null"]]"#
                .into(),
        ),
    ])
    .await;
    assert!(translate(request("Google", &server.base()))
        .await
        .unwrap_err()
        .contains("no translation"));
}

#[tokio::test]
async fn bing_tokens_query_and_form_preserve_unicode() {
    let server = mock(vec![
        (200, BING_PAGE.into()),
        (200, r#"[{"translations":[{"text":"こんにちは"}]}]"#.into()),
    ])
    .await;
    let mut req = request("Bing", &server.base());
    req.source = "auto".into();
    assert_eq!(translate(req).await.unwrap(), "こんにちは");
    let seen = server.requests();
    assert!(seen[0].request_line.starts_with("GET /translator "));
    assert!(seen[1]
        .request_line
        .contains("/ttranslatev3?isVertical=1&IG=abc-ig&IID=translator.123"));
    let form = fields(&seen[1]);
    assert_eq!(form["fromLang"], "auto-detect");
    assert_eq!(form["key"], "12345");
    assert_eq!(form["token"], "token-1");
    assert_eq!(form["text"], "hello & สวัสดี\nsecond line");
    assert_eq!(form["to"], "ja");
}

#[tokio::test]
async fn bing_regional_html_and_missing_bootstrap() {
    let server = mock(vec![
        (200, BING_PAGE.into()),
        (
            200,
            "<html><textarea>original</textarea><textarea>Hello &amp; goodbye</textarea></html>"
                .into(),
        ),
    ])
    .await;
    assert_eq!(
        translate(request("Bing", &server.base())).await.unwrap(),
        "Hello & goodbye"
    );
    let server = mock(vec![(200, "<html>captcha</html>".into())]).await;
    assert!(translate(request("Bing", &server.base()))
        .await
        .unwrap_err()
        .contains("bootstrap"));
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn papago_current_next_client_and_detection() {
    let server = mock(vec![
        (200, PAPAGO_PAGE.into()),
        (200, r#"{"langCode":"en"}"#.into()),
        (200, r#"{"translatedText":"こんにちは"}"#.into()),
    ])
    .await;
    let mut req = request("Papago", &server.base());
    req.source = "auto".into();
    assert_eq!(translate(req).await.unwrap(), "こんにちは");
    let seen = server.requests();
    assert!(seen[1].request_line.starts_with("POST /api/langs/dect "));
    assert_eq!(fields(&seen[1])["query"], "hello & สวัสดี\nsecond line");
    assert!(seen[2]
        .request_line
        .starts_with("POST /api/text/translation "));
    let form = fields(&seen[2]);
    assert_eq!(form["source"], "en");
    assert_eq!(form["target"], "ja");
    assert_eq!(form["text"], "hello & สวัสดี\nsecond line");
    assert_eq!(form["useGlossary"], "false");
    assert_eq!(form["dictDisplay"], "30");
    assert_eq!(form["honorific"], "false");
    assert!(seen[2].header("authorization").is_none());
}

#[tokio::test]
async fn papago_legacy_signs_actual_endpoint_device_and_timestamp() {
    let server = mock(vec![
        (200, LEGACY_PAGE.into()),
        (200, format!("var settings={{AUTH_KEY:\"{LEGACY_KEY}\"}};")),
        (200, r#"{"langCode":"en"}"#.into()),
        (200, r#"{"translatedText":"こんにちは"}"#.into()),
    ])
    .await;
    let mut req = request("Papago", &server.base());
    req.source = "auto".into();
    assert_eq!(translate(req).await.unwrap(), "こんにちは");
    let seen = server.requests();
    assert!(seen[1].request_line.starts_with("GET /home.123.chunk.js "));
    let form = fields(&seen[3]);
    let device = &form["deviceId"];
    assert_eq!(device.len(), 36);
    assert_eq!(&device[14..15], "4");
    for (index, path) in [(2, "/apis/langs/dect"), (3, "/apis/n2mt/translate")] {
        assert!(seen[index]
            .request_line
            .starts_with(&format!("POST {path} ")));
        let timestamp = seen[index].header("timestamp").unwrap();
        assert!(timestamp.parse::<u128>().unwrap() > 1_000_000_000_000);
        let mut mac = Hmac::<Md5>::new_from_slice(LEGACY_KEY.as_bytes()).unwrap();
        mac.update(format!("{device}\n{}{path}\n{timestamp}", server.base()).as_bytes());
        assert_eq!(
            seen[index].header("authorization"),
            Some(
                format!(
                    "PPG {device}:{}",
                    STANDARD.encode(mac.finalize().into_bytes())
                )
                .as_str()
            )
        );
        assert_eq!(seen[index].header("device-type"), Some("pc"));
    }
    assert_eq!(form["source"], "en");
}

#[tokio::test]
async fn stale_authorization_refreshes_entire_session_only_once() {
    let server = mock(vec![
        (200, BING_PAGE.into()),
        (403, "denied".into()),
        (200, BING_PAGE.replace("token-1", "fresh-token")),
        (200, r#"[{"translations":[{"text":"fresh"}]}]"#.into()),
    ])
    .await;
    assert_eq!(
        translate(request("Bing", &server.base())).await.unwrap(),
        "fresh"
    );
    let seen = server.requests();
    assert_eq!(seen.len(), 4);
    assert_eq!(fields(&seen[3])["token"], "fresh-token");

    let server = mock(vec![
        (200, PAPAGO_PAGE.into()),
        (403, "denied".into()),
        (200, PAPAGO_PAGE.into()),
        (403, "denied".into()),
    ])
    .await;
    assert_eq!(
        translate(request("Papago", &server.base()))
            .await
            .unwrap_err(),
        "Papago HTTP 403"
    );
    assert_eq!(server.requests().len(), 4);
}

#[tokio::test]
async fn transient_status_retries_post_and_unsupported_inputs_do_not_connect() {
    let server = mock(vec![
        (200, PAPAGO_PAGE.into()),
        (429, "throttled".into()),
        (503, "retry".into()),
        (200, r#"{"translatedText":"retry succeeded"}"#.into()),
    ])
    .await;
    assert_eq!(
        translate(request("Papago", &server.base())).await.unwrap(),
        "retry succeeded"
    );
    let seen = server.requests();
    assert_eq!(seen.len(), 4);
    assert_eq!(seen[1].raw, seen[2].raw);
    assert_eq!(seen[2].raw, seen[3].raw);
    for engine in ["Google", "Bing", "Papago"] {
        assert!(handles(engine));
    }
    assert!(!handles("Unknown"));
    let mut req = request("Unknown", &server.base());
    assert!(translate(req).await.unwrap_err().contains("unsupported"));
    req = request("Google", &server.base());
    req.text.clear();
    assert_eq!(translate(req).await.unwrap_err(), "empty translation text");
    req = request("Bing", &server.base());
    req.text = "ก".repeat(1001);
    assert!(translate(req)
        .await
        .unwrap_err()
        .contains("1000 characters"));
    req = request("Papago", &server.base());
    req.target = "auto".into();
    assert!(translate(req).await.unwrap_err().contains("language"));
    assert_eq!(server.requests().len(), 4);
}

#[tokio::test]
async fn missing_or_empty_translations_and_invalid_origins_are_errors() {
    for body in [
        r#"{"errorCode":"60109"}"#,
        r#"{"translatedText":""}"#,
        "not-json",
    ] {
        let server = mock(vec![(200, PAPAGO_PAGE.into()), (200, body.into())]).await;
        assert!(translate(request("Papago", &server.base())).await.is_err());
    }
    for origin in ["file:///secret", "https://user:pass@localhost/", "garbage"] {
        assert!(translate(request("Papago", origin))
            .await
            .unwrap_err()
            .contains("origin"));
    }
}

#[tokio::test]
async fn cookies_received_during_bootstrap_are_sent_to_the_translation_endpoint() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        for index in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let read = stream.read(&mut buffer).await.unwrap();
                assert_ne!(read, 0);
                bytes.extend_from_slice(&buffer[..read]);
                if bytes.windows(4).any(|part| part == b"\r\n\r\n") {
                    break;
                }
            }
            let headers = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
            if index == 1 {
                assert!(
                    headers.contains("\r\ncookie: provider-session=session-cookie\r\n"),
                    "{headers}"
                );
            }
            let body = if index == 0 {
                PAPAGO_PAGE
            } else {
                r#"{"translatedText":"cookie accepted"}"#
            };
            let cookie = if index == 0 {
                "Set-Cookie: provider-session=session-cookie; Path=/; HttpOnly\r\n"
            } else {
                ""
            };
            let reply = format!(
                "HTTP/1.1 200 OK\r\n{cookie}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(reply.as_bytes()).await.unwrap();
        }
    });
    assert_eq!(
        translate(request("Papago", &base)).await.unwrap(),
        "cookie accepted"
    );
    task.await.unwrap();
}

#[tokio::test]
async fn google_consent_form_preserves_hidden_fields_and_rejects_unexpected_origin() {
    let consent = r#"<form action='/consent/save'><input type='hidden' name='continue' value='a&amp;b'><input name='unused' type='text'><input type='hidden' name='token' value='consent-1'></form>"#;
    let server = mock(vec![
        (200, consent.into()),
        (200, GOOGLE_PAGE.into()),
        (200, google_response()),
    ])
    .await;
    assert_eq!(
        translate(request("Google", &server.base())).await.unwrap(),
        "こんにちは 二行目"
    );
    let seen = server.requests();
    assert!(seen[1].request_line.starts_with("POST /consent/save "));
    let form = fields(&seen[1]);
    assert_eq!(form["continue"], "a&b");
    assert_eq!(form["token"], "consent-1");
    assert!(!form.contains_key("unused"));

    let server = mock(vec![(
        200,
        consent.replace("/consent/save", "https://unexpected.invalid/collect"),
    )])
    .await;
    assert_eq!(
        translate(request("Google", &server.base()))
            .await
            .unwrap_err(),
        "unexpected Google consent origin"
    );
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn legacy_papago_requires_a_live_signing_key_and_permanent_failure_is_not_retried() {
    let server = mock(vec![
        (200, LEGACY_PAGE.into()),
        (200, "var settings={};".into()),
    ])
    .await;
    assert_eq!(
        translate(request("Papago", &server.base()))
            .await
            .unwrap_err(),
        "Papago bootstrap has no signing key"
    );
    assert_eq!(server.requests().len(), 2);

    let server = mock(vec![
        (200, PAPAGO_PAGE.into()),
        (400, r#"{"message":"secret-token-not-for-errors"}"#.into()),
    ])
    .await;
    assert_eq!(
        translate(request("Papago", &server.base()))
            .await
            .unwrap_err(),
        "Papago HTTP 400"
    );
    assert_eq!(server.requests().len(), 2);
}
