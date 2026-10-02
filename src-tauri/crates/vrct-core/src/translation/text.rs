//! One translation of a piece of text, from language names to the engine's
//! answer: `Translator.translate` for the engines Rust performs (DeepL and the
//! LLM providers). Python keeps the authenticated key, model and history and
//! hands them over with each call.

use serde::Deserialize;
use serde_json::{json, Value};

use super::languages::language_codes;
use super::{deepl, llm};

#[derive(Debug, Deserialize)]
pub struct Request {
    /// Key of Python's `translation_lang`: `DeepL_API`, `OpenAI_API`, ...
    pub engine: String,
    pub source_language: String,
    pub target_language: String,
    /// Picks DeepL's English/Portuguese variant.
    #[serde(default)]
    pub target_country: String,
    #[serde(default)]
    pub weight_type: String,
    pub text: String,
    #[serde(default)]
    pub api_key: Option<String>,
    /// Where the engine lives when it is not the provider's own address
    /// (OpenAI-compatible, LM Studio, Ollama; a test server for DeepL).
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub history: Vec<Value>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Text(String),
    /// A language (or the engine) is not in the table. Not a failure.
    Unsupported(String),
}

impl Outcome {
    /// `{kind: "text", text}` or `{kind: "unsupported", reason}`.
    pub fn to_json(&self) -> Value {
        match self {
            Outcome::Text(text) => json!({"kind": "text", "text": text}),
            Outcome::Unsupported(reason) => json!({"kind": "unsupported", "reason": reason}),
        }
    }
}

const LLM_ENGINES: &[&str] =
    &["OpenAI_API", "OpenAI_Compatible", "Groq_API", "OpenRouter_API", "Plamo_API", "LMStudio", "Ollama", "Gemini_API"];

/// Whether this build performs `engine` itself.
pub fn handles(engine: &str) -> bool {
    engine == "DeepL_API" || LLM_ENGINES.contains(&engine)
}

pub async fn translate(request: Request) -> Result<Outcome, String> {
    if !handles(&request.engine) {
        return Err(format!("engine {:?} is not performed here", request.engine));
    }
    if request.source_language == request.target_language {
        return Ok(Outcome::Text(request.text));
    }
    let (source, target) = match language_codes(
        &request.engine,
        &request.weight_type,
        &request.target_country,
        &request.source_language,
        &request.target_language,
    ) {
        Ok(codes) => codes,
        Err(unsupported) => return Ok(Outcome::Unsupported(unsupported.0)),
    };
    let text = if request.engine == "DeepL_API" {
        let auth_key = request.api_key.filter(|key| !key.is_empty()).ok_or("no API key")?;
        deepl::translate(deepl::Request {
            auth_key,
            text: request.text,
            source_lang: Some(source),
            target_lang: target,
            server_url: request.base_url,
        })
        .await?
    } else {
        llm::translate(llm::Request {
            engine: request.engine,
            base_url: request.base_url,
            api_key: request.api_key,
            model: request.model,
            text: request.text,
            input_lang: source,
            output_lang: target,
            history: request.history,
        })
        .await?
    };
    Ok(Outcome::Text(text))
}
