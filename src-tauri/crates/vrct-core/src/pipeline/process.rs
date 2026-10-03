//! `Controller._processMessage` and the four entry points that feed it.
//!
//! A message (what the microphone heard, what the speakers played, what the screen showed, what was
//! typed) goes through the same steps: word filter, repeat check, translation, transliteration, then out
//! to the chatbox, the overlays, the clipboard, the UI, the WebSocket, the log file and the history. What
//! differs between the directions is data ([`Spec`]), as in Python's `MessageDirectionSpec`.
//!
//! `tests/pipeline.rs` replays scenarios recorded from the real Python code against this one.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use serde_json::{json, Map, Value};

use super::errors::{TRANSLATION_DISABLED_VRAM, TRANSLATION_ENGINE_LIMIT};
use super::format::message_formatter;
use super::history::{isoformat_now, History};
use super::host::{Host, LargeLog, SmallLog};
use super::keywords::KeywordFilter;
use super::spec::{self, endpoints, Delivery, OwnTransliteration, Repeat, Spec};
use crate::transcription::native::Config;

#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    /// The translator raised something that is not an out-of-memory error.
    #[error("translation failed: {0}")]
    Translate(String),
    /// A target language slot is enabled but the translator returned fewer translations than there are
    /// slots in front of it (Python's `translation[i]` raising `IndexError`).
    #[error("no translation for target slot {0}")]
    MissingTranslation(usize),
    /// The request lacks something Python indexed (`result["text"]`, `data["id"]`).
    #[error("malformed message: {0}")]
    Malformed(&'static str),
}

#[derive(Default)]
struct State {
    keywords: KeywordFilter,
    previous_send: String,
    previous_receive: String,
    history: History,
}

pub struct Pipeline {
    config: Arc<dyn Config>,
    host: Arc<dyn Host>,
    state: Mutex<State>,
}

// ---- reading settings the way the Python code does ---------------------------------------------------------

fn value(config: &dyn Config, name: &str) -> Value {
    config.get(name).unwrap_or(Value::Null)
}

fn text(config: &dyn Config, name: &str) -> String {
    value(config, name).as_str().unwrap_or_default().to_string()
}

/// `config.X is True`.
fn is_true(config: &dyn Config, name: &str) -> bool {
    value(config, name) == Value::Bool(true)
}

/// `config.X is False`.
fn is_false(config: &dyn Config, name: &str) -> bool {
    value(config, name) == Value::Bool(false)
}

fn nested<'a>(root: &'a Value, path: &[&str]) -> &'a Value {
    let mut current = root;
    for key in path {
        current = current.get(key).unwrap_or(&Value::Null);
    }
    current
}

fn rounded_ms(started: Instant) -> i64 {
    (started.elapsed().as_secs_f64() * 1000.0).round() as i64
}

impl Pipeline {
    pub fn new(config: Arc<dyn Config>, host: Arc<dyn Host>) -> Self {
        Self { config, host, state: Mutex::new(State::default()) }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// `resetKeywordProcessor` + `addKeywords`: the words that keep a transcript from being sent.
    pub fn set_word_filter(&self, words: &[String]) {
        let mut filter = KeywordFilter::new();
        for word in words {
            filter.add(word);
        }
        self.state().keywords = filter;
    }

    /// The translation context so far (`getTranslationHistory`).
    pub fn history(&self) -> Vec<Value> {
        self.state().history.to_values()
    }

    pub fn clear_history(&self) {
        self.state().history.clear();
    }

    fn check_keywords(&self, message: &str) -> bool {
        self.state().keywords.matches(message)
    }

    /// `detectRepeatSendMessage` / `detectRepeatReceiveMessage`: whether this is the last message again.
    fn detect_repeat(&self, which: Repeat, message: &str) -> bool {
        let mut state = self.state();
        let previous = match which {
            Repeat::Send => &mut state.previous_send,
            Repeat::Receive => &mut state.previous_receive,
        };
        let repeated = previous == message;
        *previous = message.to_string();
        repeated
    }

    fn add_history(&self, source: &str, message: &str) {
        self.state().history.add(source, message, isoformat_now);
    }

    // ---- the entry points ----------------------------------------------------------------------------------

    /// `micMessage`: one result of the microphone's transcription.
    pub fn mic_message(&self, result: &Value) -> Result<(), PipelineError> {
        let recognition_error = result.get("recognition_error") == Some(&Value::Bool(true));
        if !recognition_error && is_true(&*self.config, "VRC_MIC_MUTE_SYNC") && self.host.mic_mute_status() == Some(true) {
            return Ok(());
        }
        self.speech_message(result, &spec::MIC, "mic", "Mic")
    }

    /// `speakerMessage`: one result of the speaker's transcription.
    pub fn speaker_message(&self, result: &Value) -> Result<(), PipelineError> {
        self.speech_message(result, &spec::SPEAKER, "speaker", "Speaker")
    }

    fn speech_message(&self, result: &Value, spec: &Spec, source: &str, label: &str) -> Result<(), PipelineError> {
        if result.get("recognition_error") == Some(&Value::Bool(true)) {
            let is_pipeline_error = ["error_code", "stage", "source", "message", "recoverable"].iter().all(|key| result.get(key).is_some());
            let payload = if is_pipeline_error {
                self.disable_transcription_after_pipeline_error(source);
                json!({
                    "error_code": result["error_code"],
                    "stage": result["stage"],
                    "source": result["source"],
                    "message": result["message"],
                    "recoverable": result["recoverable"],
                })
            } else {
                json!({
                    "message": format!("{label} speech recognition request failed. Check your network connection."),
                    "data": null,
                })
            };
            self.host.run(200, endpoints::RECOGNITION_ERROR, payload);
            return Ok(());
        }

        let message = result.get("text").ok_or(PipelineError::Malformed("text"))?;
        let language = result.get("language").ok_or(PipelineError::Malformed("language"))?;
        if message == &Value::Bool(false) {
            self.host.run(400, endpoints::ERROR_DEVICE, json!({"message": format!("No {source} device detected"), "data": null}));
        } else if let Some(message) = message.as_str().filter(|message| !message.is_empty()) {
            let asr_ms = result.get("asr_ms").and_then(Value::as_i64);
            self.process_message(spec, message, language.as_str(), None, asr_ms)?;
        }
        Ok(())
    }

    /// `ocrMessage`: a chat bubble read off the screen.
    pub fn ocr_message(&self, result: &Value) -> Result<(), PipelineError> {
        if !is_true(&*self.config, "ENABLE_OCR_CAPTURE") {
            return Ok(());
        }
        let Some(message) = result.get("text").and_then(Value::as_str).filter(|message| !message.is_empty()) else {
            return Ok(());
        };
        // The bubble holds what someone else said, so the text is in their language; none means the
        // settings' own target language, never mine.
        let language = result.get("language").and_then(Value::as_str).filter(|language| !language.is_empty());
        let msg_id = match result.get("segment_id") {
            None | Some(Value::Null) => None,
            Some(Value::String(id)) => Some(json!(format!("transcription-ocr-{id}"))),
            Some(id) => Some(json!(format!("transcription-ocr-{id}"))),
        };
        self.process_message(&spec::OCR, message, language, msg_id, None)?;
        Ok(())
    }

    /// `chatMessage`: something typed. The answer is `{"status": 200, "result": ...}`.
    pub fn chat_message(&self, data: &Value) -> Result<Value, PipelineError> {
        let msg_id = data.get("id").ok_or(PipelineError::Malformed("id"))?.clone();
        let message = data.get("message").and_then(Value::as_str).ok_or(PipelineError::Malformed("message"))?;
        if message.is_empty() {
            self.add_history("chat", message);
            return Ok(json!({
                "status": 200,
                "result": {"id": msg_id, "original": {"message": message, "transliteration": []}, "translations": []},
            }));
        }
        let result = self.process_message(&spec::CHAT, message, None, Some(msg_id), None)?;
        Ok(json!({"status": 200, "result": result.unwrap_or(Value::Null)}))
    }

    /// `_disableTranscriptionAfterPipelineError`: the session stopped itself, so the setting and the UI follow.
    fn disable_transcription_after_pipeline_error(&self, source: &str) {
        let (setting, endpoint) = match source {
            "mic" => ("ENABLE_TRANSCRIPTION_SEND", endpoints::DISABLE_TRANSCRIPTION_SEND),
            "speaker" => ("ENABLE_TRANSCRIPTION_RECEIVE", endpoints::DISABLE_TRANSCRIPTION_RECEIVE),
            _ => return,
        };
        self.host.set_setting(setting, Value::Bool(false));
        self.host.run(200, endpoint, Value::Bool(false));
    }

    // ---- the pipeline ----------------------------------------------------------------------------------------

    /// `_processMessage`. `message` is not empty. Pushing directions answer `None`; chat answers
    /// `{"id", "original", "translations"}`.
    pub fn process_message(
        &self,
        spec: &Spec,
        message: &str,
        language: Option<&str>,
        msg_id: Option<Value>,
        asr_ms: Option<i64>,
    ) -> Result<Option<Value>, PipelineError> {
        let config: &dyn Config = &*self.config;
        let host = &*self.host;
        let msg_id = msg_id.filter(|id| !id.is_null());

        if spec.has_word_filter && self.check_keywords(message) {
            host.run(200, endpoints::WORD_FILTER, json!({"message": format!("Detected by word filter: {message}")}));
            return Ok(None);
        }
        if let Some(which) = spec.repeat {
            if self.detect_repeat(which, message) {
                return Ok(None);
            }
        }

        let pipeline_started = Instant::now();
        let mut translate_ms = 0;
        let tab = text(config, "SELECTED_TAB_NO");
        let no_list: Vec<String> = value(config, "SELECTED_TAB_TARGET_LANGUAGES_NO_LIST")
            .as_array()
            .map(|slots| slots.iter().filter_map(|slot| slot.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        let your_languages = value(config, "SELECTED_YOUR_LANGUAGES");
        let target_languages = value(config, "SELECTED_TARGET_LANGUAGES");
        let your_tab = your_languages.get(&tab).cloned().unwrap_or(Value::Null);
        let target_tab = target_languages.get(&tab).cloned().unwrap_or(Value::Null);

        let mut translation: Vec<String> = Vec::new();
        if !is_false(config, "ENABLE_TRANSLATION") {
            let started = Instant::now();
            let outcome = host.translate(spec.translate, message, language);
            translate_ms = rounded_ms(started);
            match outcome {
                Ok(translated) => {
                    translation = translated.translation;
                    if !translated.success.iter().all(|ok| *ok) {
                        host.fall_back_to_ctranslate2();
                        let (status, result) = TRANSLATION_ENGINE_LIMIT.response(Value::Null);
                        host.run(status, endpoints::ERROR_TRANSLATION_ENGINE, result);
                    }
                }
                Err(error) => {
                    let Some(error_message) = error.vram_message() else {
                        return Err(PipelineError::Translate(error.to_string()));
                    };
                    let (status, result) = spec.vram_error.response(Value::String(error_message));
                    host.run(status, spec.vram_endpoint, result);
                    host.disable_translation();
                    let (status, result) = TRANSLATION_DISABLED_VRAM.response(Value::Bool(false));
                    host.run(status, endpoints::ENABLE_TRANSLATION, result);
                    if spec.delivery == Delivery::Return {
                        let empty: Vec<Value> = no_list.iter().map(|_| json!({"message": "", "transliteration": []})).collect();
                        return Ok(Some(json!({
                            "id": msg_id.unwrap_or(Value::Null),
                            "original": {"message": message, "transliteration": []},
                            "translations": empty,
                        })));
                    }
                    return Ok(None);
                }
            }
        }

        let hiragana = is_true(config, "CONVERT_MESSAGE_TO_HIRAGANA");
        let romaji = is_true(config, "CONVERT_MESSAGE_TO_ROMAJI");
        let enable_translation = is_true(config, "ENABLE_TRANSLATION");
        let your_language_is_japanese = nested(&your_tab, &["1", "language"]) == "Japanese";
        let mut transliteration_message: Vec<Value> = Vec::new();
        let mut transliteration_translation: Vec<Vec<Value>> = Vec::new();
        if hiragana || romaji {
            let own_is_japanese = match spec.own_transliteration {
                OwnTransliteration::YourLanguage => your_language_is_japanese,
                OwnTransliteration::DetectedLanguage => language == Some("Japanese"),
            };
            if own_is_japanese {
                transliteration_message = host.transliterate(message, hiragana, romaji);
            }
            if spec.multi_target {
                for (i, no) in no_list.iter().enumerate() {
                    let slot = nested(&target_tab, &[no]);
                    if enable_translation && slot.get("language") == Some(&json!("Japanese")) && slot.get("enable") == Some(&Value::Bool(true)) {
                        let translated = translation.get(i).ok_or(PipelineError::MissingTranslation(i))?;
                        transliteration_translation.push(host.transliterate(translated, hiragana, romaji));
                    } else {
                        transliteration_translation.push(Vec::new());
                    }
                }
            } else if enable_translation && your_language_is_japanese {
                let translated = translation.first().ok_or(PipelineError::MissingTranslation(0))?;
                transliteration_translation.push(host.transliterate(translated, hiragana, romaji));
            } else {
                transliteration_translation.push(Vec::new());
            }
        } else if spec.multi_target {
            transliteration_translation = no_list.iter().map(|_| Vec::new()).collect();
        } else {
            transliteration_translation = vec![Vec::new()];
        }

        let translations: Vec<Value> = translation
            .iter()
            .zip(&transliteration_translation)
            .map(|(translated, transliteration)| json!({"message": translated, "transliteration": transliteration}))
            .collect();
        let mut payload = Map::new();
        payload.insert("original".to_string(), json!({"message": message, "transliteration": transliteration_message}));
        payload.insert("translations".to_string(), Value::Array(translations));
        if let Some(source) = spec.payload_source {
            payload.insert("source".to_string(), json!(source));
        }
        if let Some(id) = &msg_id {
            payload.insert("id".to_string(), id.clone());
        }

        let gate_open = spec.feature_gate.is_none_or(|gate| is_true(config, gate));
        if gate_open {
            let parts = value(config, spec.osc_format.setting());
            if spec.osc_gate.is_some_and(|gate| is_true(config, gate)) {
                let osc_message = if is_true(config, "SEND_ONLY_TRANSLATED_MESSAGES") {
                    if is_false(config, "ENABLE_TRANSLATION") {
                        message_formatter(&parts, &[], message)
                    } else {
                        message_formatter(&parts, &translation, "")
                    }
                } else {
                    message_formatter(&parts, &translation, message)
                };
                host.send_osc(&osc_message);
            }

            let only_translated = is_true(config, "OVERLAY_SHOW_ONLY_TRANSLATED_MESSAGES");
            if spec.overlay_small_log && is_true(config, "OVERLAY_SMALL_LOG") && host.overlay_available() {
                if only_translated {
                    if !translation.is_empty() {
                        host.overlay_small_log(&SmallLog {
                            message: None,
                            language: None,
                            translation: &translation,
                            your_languages: &your_tab,
                            transliteration_message: &transliteration_message,
                            transliteration_translation: &transliteration_translation,
                        });
                    }
                } else {
                    host.overlay_small_log(&SmallLog {
                        message: Some(message),
                        language,
                        translation: &translation,
                        your_languages: &your_tab,
                        transliteration_message: &transliteration_message,
                        transliteration_translation: &transliteration_translation,
                    });
                }
            }

            if is_true(config, "OVERLAY_LARGE_LOG") && host.overlay_available() {
                let (own_language, languages) = if spec.overlay_direction == "send" {
                    (nested(&your_tab, &["1", "language"]).as_str(), &target_tab)
                } else {
                    (language, &your_tab)
                };
                if only_translated {
                    if !translation.is_empty() {
                        host.overlay_large_log(&LargeLog {
                            direction: spec.overlay_direction,
                            message: None,
                            language: None,
                            translation: &translation,
                            languages,
                            transliteration_message: &transliteration_message,
                            transliteration_translation: &transliteration_translation,
                        });
                    }
                } else {
                    host.overlay_large_log(&LargeLog {
                        direction: spec.overlay_direction,
                        message: Some(message),
                        language: own_language,
                        translation: &translation,
                        languages,
                        transliteration_message: &transliteration_message,
                        transliteration_translation: &transliteration_translation,
                    });
                }
            }

            if spec.clipboard && is_true(config, "ENABLE_CLIPBOARD") {
                host.set_clipboard(&message_formatter(&parts, &translation, message));
            }

            if let Delivery::Push(endpoint) = spec.delivery {
                host.run(200, endpoint, Value::Object(payload.clone()));
            }

            if host.websocket_alive() {
                let languages = |name: &str| value(config, name).get(&tab).cloned().unwrap_or(Value::Null);
                host.websocket_send(json!({
                    "type": spec.ws_type,
                    "src_languages": languages(spec.ws_src_languages),
                    "dst_languages": languages(spec.ws_dst_languages),
                    "message": message,
                    "translation": translation,
                    "transliteration": transliteration_translation,
                }));
            }

            if is_true(config, "LOGGER_FEATURE") {
                let translated = if translation.is_empty() { String::new() } else { format!(" ({})", translation.join("/")) };
                host.log_info(&format!("{} {message}{translated}", spec.logger_prefix));
            }
        }

        self.add_history(spec.kind, message);

        // Where the time went, from the end of the transcription to the end of the outputs.
        let pipeline_ms = rounded_ms(pipeline_started);
        let asr_part = asr_ms.map(|ms| format!("asr={ms}ms ")).unwrap_or_default();
        host.log(&format!(
            "[latency][{}] {asr_part}translate={translate_ms}ms output={}ms total={}ms",
            spec.kind,
            pipeline_ms - translate_ms,
            pipeline_ms + asr_ms.unwrap_or(0)
        ));

        if spec.delivery == Delivery::Return {
            let mut result = Map::new();
            result.insert("id".to_string(), msg_id.unwrap_or(Value::Null));
            result.extend(payload);
            return Ok(Some(Value::Object(result)));
        }
        Ok(None)
    }
}
