//! From a message to its translations: `Model.getTranslate`, `getInputTranslate` and `getOutputTranslate`.
//!
//! The engines themselves are behind [`Translator`] (Python's `Translator.translate`, which never raises:
//! it answers a translation, "this engine lacks the language pair", or "the engine failed"). What lives
//! here is what the model does around them: pick the engine and languages from the settings, translate
//! into every enabled target language side by side, and when the chosen engine fails, try the local
//! CTranslate2 model before giving up and keeping the original text.
//!
//! `tests/flow.rs` replays scenarios recorded from the real Python code.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::pipeline::history::SharedHistory;
use crate::pipeline::spec::Direction;
use crate::pipeline::{TranslateError, Translated};
use crate::transcription::native::Config;

/// What `Translator.translate` returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// The translation (possibly empty).
    Text(String),
    /// The engine does not have this language pair. Not a failure of the engine.
    Unsupported,
    /// The engine failed: a limit, the network, a refused key.
    Failed,
}

/// One call to an engine.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    pub engine: &'a str,
    pub weight_type: &'a str,
    pub source_language: Option<&'a str>,
    pub target_language: Option<&'a str>,
    pub target_country: Option<&'a str>,
    pub message: &'a str,
    /// The conversation so far, for the engines that use it; none on the local fallback.
    pub history: Option<&'a [Value]>,
}

pub trait Translator: Send + Sync {
    fn translate(&self, request: &Request<'_>) -> Reply;
    /// `isLoadedCTranslate2Model`: whether the local model is in memory.
    fn ctranslate2_loaded(&self) -> bool;
    /// Waits between two attempts at the local model.
    fn pause(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
    /// Called with the details logged when even the local model failed (`errorLogging`).
    fn report_failure(&self) {}
}

/// How often the local model is asked after the chosen engine failed, and how long between asks.
const FALLBACK_ATTEMPTS: usize = 20;
const FALLBACK_PAUSE: Duration = Duration::from_millis(100);

pub struct TranslationFlow {
    config: Arc<dyn Config>,
    translator: Arc<dyn Translator>,
    history: SharedHistory,
}

fn text_of(value: &Value) -> Option<String> {
    value.as_str().map(str::to_string)
}

impl TranslationFlow {
    pub fn new(config: Arc<dyn Config>, translator: Arc<dyn Translator>, history: SharedHistory) -> Self {
        Self { config, translator, history }
    }

    fn setting(&self, name: &str) -> Value {
        self.config.get(name).unwrap_or(Value::Null)
    }

    /// `getInputTranslate` or `getOutputTranslate`.
    pub fn translate(&self, direction: Direction, message: &str, source_language: Option<&str>) -> Result<Translated, TranslateError> {
        match direction {
            Direction::Input => self.input(message, source_language),
            Direction::Output => self.output(message, source_language),
        }
    }

    fn tab(&self) -> String {
        self.setting("SELECTED_TAB_NO").as_str().unwrap_or_default().to_string()
    }

    fn engine(&self, tab: &str) -> String {
        self.setting("SELECTED_TRANSLATION_ENGINES").get(tab).and_then(text_of).unwrap_or_default()
    }

    /// My language into every enabled target language, all at once.
    pub fn input(&self, message: &str, source_language: Option<&str>) -> Result<Translated, TranslateError> {
        let tab = self.tab();
        let engine = self.engine(&tab);
        let source = source_language
            .map(str::to_string)
            .or_else(|| self.setting("SELECTED_YOUR_LANGUAGES").pointer(&format!("/{tab}/1/language")).and_then(text_of));
        let slots = self.setting("SELECTED_TARGET_LANGUAGES");
        let mut targets: Vec<(Option<String>, Option<String>)> = Vec::new();
        if let Some(slots) = slots.get(&tab).and_then(Value::as_object) {
            for slot in slots.values() {
                if slot.get("enable") == Some(&Value::Bool(true)) {
                    let language = slot.get("language").and_then(text_of);
                    let country = slot.get("country").and_then(text_of);
                    if language.is_some() || country.is_some() {
                        targets.push((language, country));
                    }
                }
            }
        }

        let one = |(language, country): &(Option<String>, Option<String>)| {
            self.one(&engine, source.as_deref(), language.as_deref(), country.as_deref(), message)
        };
        let results: Vec<Result<(String, bool), TranslateError>> = match targets.as_slice() {
            [] => Vec::new(),
            [only] => vec![one(only)],
            many => std::thread::scope(|scope| {
                let workers: Vec<_> = many.iter().map(|target| scope.spawn(move || one(target))).collect();
                workers
                    .into_iter()
                    .map(|worker| worker.join().unwrap_or_else(|_| Err(TranslateError::Failed("a translation worker panicked".into()))))
                    .collect()
            }),
        };
        // In submission order; the first failure is the one reported.
        let mut translated = Translated { translation: Vec::new(), success: Vec::new() };
        for result in results {
            let (translation, success) = result?;
            translated.translation.push(translation);
            translated.success.push(success);
        }
        Ok(translated)
    }

    /// The other side's language into mine.
    pub fn output(&self, message: &str, source_language: Option<&str>) -> Result<Translated, TranslateError> {
        let tab = self.tab();
        let engine = self.engine(&tab);
        let source = source_language
            .map(str::to_string)
            .or_else(|| self.setting("SELECTED_TARGET_LANGUAGES").pointer(&format!("/{tab}/1/language")).and_then(text_of));
        let yours = self.setting("SELECTED_YOUR_LANGUAGES");
        let language = yours.pointer(&format!("/{tab}/1/language")).and_then(text_of);
        let country = yours.pointer(&format!("/{tab}/1/country")).and_then(text_of);
        let (translation, success) = self.one(&engine, source.as_deref(), language.as_deref(), country.as_deref(), message)?;
        Ok(Translated { translation: vec![translation], success: vec![success] })
    }

    /// `getTranslate`: one target. When the engine fails the local model gets a few tries, and when that
    /// fails too the original text stands in (and the flag says so).
    fn one(
        &self,
        engine: &str,
        source: Option<&str>,
        target: Option<&str>,
        country: Option<&str>,
        message: &str,
    ) -> Result<(String, bool), TranslateError> {
        let weight_type = self.setting("CTRANSLATE2_WEIGHT_TYPE").as_str().unwrap_or_default().to_string();
        let history = self.history.snapshot();
        let ask = |engine: &str, history: Option<&[Value]>| {
            self.translator.translate(&Request {
                engine,
                weight_type: &weight_type,
                source_language: source,
                target_language: target,
                target_country: country,
                message,
                history,
            })
        };

        if let Reply::Text(text) = ask(engine, Some(&history)) {
            return Ok((text, true));
        }
        let mut reply = Reply::Failed;
        for _ in 0..FALLBACK_ATTEMPTS {
            reply = ask("CTranslate2", None);
            if matches!(reply, Reply::Text(_) | Reply::Unsupported) {
                break;
            }
            if !self.translator.ctranslate2_loaded() {
                // Not loaded (never downloaded, failed to load, just switched): waiting changes nothing.
                break;
            }
            self.translator.pause(FALLBACK_PAUSE);
        }
        match reply {
            Reply::Text(text) => Ok((text, true)),
            // Neither engine has the pair: nothing broke, there is just nothing to translate.
            Reply::Unsupported => Ok((message.to_string(), true)),
            Reply::Failed => {
                self.translator.report_failure();
                Ok((message.to_string(), false))
            }
        }
    }
}
