//! OpenAI-style speech-to-text (`POST /audio/transcriptions`): Groq, OpenAI and a custom server,
//! which differ only in base URL, key, model and the name of their column in the language table.
//! A port of `OpenAICompatibleTranscriptionProvider` and `transcription_openai_compatible.py`.
//!
//! The Python OpenAI client retried a failed request twice; so does the HTTP policy used here.

use std::time::Duration;

use serde_json::Value;

use super::cloud::{api_error, code, other, CloudRecognizer};
use super::clip::wav_for_upload;
use super::languages;
use super::phrases::{Recognition, RecognizeError, Request};
use crate::translation::http::{get_reply, post_bytes};

/// The three engine names Python's `TRANSCRIPTION_API_ENGINES` lists.
pub const ENGINES: [&str; 3] = ["Groq_Whisper", "OpenAI_Whisper", "Custom_Whisper"];
/// Keywords that tell a speech-to-text model from a chat or embedding one in a provider's list.
pub const MODEL_KEYWORDS: [&str; 2] = ["whisper", "transcribe"];

const WAIT: Duration = Duration::from_secs(60);
const LIST_WAIT: Duration = Duration::from_secs(20);
const ATTEMPTS: u32 = 3;
const BOUNDARY: &str = "----vrct-transcription-7f3a9c2e51d84b60";

pub struct OpenAiCompatible {
    api_key: String,
    base_url: String,
    model: String,
    /// Which column of the language table to read: `Groq_Whisper`, `OpenAI_Whisper` or `Custom_Whisper`.
    engine_name: String,
    wait: Duration,
}

impl OpenAiCompatible {
    pub fn new(api_key: &str, base_url: &str, model: &str, engine_name: &str) -> Self {
        OpenAiCompatible {
            api_key: api_key.into(),
            base_url: base_url.trim_end_matches('/').into(),
            model: model.into(),
            engine_name: engine_name.into(),
            wait: WAIT,
        }
    }

    /// How long to wait for an answer.
    pub fn with_wait(mut self, wait: Duration) -> Self {
        self.wait = wait;
        self
    }
}

/// The `multipart/form-data` body the API takes: the settings as fields and the audio as a file.
fn multipart_body(fields: &[(&str, &str)], file_name: &str, file_type: &str, file: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(file.len() + 512);
    for (name, value) in fields {
        body.extend_from_slice(
            format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes(),
        );
    }
    body.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{file_name}\"\r\nContent-Type: {file_type}\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(file);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    body
}

/// A failed reply as the Python provider classified it: 401 and 429 have their own codes, every other
/// failure (including a refused connection) is a server error, and only a wait that ran out is a timeout.
fn status_code(status: u16) -> &'static str {
    match status {
        401 => code::AUTH_FAILED,
        429 => code::RATE_LIMITED,
        _ => code::SERVER_ERROR,
    }
}

/// `x or ""` for a JSON string value.
fn text_of(value: Option<&Value>) -> String {
    value.and_then(Value::as_str).unwrap_or_default().to_string()
}

/// One segment's number, or the `AttributeError` Python raised when the server left it out.
fn number(segment: &Value, key: &str) -> Result<f64, RecognizeError> {
    segment.get(key).and_then(Value::as_f64).ok_or_else(|| other("AttributeError"))
}

impl CloudRecognizer for OpenAiCompatible {
    async fn recognize(&self, request: &Request<'_>) -> Result<Recognition, RecognizeError> {
        let wav = wav_for_upload(request.pcm, request.format).map_err(|_| other("error"))?;
        let table_code = || languages::code(request.language, request.country, &self.engine_name).ok_or_else(|| other("KeyError"));
        let source_language = if request.force_language { Some(table_code()?) } else { None };

        let mut fields = vec![("model", self.model.as_str())];
        if let Some(language) = source_language {
            fields.push(("language", language));
        }
        fields.push(("response_format", "verbose_json"));
        fields.push(("temperature", "0.0"));
        let body = multipart_body(&fields, "audio.wav", "audio/wav", &wav);

        let headers = [("Authorization", format!("Bearer {}", self.api_key))];
        let content_type = format!("multipart/form-data; boundary={BOUNDARY}");
        let reply = post_bytes(&format!("{}/audio/transcriptions", self.base_url), &headers, &[], &content_type, &body, self.wait, ATTEMPTS)
            .await
            .map_err(|error| api_error(if error.timed_out { code::TIMEOUT } else { code::SERVER_ERROR }))?;
        if !(200..300).contains(&reply.status) {
            return Err(api_error(status_code(reply.status)));
        }

        let empty = || Ok(Recognition { text: String::new(), confidence: 0.0, definitive: false });
        // A server that answers in plain text has no segments and no `text` attribute: nothing recognised.
        let Ok(response) = serde_json::from_slice::<Value>(&reply.body) else { return empty() };

        let segments = response.get("segments").and_then(Value::as_array).filter(|s| !s.is_empty());
        let mut accepted: Vec<f64> = Vec::new();
        let mut text = String::new();
        match segments {
            Some(segments) => {
                for segment in segments {
                    let (log_prob, no_speech) = (number(segment, "avg_logprob")?, number(segment, "no_speech_prob")?);
                    if log_prob < request.avg_logprob || no_speech > request.no_speech_prob {
                        continue;
                    }
                    text.push_str(segment.get("text").and_then(Value::as_str).unwrap_or_default());
                    accepted.push(log_prob);
                }
            }
            // No per-segment figures to filter by (a server that ignores `verbose_json`): take the text.
            None => text = text_of(response.get("text")),
        }
        if text.is_empty() {
            return empty();
        }

        // The mean log-probability as a probability, standing in for the language probability a local
        // Whisper model reports. Summed left to right, as Python 3.11 does.
        let confidence = if accepted.is_empty() {
            0.5
        } else {
            let mut sum = 0.0;
            for log_prob in &accepted {
                sum += log_prob;
            }
            (sum / accepted.len() as f64).exp()
        };

        let definitive = request.force_language
            || match response.get("language").and_then(Value::as_str) {
                Some(detected) => detected == table_code()?,
                None => false,
            };
        Ok(Recognition { text, confidence, definitive })
    }
}

fn is_success(status: u16) -> bool {
    (200..300).contains(&status)
}

/// Whether the key can list the server's models; the stand-in for "can transcribe", which only a real
/// clip could tell.
pub async fn check_api_key(base_url: &str, api_key: &str) -> bool {
    list_models(base_url, api_key).await.is_ok()
}

async fn list_models(base_url: &str, api_key: &str) -> Result<Vec<String>, String> {
    let headers = [("Authorization", format!("Bearer {api_key}"))];
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let reply = get_reply(&url, &headers, LIST_WAIT, ATTEMPTS).await.map_err(|e| e.message)?;
    if !is_success(reply.status) {
        return Err(format!("HTTP {}", reply.status));
    }
    let payload: Value = serde_json::from_slice(&reply.body).map_err(|e| format!("bad reply: {e}"))?;
    let ids = payload.get("data").and_then(Value::as_array).ok_or("bad reply: no model list")?;
    Ok(ids.iter().filter_map(|m| m.get("id").and_then(Value::as_str).map(str::to_string)).collect())
}

/// The server's model ids, sorted; with `keyword_filter`, only those whose lower-cased id contains one.
pub async fn available_models(base_url: &str, api_key: &str, keyword_filter: Option<&[&str]>) -> Result<Vec<String>, String> {
    let mut models = list_models(base_url, api_key).await?;
    if let Some(keywords) = keyword_filter.filter(|k| !k.is_empty()) {
        models.retain(|id| keywords.iter().any(|keyword| id.to_lowercase().contains(keyword)));
    }
    models.sort();
    Ok(models)
}
