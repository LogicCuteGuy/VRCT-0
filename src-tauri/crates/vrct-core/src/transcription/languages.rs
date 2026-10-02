//! `transcription_lang`: a display language and country to each engine's own language code.
//!
//! The table is Python's `models/transcription/transcription_languages.py` as JSON
//! (`assets/transcription_languages.json`, written by `tests/fixtures/regenerate_cloud_stt_golden.py`).

use std::sync::OnceLock;

use serde_json::{Map, Value};

fn table() -> &'static Value {
    static TABLE: OnceLock<Value> = OnceLock::new();
    TABLE.get_or_init(|| {
        serde_json::from_str(include_str!("assets/transcription_languages.json"))
            .expect("transcription_languages.json is valid JSON")
    })
}

/// The codes of one language in one country: `Google`, `Whisper`, `Groq_Whisper`, ...
pub fn entry(language: &str, country: &str) -> Option<&'static Map<String, Value>> {
    table().get(language)?.get(country)?.as_object()
}

/// `transcription_lang[language][country][engine]`.
pub fn code(language: &str, country: &str, engine: &str) -> Option<&'static str> {
    entry(language, country)?.get(engine)?.as_str()
}

/// Every display language, in the table's order, with its countries.
pub fn languages() -> impl Iterator<Item = (&'static str, Vec<&'static str>)> {
    table().as_object().into_iter().flatten().map(|(language, countries)| {
        (language.as_str(), countries.as_object().into_iter().flatten().map(|(c, _)| c.as_str()).collect())
    })
}
