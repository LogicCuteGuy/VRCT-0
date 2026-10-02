//! DeepL's translate endpoint, called the way the `deepl` Python SDK calls it.
//!
//! The SDK picks the free or pro server from the key's `:fx` suffix, sends a
//! JSON body with upper-cased language codes and refuses a bare `EN`/`PT`
//! target or empty text before touching the network. Python still holds the
//! authenticated key and the resolved codes; this is only the HTTP call.

use serde::Deserialize;
use serde_json::{json, Value};

use super::http::post_json;

const SERVER: &str = "https://api.deepl.com";
const SERVER_FREE: &str = "https://api-free.deepl.com";

/// What `authenticationDeepLAuthKey` translates to prove a key works.
const CHECK_TEXT: &str = " ";
const CHECK_TARGET: &str = "EN-US";

#[derive(Debug, Deserialize)]
pub struct Request {
    pub auth_key: String,
    pub text: String,
    /// Absent means DeepL detects the language.
    #[serde(default)]
    pub source_lang: Option<String>,
    pub target_lang: String,
    /// Test seam and self-hosted proxies; normally chosen from the key.
    #[serde(default)]
    pub server_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Check {
    pub auth_key: String,
    #[serde(default)]
    pub server_url: Option<String>,
}

fn server_of(auth_key: &str, server_url: &Option<String>) -> String {
    match server_url.as_deref().filter(|url| !url.is_empty()) {
        Some(url) => url.trim_end_matches('/').to_string(),
        None if auth_key.ends_with(":fx") => SERVER_FREE.to_string(),
        None => SERVER.to_string(),
    }
}

/// A fully built call, kept apart from sending so it can be inspected.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
    pub body: Value,
}

pub fn build(request: &Request) -> Result<Call, String> {
    if request.auth_key.is_empty() {
        return Err("auth_key must not be empty".into());
    }
    if request.text.is_empty() {
        return Err("text must not be empty".into());
    }
    let target = request.target_lang.to_uppercase();
    if target == "EN" {
        return Err(r#"target_lang="EN" is deprecated, please use "EN-GB" or "EN-US" instead."#.into());
    }
    if target == "PT" {
        return Err(r#"target_lang="PT" is deprecated, please use "PT-PT" or "PT-BR" instead."#.into());
    }
    let mut body = json!({
        "target_lang": target,
        "text": [request.text],
        "show_billed_characters": true,
    });
    if let Some(source) = &request.source_lang {
        body["source_lang"] = Value::String(source.to_uppercase());
    }
    Ok(Call {
        url: format!("{}/v2/translate", server_of(&request.auth_key, &request.server_url)),
        headers: vec![("authorization", format!("DeepL-Auth-Key {}", request.auth_key))],
        body,
    })
}

fn parse_reply(reply: &Value) -> Result<String, String> {
    reply
        .pointer("/translations/0/text")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "reply has no translation".to_string())
}

pub async fn translate(request: Request) -> Result<String, String> {
    let call = build(&request)?;
    parse_reply(&post_json(&call.url, &call.headers, &call.body).await?)
}

/// True when DeepL accepts the key. Like the SDK flow it replaces, this spends
/// a (blank) translation rather than reading the usage endpoint.
pub async fn check(check: Check) -> Result<bool, String> {
    translate(Request {
        auth_key: check.auth_key,
        text: CHECK_TEXT.into(),
        source_lang: None,
        target_lang: CHECK_TARGET.into(),
        server_url: check.server_url,
    })
    .await
    .map(|_| true)
}
