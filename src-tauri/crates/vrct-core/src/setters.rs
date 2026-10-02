//! The `/set/data/*` endpoints that only change a setting, served from [`Settings`].
//!
//! These are the controller's handlers that read and write `config` and nothing else, ported with
//! the answers Python gave: `tests/setters.rs` runs every one against the recorded replies of the
//! real code. A handler either answers 200 with the value now stored, a 400 error reply carrying the
//! value that is still in force, or (where Python let an exception escape) 500 `"Internal error"`.
//!
//! Only the endpoints in [`NATIVE`] are registered. The sidecar still reads some of these settings
//! (the recorder's thresholds, the message pipeline's formats, the overlay's language), and it would
//! go on using the old value if Rust answered the request; those endpoints stay with the sidecar
//! until what reads them has been ported, and Rust learns of the change over the config bridge.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::router::{Reply, Router};
use crate::settings::pyconv::{int_value, py_dict, py_float, py_int, py_str};
use crate::settings::{SetError, Settings};

/// The endpoints Rust answers now: their settings are read by the UI and by Rust code only.
pub const NATIVE: &[&str] = &[
    "/set/data/release_channel",
    "/set/data/transparency",
    "/set/data/ui_scaling",
    "/set/data/textbox_ui_scaling",
    "/set/data/message_box_ratio",
    "/set/data/send_message_button_type",
    "/set/data/font_family",
    "/set/data/main_window_geometry",
    "/set/data/hotkeys",
];

/// An error code and its text, as `errors.py` has it.
struct Code {
    name: &'static str,
    message: &'static str,
    category: &'static str,
    severity: &'static str,
}

const fn code(name: &'static str, message: &'static str, category: &'static str, severity: &'static str) -> Code {
    Code { name, message, category, severity }
}

const CONFIG_VALUE_INVALID: Code = code("VALIDATION_CONFIG_VALUE_INVALID", "The provided value was rejected", "validation", "warning");
const GENERAL_EXCEPTION: Code = code("GENERAL_EXCEPTION", "An error occurred", "general", "error");
const TRANSCRIPTION_MODEL_INVALID: Code =
    code("MODEL_TRANSCRIPTION_INVALID", "Transcription API model is not valid", "model", "warning");
const MIC_THRESHOLD: Code = code("VALIDATION_MIC_THRESHOLD", "Mic energy threshold value is out of range", "validation", "warning");
const MIC_RECORD_TIMEOUT: Code =
    code("VALIDATION_MIC_RECORD_TIMEOUT", "Mic record timeout value is out of range", "validation", "warning");
const MIC_PHRASE_TIMEOUT: Code =
    code("VALIDATION_MIC_PHRASE_TIMEOUT", "Mic phrase timeout value is out of range", "validation", "warning");
const MIC_MAX_PHRASES: Code = code("VALIDATION_MIC_MAX_PHRASES", "Mic max phrases value is out of range", "validation", "warning");
const SPEAKER_THRESHOLD: Code =
    code("VALIDATION_SPEAKER_THRESHOLD", "Speaker energy threshold value is out of range", "validation", "warning");
const SPEAKER_RECORD_TIMEOUT: Code =
    code("VALIDATION_SPEAKER_RECORD_TIMEOUT", "Speaker record timeout value is out of range", "validation", "warning");
const SPEAKER_PHRASE_TIMEOUT: Code =
    code("VALIDATION_SPEAKER_PHRASE_TIMEOUT", "Speaker phrase timeout value is out of range", "validation", "warning");
const SPEAKER_MAX_PHRASES: Code =
    code("VALIDATION_SPEAKER_MAX_PHRASES", "Speaker max phrases value is out of range", "validation", "warning");

/// `VRCTError.create_error_response`: status 400 with the code, its text and the value to keep showing.
fn error(code: &Code, data: Value, message: Option<&str>) -> Reply {
    (
        400,
        json!({
            "error_code": code.name,
            "message": message.unwrap_or(code.message),
            "data": data,
            "details": {},
            "category": code.category,
            "severity": code.severity,
        }),
    )
}

/// What `mainloop` answers when a handler raises.
fn internal_error() -> Reply {
    (500, json!("Internal error"))
}

/// How the payload becomes the value that is set.
enum Conv {
    /// As it came (`config.X = data`).
    Raw,
    /// `str(data)`.
    Text,
    /// `dict(data)`.
    Dict,
}

/// A limit a number must respect: a constant, or another setting's current value.
enum Bound {
    At(i64),
    Setting(&'static str),
}

enum Kind {
    /// `config.X = <conv>(data)`; a refused value is a `VALIDATION_CONFIG_VALUE_INVALID` reply.
    Plain(Conv),
    /// `config.X = int(data)`; a payload that is no integer is a `GENERAL_EXCEPTION` reply with this text.
    Integer(&'static str),
    /// `int(data)` that must lie between the bounds, with its own error code for everything that goes wrong.
    Ranged { code: Code, at_least: &'static [Bound], at_most: Option<Bound> },
    /// `config.X = float(data)`, like `Integer`.
    Float(&'static str),
    /// `str(data)` that must be on the setting `list`; else `MODEL_TRANSCRIPTION_INVALID`.
    OnList(&'static str),
}

struct Def {
    endpoint: &'static str,
    setting: &'static str,
    kind: Kind,
}

const fn def(endpoint: &'static str, setting: &'static str, kind: Kind) -> Def {
    Def { endpoint, setting, kind }
}

const NON_NEGATIVE: &[Bound] = &[Bound::At(0)];

fn definitions() -> Vec<Def> {
    use Kind::*;
    vec![
        def("/set/data/selected_groq_whisper_model", "SELECTED_GROQ_WHISPER_MODEL", OnList("SELECTABLE_GROQ_WHISPER_MODEL_LIST")),
        def("/set/data/selected_openai_whisper_model", "SELECTED_OPENAI_WHISPER_MODEL", OnList("SELECTABLE_OPENAI_WHISPER_MODEL_LIST")),
        def("/set/data/selected_custom_whisper_model", "SELECTED_CUSTOM_WHISPER_MODEL", OnList("SELECTABLE_CUSTOM_WHISPER_MODEL_LIST")),
        def("/set/data/release_channel", "SELECTED_RELEASE_CHANNEL", Plain(Conv::Text)),
        def("/set/data/transparency", "TRANSPARENCY", Integer("Transparency must be a number")),
        def("/set/data/ui_scaling", "UI_SCALING", Integer("UI scaling must be a number")),
        def("/set/data/textbox_ui_scaling", "TEXTBOX_UI_SCALING", Integer("Textbox UI scaling must be a number")),
        def("/set/data/message_box_ratio", "MESSAGE_BOX_RATIO", Plain(Conv::Raw)),
        def("/set/data/send_message_button_type", "SEND_MESSAGE_BUTTON_TYPE", Plain(Conv::Raw)),
        def("/set/data/font_family", "FONT_FAMILY", Plain(Conv::Raw)),
        def("/set/data/ui_language", "UI_LANGUAGE", Plain(Conv::Raw)),
        def("/set/data/main_window_geometry", "MAIN_WINDOW_GEOMETRY", Plain(Conv::Raw)),
        def(
            "/set/data/mic_threshold",
            "MIC_THRESHOLD",
            Ranged { code: MIC_THRESHOLD, at_least: NON_NEGATIVE, at_most: Some(Bound::Setting("MAX_MIC_THRESHOLD")) },
        ),
        def(
            "/set/data/mic_record_timeout",
            "MIC_RECORD_TIMEOUT",
            Ranged { code: MIC_RECORD_TIMEOUT, at_least: NON_NEGATIVE, at_most: Some(Bound::Setting("MIC_PHRASE_TIMEOUT")) },
        ),
        def(
            "/set/data/mic_phrase_timeout",
            "MIC_PHRASE_TIMEOUT",
            Ranged { code: MIC_PHRASE_TIMEOUT, at_least: &[Bound::Setting("MIC_RECORD_TIMEOUT")], at_most: None },
        ),
        def("/set/data/mic_max_phrases", "MIC_MAX_PHRASES", Ranged { code: MIC_MAX_PHRASES, at_least: NON_NEGATIVE, at_most: None }),
        def("/set/data/hotkeys", "HOTKEYS", Plain(Conv::Raw)),
        def("/set/data/mic_avg_logprob", "MIC_AVG_LOGPROB", Float("Mic average logprob must be a number")),
        def("/set/data/mic_no_speech_prob", "MIC_NO_SPEECH_PROB", Float("Mic no-speech probability must be a number")),
        def(
            "/set/data/speaker_threshold",
            "SPEAKER_THRESHOLD",
            Ranged { code: SPEAKER_THRESHOLD, at_least: NON_NEGATIVE, at_most: Some(Bound::Setting("MAX_SPEAKER_THRESHOLD")) },
        ),
        def(
            "/set/data/speaker_record_timeout",
            "SPEAKER_RECORD_TIMEOUT",
            Ranged { code: SPEAKER_RECORD_TIMEOUT, at_least: NON_NEGATIVE, at_most: Some(Bound::Setting("SPEAKER_PHRASE_TIMEOUT")) },
        ),
        def(
            "/set/data/speaker_phrase_timeout",
            "SPEAKER_PHRASE_TIMEOUT",
            Ranged {
                code: SPEAKER_PHRASE_TIMEOUT,
                at_least: &[Bound::At(0), Bound::Setting("SPEAKER_RECORD_TIMEOUT")],
                at_most: None,
            },
        ),
        def(
            "/set/data/speaker_max_phrases",
            "SPEAKER_MAX_PHRASES",
            Ranged { code: SPEAKER_MAX_PHRASES, at_least: NON_NEGATIVE, at_most: None },
        ),
        def("/set/data/speaker_avg_logprob", "SPEAKER_AVG_LOGPROB", Float("Speaker average logprob must be a number")),
        def("/set/data/speaker_no_speech_prob", "SPEAKER_NO_SPEECH_PROB", Float("Speaker no-speech probability must be a number")),
        def("/set/data/selected_whisper_weight_type", "WHISPER_WEIGHT_TYPE", Plain(Conv::Text)),
        def("/set/data/selected_transcription_compute_type", "SELECTED_TRANSCRIPTION_COMPUTE_TYPE", Plain(Conv::Text)),
        def("/set/data/send_message_format_parts", "SEND_MESSAGE_FORMAT_PARTS", Plain(Conv::Dict)),
        def("/set/data/received_message_format_parts", "RECEIVED_MESSAGE_FORMAT_PARTS", Plain(Conv::Dict)),
    ]
}

/// Every endpoint ported, whether or not it is registered yet.
pub fn endpoints() -> Vec<&'static str> {
    definitions().iter().map(|d| d.endpoint).collect()
}

/// Serves the [`NATIVE`] endpoints on `router`.
pub fn register(mut router: Router, settings: &Arc<Settings>) -> Router {
    for definition in definitions().into_iter().filter(|d| NATIVE.contains(&d.endpoint)) {
        let endpoint = definition.endpoint;
        let definition = Arc::new(definition);
        let settings = Arc::clone(settings);
        router = router.handle(endpoint, move |data| {
            let reply = answer(&definition, &settings, data.unwrap_or(Value::Null));
            async move { reply }
        });
    }
    router
}

/// The reply to one request. Public for the tests, which call it without a router.
pub fn answer_for(endpoint: &str, settings: &Settings, data: Value) -> Option<Reply> {
    let definition = definitions().into_iter().find(|d| d.endpoint == endpoint)?;
    Some(answer(&definition, settings, data))
}

fn answer(definition: &Def, settings: &Settings, data: Value) -> Reply {
    let key = definition.setting;
    let current = || settings.get(key).unwrap_or(Value::Null);
    // Sets the value and answers with what is stored. `refused` builds the reply for a rejected value.
    let apply = |value: Value, refused: &dyn Fn() -> Reply| -> Reply {
        match settings.set(key, value) {
            Ok(()) => (200, current()),
            Err(SetError::Invalid) => refused(),
            Err(_) => internal_error(),
        }
    };

    match &definition.kind {
        Kind::Plain(conv) => {
            let offered = match conv {
                Conv::Raw => data,
                Conv::Text => Value::String(py_str(&data)),
                // Number or `None` keys (`[[1, 2]]`) become text here and the setting refuses them, as
                // it refused the non-text keys in Python.
                Conv::Dict => match py_dict(&data) {
                    None => return internal_error(),
                    Some(map) => Value::Object(map),
                },
            };
            apply(offered.clone(), &|| error(&CONFIG_VALUE_INVALID, offered.clone(), None))
        }
        Kind::Integer(message) => {
            let Some(number) = py_int(&data).and_then(int_value) else {
                return error(&GENERAL_EXCEPTION, current(), Some(message));
            };
            apply(number, &internal_error)
        }
        Kind::Float(message) => {
            let Some(number) = py_float(&data).and_then(crate::settings::pyvalue::float_value) else {
                return error(&GENERAL_EXCEPTION, current(), Some(message));
            };
            apply(number, &internal_error)
        }
        Kind::Ranged { code, at_least, at_most } => {
            let refuse = || error(code, current(), None);
            let Some(number) = py_int(&data) else { return refuse() };
            let bound = |bound: &Bound| match bound {
                Bound::At(limit) => Some(i128::from(*limit)),
                Bound::Setting(name) => settings.get(name).as_ref().and_then(py_int),
            };
            let low_ok = at_least.iter().all(|b| bound(b).is_some_and(|limit| limit <= number));
            let high_ok = at_most.as_ref().is_none_or(|b| bound(b).is_some_and(|limit| number <= limit));
            let Some(value) = int_value(number).filter(|_| low_ok && high_ok) else { return refuse() };
            // A refusal by the setting itself lands in Python's `except Exception` too.
            apply(value, &refuse)
        }
        Kind::OnList(list) => {
            let model = py_str(&data);
            let listed = match settings.get(list) {
                Some(Value::Array(models)) => models.iter().any(|m| m.as_str() == Some(model.as_str())),
                _ => false,
            };
            if !listed {
                return error(&TRANSCRIPTION_MODEL_INVALID, current(), None);
            }
            apply(Value::String(model), &internal_error)
        }
    }
}
