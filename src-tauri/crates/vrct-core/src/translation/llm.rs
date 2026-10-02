//! One translation request to an LLM provider.
//!
//! The Python clients all go through langchain: OpenAI, Groq, OpenRouter,
//! Plamo, LM Studio and "OpenAI compatible" speak the OpenAI chat-completions
//! protocol, Ollama and Gemini have their own. The system prompt comes from
//! `prompt`, so every engine asks the model exactly what Python asked.

use serde::Deserialize;
use serde_json::{json, Value};

use super::http::post_json;
use super::prompt::{py_strip, reply_text, system_prompt};

const OPENAI_BASE: &str = "https://api.openai.com/v1";
const OLLAMA_BASE: &str = "http://localhost:11434";
const GEMINI_BASE: &str = "https://generativelanguage.googleapis.com/v1beta";
/// langchain-google-genai's default `temperature`; the others send none.
const GEMINI_TEMPERATURE: f64 = 0.7;

#[derive(Debug, Deserialize)]
pub struct Request {
    /// Key of Python's `translation_lang`: `OpenAI_API`, `Ollama`, ...
    pub engine: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    pub model: String,
    pub text: String,
    pub input_lang: String,
    pub output_lang: String,
    #[serde(default)]
    pub history: Vec<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wire {
    OpenAi,
    Ollama,
    Gemini,
}

fn wire_of(engine: &str) -> Option<Wire> {
    match engine {
        "OpenAI_API" | "OpenAI_Compatible" | "Groq_API" | "OpenRouter_API" | "Plamo_API" | "LMStudio" => {
            Some(Wire::OpenAi)
        }
        "Ollama" => Some(Wire::Ollama),
        "Gemini_API" => Some(Wire::Gemini),
        _ => None,
    }
}

/// A fully built HTTP call, kept apart from sending so it can be inspected.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    wire: Wire,
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
    pub body: Value,
}

fn join(base: &str, path: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), path)
}

fn present(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|value| !value.is_empty())
}

pub fn build(request: &Request) -> Result<Call, String> {
    let wire = wire_of(&request.engine).ok_or_else(|| format!("unknown LLM engine {:?}", request.engine))?;
    if request.model.is_empty() {
        return Err("no model selected".into());
    }
    let system = system_prompt(&request.engine, &request.input_lang, &request.output_lang, &request.history)?;
    let base = present(&request.base_url);
    Ok(match wire {
        Wire::OpenAi => {
            let key = present(&request.api_key).ok_or("no API key")?;
            Call {
                wire,
                url: join(base.unwrap_or(OPENAI_BASE), "chat/completions"),
                headers: vec![("authorization", format!("Bearer {key}"))],
                body: json!({
                    "model": request.model,
                    "stream": false,
                    "messages": [
                        {"role": "system", "content": system},
                        {"role": "user", "content": request.text},
                    ],
                }),
            }
        }
        Wire::Ollama => Call {
            wire,
            url: join(base.unwrap_or(OLLAMA_BASE), "api/chat"),
            headers: Vec::new(),
            body: json!({
                "model": request.model,
                "stream": false,
                "messages": [
                    {"role": "system", "content": system},
                    {"role": "user", "content": request.text},
                ],
            }),
        },
        Wire::Gemini => {
            let key = present(&request.api_key).ok_or("no API key")?;
            Call {
                wire,
                url: join(
                    base.unwrap_or(GEMINI_BASE),
                    &format!("models/{}:generateContent", request.model),
                ),
                headers: vec![("x-goog-api-key", key.to_string())],
                body: json!({
                    "systemInstruction": {"parts": [{"text": system}]},
                    "contents": [{"role": "user", "parts": [{"text": request.text}]}],
                    "generationConfig": {"temperature": GEMINI_TEMPERATURE},
                }),
            }
        }
    })
}

/// The translated text inside a provider's JSON reply.
fn parse_reply(wire: Wire, reply: &Value) -> Result<String, String> {
    match wire {
        Wire::OpenAi => reply
            .pointer("/choices/0/message/content")
            .map(reply_text)
            .ok_or_else(|| "reply has no message content".to_string()),
        Wire::Ollama => reply
            .pointer("/message/content")
            .map(reply_text)
            .ok_or_else(|| "reply has no message content".to_string()),
        Wire::Gemini => {
            // A blocked prompt comes back without candidates; like langchain,
            // that is an empty translation rather than a failed engine.
            let Some(parts) = reply.pointer("/candidates/0/content/parts").and_then(Value::as_array) else {
                return Ok(String::new());
            };
            let joined: String = parts
                .iter()
                .filter(|part| part.get("thought").and_then(Value::as_bool) != Some(true))
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect();
            Ok(py_strip(&joined).to_string())
        }
    }
}

pub async fn translate(request: Request) -> Result<String, String> {
    let call = build(&request)?;
    let reply = post_json(&call.url, &call.headers, &call.body).await?;
    parse_reply(call.wire, &reply)
}
