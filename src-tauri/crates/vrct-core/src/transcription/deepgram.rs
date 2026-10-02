//! Deepgram's pre-recorded speech-to-text API (`/v1/listen`), as `DeepgramProvider` and
//! `transcription_deepgram.py` used it.
//!
//! One call settles a phrase (Deepgram identifies the language itself, or is told it), so a
//! result is always definitive. Like Python's `requests`, a request is made once and not retried.

use std::time::Duration;

use serde_json::Value;

use super::cloud::{api_error, code, other, CloudRecognizer};
use super::clip::wav_for_upload;
use super::languages;
use super::phrases::{Recognition, RecognizeError, Request};
use crate::settings::pyconv::{py_float, py_str};
use crate::translation::http::{get_reply, post_bytes};

pub const BASE_URL: &str = "https://api.deepgram.com/v1";
/// Python waited up to 10 s to connect and 60 s for the answer.
const WAIT: Duration = Duration::from_secs(60);

pub struct DeepgramProvider {
    api_key: String,
    model: String,
    model_languages: Vec<String>,
    base_url: String,
    wait: Duration,
}

impl DeepgramProvider {
    pub fn new(api_key: &str, model: &str, model_languages: Vec<String>) -> Self {
        DeepgramProvider { api_key: api_key.into(), model: model.into(), model_languages, base_url: BASE_URL.into(), wait: WAIT }
    }

    /// How long to wait for an answer.
    pub fn with_wait(mut self, wait: Duration) -> Self {
        self.wait = wait;
        self
    }

    /// For tests: another server in place of Deepgram's.
    pub fn with_base_url(mut self, base_url: &str) -> Self {
        self.base_url = base_url.trim_end_matches('/').to_string();
        self
    }
}

/// `"en-US"` to `"en"`.
fn base_language_code(code: &str) -> String {
    code.split('-').next().unwrap_or_default().to_lowercase()
}

/// The code, as the model itself lists it, that fits a language and country best: the table's
/// `Google` code if the model lists it (any case), else a listed code with the same base as the
/// table's `Whisper` code. `None` when neither fits, when the model lists nothing, or when it lists
/// `multi` (which names no particular language).
pub fn resolve_language_code(language: &str, country: &str, model_languages: &[String]) -> Option<String> {
    let entry = languages::entry(language, country)?;
    // lowercased code -> the code as listed; the first listing of a code wins.
    let mut listed: Vec<(String, &str)> = Vec::new();
    for code in model_languages.iter().filter(|c| !c.is_empty()) {
        let lower = code.to_lowercase();
        if !listed.iter().any(|(known, _)| *known == lower) {
            listed.push((lower, code));
        }
    }
    if listed.is_empty() || listed.iter().any(|(lower, _)| lower == "multi") {
        return None;
    }

    let text = |key: &str| entry.get(key).and_then(Value::as_str).filter(|s| !s.is_empty());
    if let Some(google) = text("Google") {
        let google = google.to_lowercase();
        if let Some((_, original)) = listed.iter().find(|(lower, _)| *lower == google) {
            return Some((*original).to_string());
        }
    }
    let base = base_language_code(text("Whisper")?);
    listed.iter().find(|(lower, _)| base_language_code(lower) == base).map(|(_, original)| (*original).to_string())
}

/// Whether a model's language list covers a language and country; `multi` covers everything.
pub fn is_language_supported(language: &str, country: &str, model_languages: &[String]) -> bool {
    if model_languages.iter().any(|code| code.to_lowercase() == "multi") {
        return true;
    }
    resolve_language_code(language, country, model_languages).is_some()
}

fn status_code(status: u16) -> &'static str {
    match status {
        401 | 403 => code::AUTH_FAILED,
        429 => code::RATE_LIMITED,
        _ => code::SERVER_ERROR,
    }
}

/// `payload[key]` the way Python indexes it: a missing key is `Ok(None)` (a caught `KeyError`), a value
/// that cannot be indexed is an error (an uncaught `TypeError`).
fn index_key<'a>(value: &'a Value, key: &str) -> Result<Option<&'a Value>, RecognizeError> {
    match value {
        Value::Object(map) => Ok(map.get(key)),
        _ => Err(other("TypeError")),
    }
}

fn index_first(value: &Value) -> Result<Option<&Value>, RecognizeError> {
    match value {
        Value::Array(items) => Ok(items.first()),
        Value::Object(_) => Ok(None),
        _ => Err(other("TypeError")),
    }
}

/// Python truthiness of a JSON value.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `payload["results"]["channels"][0]["alternatives"][0]`; `None` where a key or an index is missing
/// (a caught `KeyError` / `IndexError` in Python).
fn first_alternative(payload: &Value) -> Result<Option<&Value>, RecognizeError> {
    let Some(results) = index_key(payload, "results")? else { return Ok(None) };
    let Some(channels) = index_key(results, "channels")? else { return Ok(None) };
    let Some(channel) = index_first(channels)? else { return Ok(None) };
    let Some(alternatives) = index_key(channel, "alternatives")? else { return Ok(None) };
    index_first(alternatives)
}

/// The recognition in a `/v1/listen` reply; empty where the reply has no alternative to take.
fn recognition_of(payload: &Value) -> Result<Recognition, RecognizeError> {
    let empty = || Recognition { text: String::new(), confidence: 0.0, definitive: false };
    let Some(alternative) = first_alternative(payload)? else { return Ok(empty()) };
    let Value::Object(alternative) = alternative else { return Err(other("AttributeError")) };

    let Some(transcript) = alternative.get("transcript").filter(|v| truthy(v)) else { return Ok(empty()) };
    let text = py_str(transcript);
    if text.is_empty() {
        return Ok(empty());
    }
    let confidence = match alternative.get("confidence").filter(|v| truthy(v)) {
        None => 0.0,
        Some(value) => py_float(value).ok_or_else(|| other(if value.is_string() { "ValueError" } else { "TypeError" }))?,
    };
    Ok(Recognition { text, confidence, definitive: true })
}

impl CloudRecognizer for DeepgramProvider {
    async fn recognize(&self, request: &Request<'_>) -> Result<Recognition, RecognizeError> {
        let wav = wav_for_upload(request.pcm, request.format).map_err(|_| other("error"))?;

        let mut query = vec![("model", self.model.clone())];
        let resolved = if request.force_language {
            resolve_language_code(request.language, request.country, &self.model_languages)
        } else {
            None
        };
        match resolved {
            Some(language) => query.push(("language", language)),
            None => query.push(("detect_language", "true".to_string())),
        }

        let headers = [("Authorization", format!("Token {}", self.api_key))];
        let reply = post_bytes(&format!("{}/listen", self.base_url), &headers, &query, "audio/wav", &wav, self.wait, 1)
            .await
            .map_err(|error| api_error(if error.timed_out { code::TIMEOUT } else { code::SERVER_ERROR }))?;
        if reply.status != 200 {
            return Err(api_error(status_code(reply.status)));
        }
        let payload: Value = serde_json::from_slice(&reply.body).map_err(|_| other("JSONDecodeError"))?;
        recognition_of(&payload)
    }
}

/// Whether `api_key` can list the models.
pub async fn check_api_key(base_url: &str, api_key: &str) -> bool {
    let headers = [("Authorization", format!("Token {api_key}"))];
    matches!(get_reply(&format!("{base_url}/models"), &headers, WAIT, 1).await, Ok(reply) if reply.status == 200)
}

/// A speech-to-text model that handles recorded audio, with the languages it lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    pub name: String,
    pub languages: Vec<String>,
}

/// The batch-capable speech-to-text models, once each, by name; empty where the list cannot be had.
pub async fn models_detailed(base_url: &str, api_key: &str) -> Vec<Model> {
    let headers = [("Authorization", format!("Token {api_key}"))];
    let Ok(reply) = get_reply(&format!("{base_url}/models"), &headers, WAIT, 1).await else { return Vec::new() };
    if reply.status >= 400 {
        return Vec::new();
    }
    let Ok(payload) = serde_json::from_slice::<Value>(&reply.body) else { return Vec::new() };
    let mut seen: Vec<String> = Vec::new();
    let mut models = Vec::new();
    for entry in payload.get("stt").and_then(Value::as_array).into_iter().flatten() {
        let Some(name) = entry.get("name").and_then(Value::as_str).filter(|n| !n.is_empty()) else { continue };
        if entry.get("batch") != Some(&Value::Bool(true)) || seen.iter().any(|s| s == name) {
            continue;
        }
        seen.push(name.to_string());
        let languages = entry
            .get("languages")
            .and_then(Value::as_array)
            .map(|list| list.iter().filter_map(|l| l.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        models.push(Model { name: name.to_string(), languages });
    }
    models.sort_by(|a, b| a.name.cmp(&b.name));
    models
}

pub async fn models(base_url: &str, api_key: &str) -> Vec<String> {
    models_detailed(base_url, api_key).await.into_iter().map(|m| m.name).collect()
}
