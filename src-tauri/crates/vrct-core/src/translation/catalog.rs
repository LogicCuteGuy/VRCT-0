//! Key checks and model lists for the LLM engines.
//!
//! Each function reproduces what the Python client module does for the same
//! engine, including its quirks: which models are filtered out, how Gemini's
//! list is paged, OpenRouter's separate key endpoint. Python keeps the
//! authenticated key and the chosen model; it passes them in with each call.
//!
//! Results follow Python's exception behaviour so the callers there change
//! nothing: where Python lets an error escape, so does `Err` here; where it
//! turns failure into `False` or `[]`, the Python side does that conversion.
//! The local servers (LM Studio, Ollama) are the exception, answering `false`
//! or `[]` themselves when nothing is listening.

use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use super::http::{get_json, get_json_once, get_status_once};
use super::llm::{GEMINI_BASE, OLLAMA_BASE, OPENAI_BASE};

const GROQ_BASE: &str = "https://api.groq.com/openai/v1";
const OPENROUTER_BASE: &str = "https://openrouter.ai/api/v1";
const PLAMO_BASE: &str = "https://api.platform.preferredai.jp/v1";

/// Python waits 0.2 s for these. Loopback needs a little more headroom than
/// that on Windows, where `localhost` can resolve to an address nothing is on.
const LOCAL_WAIT: Duration = Duration::from_secs(1);
/// Ollama's model list had no timeout in Python; a loaded machine can be slow.
const OLLAMA_LIST_WAIT: Duration = Duration::from_secs(10);
/// OpenRouter's key endpoint.
const KEY_ENDPOINT_WAIT: Duration = Duration::from_secs(10);

#[derive(Debug, Deserialize)]
pub struct Target {
    /// Key of Python's `translation_lang`: `OpenAI_API`, `Ollama`, ...
    pub engine: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    OpenAi,
    OpenAiCompatible,
    Groq,
    OpenRouter,
    Plamo,
    Gemini,
    LmStudio,
    Ollama,
}

fn kind_of(engine: &str) -> Result<Kind, String> {
    Ok(match engine {
        "OpenAI_API" => Kind::OpenAi,
        "OpenAI_Compatible" => Kind::OpenAiCompatible,
        "Groq_API" => Kind::Groq,
        "OpenRouter_API" => Kind::OpenRouter,
        "Plamo_API" => Kind::Plamo,
        "Gemini_API" => Kind::Gemini,
        "LMStudio" => Kind::LmStudio,
        "Ollama" => Kind::Ollama,
        other => return Err(format!("unknown LLM engine {other:?}")),
    })
}

fn present(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|value| !value.is_empty())
}

fn join(base: &str, path: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), path)
}

fn base_of(kind: Kind, target: &Target) -> Option<String> {
    let fallback = match kind {
        Kind::OpenAi | Kind::OpenAiCompatible => Some(OPENAI_BASE),
        Kind::Groq => Some(GROQ_BASE),
        Kind::OpenRouter => Some(OPENROUTER_BASE),
        Kind::Plamo => Some(PLAMO_BASE),
        Kind::Gemini => Some(GEMINI_BASE),
        Kind::Ollama => Some(OLLAMA_BASE),
        // LM Studio has no default: the user points at their server.
        Kind::LmStudio => None,
    };
    present(&target.base_url).or(fallback).map(str::to_string)
}

fn auth_headers(kind: Kind, key: &str) -> Vec<(&'static str, String)> {
    match kind {
        Kind::Gemini => vec![("x-goog-api-key", key.to_string())],
        _ => vec![("authorization", format!("Bearer {key}"))],
    }
}

/// True when the engine accepts the key (or, for the local servers, answers).
pub async fn auth_check(target: Target) -> Result<bool, String> {
    let kind = kind_of(&target.engine)?;
    let Some(base) = base_of(kind, &target) else { return Ok(false) };
    match kind {
        // OpenRouter's SDK-less check: the key endpoint, 200 or not.
        Kind::OpenRouter => {
            let key = present(&target.api_key).unwrap_or_default();
            let status =
                get_status_once(&join(&base, "auth/key"), &auth_headers(kind, key), KEY_ENDPOINT_WAIT).await?;
            Ok(status == 200)
        }
        // The rest list models and call that a valid key; a refusal is an error.
        Kind::OpenAi | Kind::OpenAiCompatible | Kind::Groq | Kind::Plamo | Kind::Gemini => {
            let key = present(&target.api_key).ok_or("no API key")?;
            get_json(&join(&base, "models"), &auth_headers(kind, key)).await.map(|_| true)
        }
        Kind::LmStudio => Ok(local_status(&join(&base, "models")).await == Some(200)),
        Kind::Ollama => Ok(local_status(&base).await == Some(200)),
    }
}

async fn local_status(url: &str) -> Option<u16> {
    get_status_once(url, &[], LOCAL_WAIT).await.ok()
}

/// Models the engine offers for translation, sorted like Python sorts them.
pub async fn models(target: Target) -> Result<Vec<String>, String> {
    let kind = kind_of(&target.engine)?;
    let Some(base) = base_of(kind, &target) else { return Ok(Vec::new()) };
    let mut found = match kind {
        Kind::LmStudio => match get_json_once(&join(&base, "models"), &[], LOCAL_WAIT).await {
            Ok(reply) => ids(&reply, "data", "id"),
            Err(_) => Vec::new(),
        },
        Kind::Ollama => {
            if local_status(&base).await != Some(200) {
                return Ok(Vec::new());
            }
            match get_json_once(&join(&base, "api/tags"), &[], OLLAMA_LIST_WAIT).await {
                Ok(reply) => ids(&reply, "models", "name"),
                Err(_) => Vec::new(),
            }
        }
        Kind::Gemini => {
            let key = present(&target.api_key).ok_or("no API key")?;
            gemini_models(&base, key).await?
        }
        Kind::OpenAi | Kind::OpenAiCompatible | Kind::Groq | Kind::OpenRouter | Kind::Plamo => {
            let key = present(&target.api_key).ok_or("no API key")?;
            let reply = get_json(&join(&base, "models"), &auth_headers(kind, key)).await?;
            openai_models(kind, &reply)?
        }
    };
    found.sort();
    Ok(found)
}

/// The string at `field` of every element of `reply[list]`.
fn ids(reply: &Value, list: &str, field: &str) -> Vec<String> {
    reply
        .get(list)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get(field).and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Models that are not for text translation, matched as substrings.
const NOT_TEXT: &[&str] = &["whisper", "embedding", "image", "tts", "audio", "search", "transcribe", "diarize", "vision"];
/// OpenAI-compatible endpoints get a few more, but no `gpt-` requirement.
const NOT_TEXT_COMPATIBLE: &[&str] = &[
    "whisper", "embedding", "image", "tts", "audio", "search", "transcribe", "diarize", "vision", "dall-e",
    "moderation", "rerank",
];
const NOT_TEXT_GEMINI: &[&str] = &["audio", "image", "veo", "tts", "robotics", "computer-use"];

fn mentions(id: &str, words: &[&str]) -> bool {
    words.iter().any(|word| id.contains(word))
}

fn openai_models(kind: Kind, reply: &Value) -> Result<Vec<String>, String> {
    let items = reply.get("data").and_then(Value::as_array).ok_or("reply has no model list")?;
    let mut allowed = Vec::new();
    for item in items {
        let Some(id) = item.get("id").and_then(Value::as_str) else { continue };
        let keep = match kind {
            // Only GPT models, and fine-tunes of them; the keywords are matched as written.
            Kind::OpenAi => {
                let root = item.get("root").and_then(Value::as_str).unwrap_or_default();
                !mentions(id, NOT_TEXT) && (id.starts_with("gpt-") || (id.starts_with("ft:") && root.starts_with("gpt-")))
            }
            Kind::OpenAiCompatible => !mentions(&id.to_lowercase(), NOT_TEXT_COMPATIBLE),
            Kind::Groq | Kind::OpenRouter => !mentions(&id.to_lowercase(), NOT_TEXT),
            Kind::Plamo => true,
            _ => unreachable!("not an OpenAI-style list"),
        };
        if keep {
            allowed.push(id.to_string());
        }
    }
    Ok(allowed)
}

/// Gemini pages its list; Python's SDK walks every page.
async fn gemini_models(base: &str, key: &str) -> Result<Vec<String>, String> {
    let headers = auth_headers(Kind::Gemini, key);
    let mut allowed = Vec::new();
    let mut page_token: Option<String> = None;
    loop {
        let mut url = join(base, "models");
        if let Some(token) = &page_token {
            url.push_str("?pageToken=");
            url.push_str(&encode_query(token));
        }
        let reply = get_json(&url, &headers).await?;
        for item in reply.get("models").and_then(Value::as_array).into_iter().flatten() {
            let Some(name) = item.get("name").and_then(Value::as_str) else { continue };
            let lower = name.to_lowercase();
            let generates = item
                .get("supportedGenerationMethods")
                .and_then(Value::as_array)
                .is_some_and(|methods| methods.iter().any(|method| method.as_str() == Some("generateContent")));
            if (lower.contains("gemini") || lower.contains("gemma")) && generates && !mentions(name, NOT_TEXT_GEMINI) {
                allowed.push(name.replace("models/", ""));
            }
        }
        match reply.get("nextPageToken").and_then(Value::as_str).filter(|token| !token.is_empty()) {
            Some(token) => page_token = Some(token.to_string()),
            None => return Ok(allowed),
        }
    }
}

fn encode_query(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (byte as char).to_string(),
            other => format!("%{other:02X}"),
        })
        .collect()
}
