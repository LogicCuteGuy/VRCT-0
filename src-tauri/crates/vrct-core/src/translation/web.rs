//! The public web translators, using each website's own session protocol.
//!
//! Language names are resolved by `languages` before entering this module.
//! Website tokens are fetched afresh; no provider credentials or tokens persist.
//! Protocol reference: https://github.com/UlionTse/translators and the providers'
//! live web clients. This is an independent Rust implementation, not vendored JS.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use hmac::{Hmac, Mac};
use md5::Md5;
use reqwest::{Client, RequestBuilder, Url};
use serde::Deserialize;
use serde_json::{json, Value};

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/132.0.0.0 Safari/537.36";
const MAX_BODY: usize = 8 * 1024 * 1024;

#[derive(Debug, Deserialize)]
pub struct Request {
    pub engine: String,
    pub text: String,
    /// Provider language code, e.g. `en`, `ja`, `zh-Hans`.
    pub source: String,
    pub target: String,
    /// Override the provider origin, for a local test server or a proxy.
    #[serde(default)]
    pub base_url: Option<String>,
}

pub fn handles(engine: &str) -> bool {
    matches!(engine, "Google" | "Bing" | "Papago")
}

struct Session {
    client: Client,
    base: Url,
}

enum Failure {
    Status(u16),
    Other(String),
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self::Other(message)
    }
}

impl Session {
    fn new(base: &str) -> Result<Self, String> {
        let base = Url::parse(base).map_err(|_| "invalid web translator origin")?;
        if !matches!(base.scheme(), "http" | "https")
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
        {
            return Err("invalid web translator origin".into());
        }
        let client = Client::builder()
            .cookie_store(true)
            .user_agent(UA)
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| "could not create web translator HTTP client")?;
        Ok(Self { client, base })
    }

    fn url(&self, path: &str) -> Result<Url, Failure> {
        self.base
            .join(path)
            .map_err(|_| Failure::Other("invalid web translator endpoint".into()))
    }

    async fn get(&self, url: Url) -> Result<String, Failure> {
        reply(
            self.client
                .get(url)
                .header("Accept-Language", "en-US,en;q=0.9"),
        )
        .await
    }

    async fn post(
        &self,
        url: Url,
        fields: &[(&str, String)],
        extra: &[(&str, String)],
    ) -> Result<String, Failure> {
        let mut encoding = Url::parse("http://form.invalid/").expect("constant URL");
        encoding
            .query_pairs_mut()
            .extend_pairs(fields.iter().map(|(key, value)| (*key, value.as_str())));
        let mut request = self
            .client
            .post(url)
            .header(
                "Content-Type",
                "application/x-www-form-urlencoded;charset=UTF-8",
            )
            .header("Origin", self.base.origin().ascii_serialization())
            .header("Referer", self.base.as_str())
            .header("Accept-Language", "en")
            .body(encoding.query().unwrap_or_default().to_owned());
        for (name, value) in extra {
            request = request.header(*name, value);
        }
        reply(request).await
    }
}

async fn reply(request: RequestBuilder) -> Result<String, Failure> {
    for attempt in 0..3 {
        let cloned = request
            .try_clone()
            .ok_or_else(|| Failure::Other("could not retry web request".into()))?;
        let mut response = cloned.send().await.map_err(|error| {
            Failure::Other(format!("web request failed: {}", error.without_url()))
        })?;
        let status = response.status();
        if !status.is_success() {
            if attempt < 2 && (matches!(status.as_u16(), 408 | 429) || status.is_server_error()) {
                tokio::time::sleep(Duration::from_millis(200 * (attempt + 1))).await;
                continue;
            }
            return Err(Failure::Status(status.as_u16()));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| Failure::Other(format!("web reply failed: {}", error.without_url())))?
        {
            if bytes.len() + chunk.len() > MAX_BODY {
                return Err(Failure::Other(
                    "web translator reply exceeds size limit".into(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        return String::from_utf8(bytes)
            .map_err(|_| Failure::Other("web translator reply is not UTF-8".into()));
    }
    unreachable!("last attempt returns")
}

/// One translation, bounded to thirty seconds including bootstrap and retries.
/// Authentication failures refresh the entire cookie/token session once.
pub async fn translate(request: Request) -> Result<String, String> {
    let base = match request.engine.as_str() {
        "Google" => "https://translate.google.com/",
        "Bing" => "https://www.bing.com/",
        "Papago" => "https://papago.naver.com/",
        _ => return Err(format!("unsupported web translator {:?}", request.engine)),
    };
    if request.text.trim().is_empty() {
        return Err("empty translation text".into());
    }
    let limit = if request.engine == "Google" {
        5000
    } else {
        1000
    };
    if request.text.chars().count() > limit {
        return Err(format!(
            "{} accepts at most {limit} characters per request",
            request.engine
        ));
    }
    if request.source.is_empty()
        || request.target.is_empty()
        || matches!(request.target.as_str(), "auto" | "auto-detect")
    {
        return Err("invalid web translation language".into());
    }
    if request.source == request.target {
        return Ok(request.text);
    }
    let work = async {
        for attempt in 0..2 {
            let session = Session::new(request.base_url.as_deref().unwrap_or(base))?;
            let result = match request.engine.as_str() {
                "Google" => google(&session, &request).await,
                "Bing" => bing(&session, &request).await,
                "Papago" => papago(&session, &request).await,
                _ => unreachable!(),
            };
            match result {
                Ok(text) if !text.trim().is_empty() => return Ok(text),
                Ok(_) => return Err("web translator returned empty text".into()),
                Err(Failure::Status(401 | 403)) if attempt == 0 => continue,
                Err(Failure::Status(status)) => {
                    return Err(format!("{} HTTP {status}", request.engine))
                }
                Err(Failure::Other(message)) => return Err(message),
            }
        }
        unreachable!("last attempt returns")
    };
    tokio::time::timeout(Duration::from_secs(30), work)
        .await
        .map_err(|_| "web translation timed out".to_string())?
}

/// Read the first JSON value after a JS assignment without executing JavaScript.
fn assignment(document: &str, marker: &str) -> Result<Value, Failure> {
    let tail = document
        .split_once(marker)
        .ok_or_else(|| Failure::Other(format!("web bootstrap missing {marker}")))?
        .1;
    let tail = tail
        .trim_start()
        .strip_prefix('=')
        .unwrap_or(tail)
        .trim_start();
    serde_json::Deserializer::from_str(tail)
        .into_iter::<Value>()
        .next()
        .ok_or_else(|| Failure::Other("empty web bootstrap".into()))?
        .map_err(|_| Failure::Other("invalid web bootstrap JSON".into()))
}

fn json(body: &str) -> Result<Value, Failure> {
    serde_json::from_str(body)
        .map_err(|_| Failure::Other("invalid web translator JSON reply".into()))
}

fn text_at(value: &Value, pointer: &str) -> Result<String, Failure> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| Failure::Other("web translator reply contains no translation".into()))
}

// Parse HTML attributes, including single/double quotes. No script execution.
fn attributes(tag: &str) -> Vec<(String, String)> {
    let mut remaining = tag.trim_start_matches('<');
    remaining = remaining.trim_start_matches(|c: char| !c.is_whitespace() && c != '>');
    let mut found = Vec::new();
    while !remaining.is_empty() {
        remaining = remaining.trim_start_matches(|c: char| c.is_whitespace() || c == '/');
        let end = remaining
            .find(|c: char| c.is_whitespace() || matches!(c, '=' | '>'))
            .unwrap_or(remaining.len());
        if end == 0 {
            break;
        }
        let name = remaining[..end].to_ascii_lowercase();
        remaining = remaining[end..].trim_start();
        if let Some(tail) = remaining.strip_prefix('=') {
            remaining = tail.trim_start();
            let quote = remaining.chars().next().unwrap_or('>');
            let value;
            if matches!(quote, '\'' | '"') {
                remaining = &remaining[1..];
                let end = remaining.find(quote).unwrap_or(remaining.len());
                value = remaining[..end].to_owned();
                remaining = &remaining[end..];
                if !remaining.is_empty() {
                    remaining = &remaining[1..];
                }
            } else {
                let end = remaining
                    .find(|c: char| c.is_whitespace() || c == '>')
                    .unwrap_or(remaining.len());
                value = remaining[..end].to_owned();
                remaining = &remaining[end..];
            }
            found.push((name, html_unescape(&value)));
        }
    }
    found
}

fn tags<'a>(html: &'a str, kind: &'a str) -> impl Iterator<Item = &'a str> {
    html.match_indices('<').filter_map(move |(start, _)| {
        let tail = &html[start..];
        let tag = tail
            .get(1..)?
            .split(|c: char| c.is_whitespace() || c == '>')
            .next()?;
        if !tag.eq_ignore_ascii_case(kind) {
            return None;
        }
        tail.find('>').map(|end| &tail[..=end])
    })
}

fn html_unescape(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

async fn google(session: &Session, request: &Request) -> Result<String, Failure> {
    let mut page = session.get(session.url("/")?).await?;
    if !page.contains("WIZ_global_data") {
        // Google's consent form may precede the translator bootstrap in Europe.
        let consent_form = tags(&page, "form").next().map(str::to_owned);
        if let Some(form) = consent_form {
            let attrs = attributes(&form);
            let action = attrs
                .iter()
                .find(|(key, _)| key == "action")
                .map(|(_, value)| value.as_str())
                .unwrap_or("");
            let action = session.url(action)?;
            if action.host_str() != Some("consent.google.com")
                && action.origin() != session.base.origin()
            {
                return Err(Failure::Other("unexpected Google consent origin".into()));
            }
            let fields: Vec<(String, String)> = tags(&page, "input")
                .filter_map(|tag| {
                    let attrs = attributes(tag);
                    if !attrs
                        .iter()
                        .any(|(key, value)| key == "type" && value == "hidden")
                    {
                        return None;
                    }
                    let name = attrs.iter().find(|(key, _)| key == "name")?.1.clone();
                    let value = attrs
                        .iter()
                        .find(|(key, _)| key == "value")
                        .map(|(_, value)| value.clone())
                        .unwrap_or_default();
                    Some((name, value))
                })
                .collect();
            page = session
                .post(
                    action,
                    &fields
                        .iter()
                        .map(|(key, value)| (key.as_str(), value.clone()))
                        .collect::<Vec<_>>(),
                    &[],
                )
                .await?;
            if !page.contains("WIZ_global_data") {
                page = session.get(session.url("/")?).await?;
            }
        }
    }
    let info = assignment(&page, "WIZ_global_data")?;
    let mut url = session.url("/_/TranslateWebserverUi/data/batchexecute")?;
    let mut query = url.query_pairs_mut();
    query
        .append_pair("rpcids", "MkEWBc")
        .append_pair("rt", "c")
        .append_pair("hl", "en")
        .append_pair("soc-app", "1")
        .append_pair("soc-platform", "1");
    for (key, field) in [("f.sid", "FdrFJe"), ("bl", "cfb2h")] {
        if let Some(value) = info.get(field).and_then(Value::as_str) {
            query.append_pair(key, value);
        }
    }
    drop(query);
    let inner = json!([[request.text, request.source, request.target, true], [1]]).to_string();
    let rpc = json!([[["MkEWBc", inner, null, "generic"]]]).to_string();
    let mut fields = vec![("f.req", rpc)];
    if let Some(token) = info.get("SNlM0e").and_then(Value::as_str) {
        fields.push(("at", token.to_owned()));
    }
    let body = session
        .post(url, &fields, &[("X-Same-Domain", "1".into())])
        .await?;
    google_reply(&body)
}

fn google_reply(body: &str) -> Result<String, Failure> {
    // batchexecute has an XSSI prelude and optional byte-length framed chunks.
    let body = body.strip_prefix(")]}'").unwrap_or(body).trim_start();
    for line in body.lines() {
        let Ok(frame) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let Some(rows) = frame.as_array() else {
            continue;
        };
        for row in rows {
            if row.get(1).and_then(Value::as_str) != Some("MkEWBc") {
                continue;
            }
            let payload = row
                .get(2)
                .and_then(Value::as_str)
                .ok_or_else(|| Failure::Other("Google returned no translation payload".into()))?;
            let data = json(payload)?;
            let sentences = data
                .pointer("/1/0/0/5")
                .filter(|value| value.as_array().is_some_and(|v| !v.is_empty()))
                .or_else(|| data.pointer("/1/0"))
                .and_then(Value::as_array)
                .ok_or_else(|| Failure::Other("unrecognized Google translation payload".into()))?;
            let pieces: Vec<&str> = sentences
                .iter()
                .filter_map(|sentence| sentence.get(0).and_then(Value::as_str))
                .filter(|s| !s.is_empty())
                .collect();
            if !pieces.is_empty() {
                return Ok(pieces.join(" "));
            }
        }
    }
    Err(Failure::Other(
        "Google reply contains no translation".into(),
    ))
}

async fn bing(session: &Session, request: &Request) -> Result<String, Failure> {
    let page = session.get(session.url("/translator")?).await?;
    let helper = assignment(&page, "params_AbusePreventionHelper")?;
    let key = helper
        .get(0)
        .ok_or_else(|| Failure::Other("Bing bootstrap has no key".into()))?;
    let key = key
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| key.to_string());
    let token = helper
        .get(1)
        .and_then(Value::as_str)
        .ok_or_else(|| Failure::Other("Bing bootstrap has no token".into()))?;
    let ig = page
        .split_once("IG:\"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(ig, _)| ig)
        .ok_or_else(|| Failure::Other("Bing bootstrap has no IG".into()))?;
    let iid = tags(&page, "div")
        .find_map(|tag| {
            let attrs = attributes(tag);
            if !attrs
                .iter()
                .any(|(key, value)| key == "id" && value == "tta_outGDCont")
            {
                return None;
            }
            attrs
                .into_iter()
                .find(|(key, _)| key == "data-iid")
                .map(|(_, value)| value)
        })
        .ok_or_else(|| Failure::Other("Bing bootstrap has no IID".into()))?;
    let mut url = session.url("/ttranslatev3")?;
    url.query_pairs_mut()
        .append_pair("isVertical", "1")
        .append_pair("IG", ig)
        .append_pair("IID", &iid);
    let source = if request.source == "auto" {
        "auto-detect"
    } else {
        &request.source
    };
    let fields = [
        ("text", request.text.clone()),
        ("fromLang", source.to_owned()),
        ("to", request.target.clone()),
        ("tryFetchingGenderDebiasedTranslations", "true".into()),
        ("key", key),
        ("token", token.into()),
    ];
    let body = session.post(url, &fields, &[]).await?;
    if let Ok(value) = serde_json::from_str::<Value>(&body) {
        return text_at(&value, "/0/translations/0/text");
    }
    // Some Bing regions return the translation in a textarea rather than JSON.
    body.rsplit_once("</textarea>")
        .and_then(|(before, _)| before.rsplit_once('>'))
        .map(|(_, text)| html_unescape(text))
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| Failure::Other("unrecognized Bing translation reply".into()))
}

async fn papago(session: &Session, request: &Request) -> Result<String, Failure> {
    let page = session.get(session.url("/")?).await?;
    if page.contains("/_next/") || page.contains("/api/text/translation") {
        let mut source = request.source.clone();
        if source == "auto" {
            let body = session
                .post(
                    session.url("/api/langs/dect")?,
                    &[("query", request.text.clone())],
                    &[],
                )
                .await?;
            source = text_at(&json(&body)?, "/langCode")?;
            if source == "py" {
                source = "zh-CN".into();
            }
            if source == "unk" {
                return Err(Failure::Other(
                    "Papago could not detect source language".into(),
                ));
            }
        }
        let fields = [
            ("source", source),
            ("target", request.target.clone()),
            ("text", request.text.clone()),
            ("dict", "true".into()),
            ("dictDisplay", "30".into()),
            ("useGlossary", "false".into()),
            ("honorific", "false".into()),
        ];
        let body = session
            .post(session.url("/api/text/translation")?, &fields, &[])
            .await?;
        return text_at(&json(&body)?, "/translatedText");
    }
    papago_legacy(session, request, &page).await
}

fn papago_key(script: &str) -> Option<String> {
    for marker in ["AUTH_KEY:", "AUTH_KEY=", "AUTH_KEY\":", "authKey:"] {
        if let Some((_, tail)) = script.split_once(marker) {
            let tail = tail.trim_start();
            let quote = tail.chars().next()?;
            if matches!(quote, '\'' | '"') {
                return tail[1..]
                    .split_once(quote)
                    .map(|(key, _)| key.to_owned())
                    .filter(|key| !key.is_empty());
            }
        }
    }
    // Historical clients inline the versioned HMAC key rather than naming it.
    for (at, _) in script.match_indices('v') {
        let candidate: String = script[at..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_'))
            .collect();
        let Some((version, suffix)) = candidate.split_once('_') else {
            continue;
        };
        if version.strip_prefix('v')?.split('.').count() == 3
            && version[1..]
                .split('.')
                .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
            && suffix.len() >= 8
            && suffix.chars().all(|c| c.is_ascii_hexdigit())
        {
            return Some(candidate);
        }
    }
    None
}

fn papago_auth(url: &Url, key: &str, device: &str) -> Result<Vec<(&'static str, String)>, Failure> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Failure::Other("system clock precedes Unix epoch".into()))?
        .as_millis()
        .to_string();
    let mut mac = Hmac::<Md5>::new_from_slice(key.as_bytes())
        .map_err(|_| Failure::Other("invalid Papago signing key".into()))?;
    mac.update(format!("{device}\n{url}\n{timestamp}").as_bytes());
    let signature = STANDARD.encode(mac.finalize().into_bytes());
    Ok(vec![
        ("timestamp", timestamp),
        ("authorization", format!("PPG {device}:{signature}")),
        ("device-type", "pc".into()),
        ("x-apigw-partnerid", "papago".into()),
    ])
}

async fn papago_legacy(
    session: &Session,
    request: &Request,
    page: &str,
) -> Result<String, Failure> {
    let mut scripts: Vec<String> = tags(page, "script")
        .filter_map(|tag| {
            attributes(tag)
                .into_iter()
                .find(|(key, _)| key == "src")
                .map(|(_, value)| value)
        })
        .collect();
    scripts.sort_by_key(|path| !(path.contains("home.") || path.contains("main.")));
    let mut key = papago_key(page);
    if key.is_none() {
        for script in scripts.into_iter().take(8) {
            let url = session.url(&script)?;
            if url.origin() != session.base.origin() {
                continue;
            }
            let body = session.get(url).await?;
            if let Some(found) = papago_key(&body) {
                key = Some(found);
                break;
            }
        }
    }
    let key = key.ok_or_else(|| Failure::Other("Papago bootstrap has no signing key".into()))?;
    let mut random = [0u8; 16];
    getrandom::fill(&mut random)
        .map_err(|_| Failure::Other("could not generate Papago device ID".into()))?;
    random[6] = (random[6] & 0x0f) | 0x40;
    random[8] = (random[8] & 0x3f) | 0x80;
    let device = format!(
        "{}-{}-{}-{}-{}",
        hex::encode(&random[..4]),
        hex::encode(&random[4..6]),
        hex::encode(&random[6..8]),
        hex::encode(&random[8..10]),
        hex::encode(&random[10..])
    );
    let mut source = request.source.clone();
    if source == "auto" {
        let url = session.url("/apis/langs/dect")?;
        let body = session
            .post(
                url.clone(),
                &[("query", request.text.clone())],
                &papago_auth(&url, &key, &device)?,
            )
            .await?;
        source = text_at(&json(&body)?, "/langCode")?;
    }
    let url = session.url("/apis/n2mt/translate")?;
    let fields = [
        ("deviceId", device.clone()),
        ("text", request.text.clone()),
        ("source", source),
        ("target", request.target.clone()),
        ("locale", "en".into()),
        ("dict", "true".into()),
        ("dictDisplay", "30".into()),
        ("honorific", "false".into()),
        ("instant", "false".into()),
        ("paging", "false".into()),
    ];
    let body = session
        .post(url.clone(), &fields, &papago_auth(&url, &key, &device)?)
        .await?;
    text_at(&json(&body)?, "/translatedText")
}
