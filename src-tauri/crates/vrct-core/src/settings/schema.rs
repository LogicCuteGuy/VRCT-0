//! Every setting of `config.py`'s `Config`, in the order Python declares them (which is the
//! order config.json is written in), with how a new value is checked.

use serde_json::Value;

use super::pyvalue::{contains, is_float, is_int, is_number, to_f64};
use super::validators::{self, get, State, Validator};
use super::Env;

/// `ManagedProperty(type_=...)`: a value of another type is rejected, `null` always passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ty {
    Any,
    Bool,
    Int,
    /// `(int, float)`
    Number,
    Str,
    Dict,
    List,
}

impl Ty {
    fn accepts(self, value: &Value) -> bool {
        match self {
            Ty::Any => true,
            Ty::Bool => value.is_boolean(),
            Ty::Int => is_int(value),
            Ty::Number => is_number(value),
            Ty::Str => value.is_string(),
            Ty::Dict => value.is_object(),
            Ty::List => value.is_array(),
        }
    }
}

/// `ManagedProperty(allowed=...)`, checked after the type, `null` included.
#[derive(Clone, Copy)]
pub enum Allowed {
    Anything,
    /// `v in inst.<list>`
    InList(&'static str),
    /// Anything when the list is still empty (models not fetched yet) or the value is `null`.
    InListOnceKnown(&'static str),
    WebsocketHost,
    NonEmptyText,
    /// `lo <= v <= hi`
    Between(f64, f64),
}

impl Allowed {
    fn accepts(self, value: &Value, state: &State) -> bool {
        match self {
            Allowed::Anything => true,
            Allowed::InList(list) => get(state, list).as_array().is_some_and(|list| contains(list, value)),
            Allowed::InListOnceKnown(list) => match get(state, list).as_array() {
                Some(list) if !list.is_empty() => value.is_null() || contains(list, value),
                _ => true,
            },
            Allowed::WebsocketHost => value
                .as_str()
                .and_then(|text| text.parse::<std::net::IpAddr>().ok())
                .is_some_and(|ip| !ip.is_unspecified()),
            Allowed::NonEmptyText => value.as_str().is_some_and(|text| !text.is_empty()),
            Allowed::Between(lo, hi) => {
                (is_int(value) || is_float(value)) && to_f64(value).is_some_and(|v| lo <= v && v <= hi)
            }
        }
    }
}

#[derive(Clone, Copy)]
pub enum Rule {
    /// Written by the app only (versions, paths, the selectable lists); `set` refuses it.
    ReadOnly,
    Managed(Ty, Allowed),
    Validated(Validator),
}

#[derive(Clone, Copy)]
pub struct Prop {
    pub name: &'static str,
    /// In config.json.
    pub persisted: bool,
    /// Written at once instead of after the debounce.
    pub immediate: bool,
    pub rule: Rule,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Rejected {
    ReadOnly,
    /// What `ConfigValidationError` was in Python.
    Invalid,
}

impl Prop {
    /// The value to store for `value`, or why not.
    pub fn check(&self, value: &Value, state: &State, env: &Env) -> Result<Value, Rejected> {
        match self.rule {
            Rule::ReadOnly => Err(Rejected::ReadOnly),
            Rule::Managed(ty, allowed) => {
                if !value.is_null() && !ty.accepts(value) {
                    return Err(Rejected::Invalid);
                }
                if !allowed.accepts(value, state) {
                    return Err(Rejected::Invalid);
                }
                Ok(value.clone())
            }
            Rule::Validated(validator) => validator(value, state, env).ok_or(Rejected::Invalid),
        }
    }

    pub fn is_read_only(&self) -> bool {
        matches!(self.rule, Rule::ReadOnly)
    }
}

const fn ro(name: &'static str) -> Prop {
    Prop { name, persisted: false, immediate: false, rule: Rule::ReadOnly }
}

/// Not persisted, but writable at run time.
const fn rt(name: &'static str, ty: Ty) -> Prop {
    Prop { name, persisted: false, immediate: false, rule: Rule::Managed(ty, Allowed::Anything) }
}

const fn typed(name: &'static str, ty: Ty) -> Prop {
    Prop { name, persisted: true, immediate: false, rule: Rule::Managed(ty, Allowed::Anything) }
}

const fn checked(name: &'static str, ty: Ty, allowed: Allowed) -> Prop {
    Prop { name, persisted: true, immediate: false, rule: Rule::Managed(ty, allowed) }
}

const fn validated(name: &'static str, validator: Validator) -> Prop {
    Prop { name, persisted: true, immediate: false, rule: Rule::Validated(validator) }
}

const fn now(prop: Prop) -> Prop {
    Prop { immediate: true, ..prop }
}

const fn model(name: &'static str, list: &'static str) -> Prop {
    checked(name, Ty::Str, Allowed::InListOnceKnown(list))
}

use Allowed::{Between, InList, NonEmptyText, WebsocketHost};
use Ty::{Bool, Dict, Int, List, Number, Str};

pub static PROPS: &[Prop] = &[
    ro("VERSION"),
    ro("PATH_LOCAL"),
    ro("PATH_CONFIG"),
    ro("PATH_LOGS"),
    ro("GITHUB_URL"),
    ro("GITHUB_RELEASES_LIST_URL"),
    ro("MIN_SUPPORTED_VERSION"),
    ro("SELECTABLE_RELEASE_CHANNEL_LIST"),
    ro("MAX_MIC_THRESHOLD"),
    ro("MAX_SPEAKER_THRESHOLD"),
    ro("WATCHDOG_TIMEOUT"),
    ro("WATCHDOG_INTERVAL"),
    ro("SELECTABLE_TAB_NO_LIST"),
    ro("SELECTED_TAB_TARGET_LANGUAGES_NO_LIST"),
    ro("SELECTABLE_CTRANSLATE2_WEIGHT_TYPE_LIST"),
    ro("SELECTABLE_WHISPER_WEIGHT_TYPE_LIST"),
    ro("SELECTABLE_TRANSLATION_ENGINE_LIST"),
    ro("SELECTABLE_TRANSCRIPTION_ENGINE_LIST"),
    ro("SELECTABLE_UI_LANGUAGE_LIST"),
    ro("SELECTABLE_OCR_SOURCE_LANGUAGE_LIST"),
    ro("COMPUTE_MODE"),
    ro("SELECTABLE_COMPUTE_DEVICE_LIST"),
    ro("SEND_MESSAGE_BUTTON_TYPE_LIST"),
    // Run-time state, never written to config.json.
    rt("ENABLE_TRANSLATION", Bool),
    rt("ENABLE_TRANSCRIPTION_SEND", Bool),
    rt("ENABLE_TRANSCRIPTION_RECEIVE", Bool),
    rt("ENABLE_OCR_CAPTURE", Bool),
    rt("ENABLE_FOREGROUND", Bool),
    rt("ENABLE_CHECK_ENERGY_SEND", Bool),
    rt("ENABLE_CHECK_ENERGY_RECEIVE", Bool),
    rt("SELECTABLE_CTRANSLATE2_WEIGHT_TYPE_DICT", Dict),
    rt("SELECTABLE_WHISPER_WEIGHT_TYPE_DICT", Dict),
    rt("SELECTABLE_TRANSLATION_ENGINE_STATUS", Dict),
    rt("SELECTABLE_TRANSCRIPTION_ENGINE_STATUS", Dict),
    rt("SELECTABLE_PLAMO_MODEL_LIST", List),
    rt("SELECTABLE_GEMINI_MODEL_LIST", List),
    rt("SELECTABLE_OPENAI_MODEL_LIST", List),
    rt("SELECTABLE_GROQ_MODEL_LIST", List),
    rt("SELECTABLE_OPENROUTER_MODEL_LIST", List),
    rt("SELECTABLE_LMSTUDIO_MODEL_LIST", List),
    rt("SELECTABLE_OPENAI_COMPATIBLE_MODEL_LIST", List),
    rt("SELECTABLE_OLLAMA_MODEL_LIST", List),
    rt("SELECTABLE_GROQ_WHISPER_MODEL_LIST", List),
    rt("SELECTABLE_OPENAI_WHISPER_MODEL_LIST", List),
    rt("SELECTABLE_CUSTOM_WHISPER_MODEL_LIST", List),
    rt("SELECTABLE_DEEPGRAM_MODEL_LIST", List),
    rt("DEEPGRAM_MODEL_LANGUAGES", Dict),
    // config.json, in file order.
    typed("CONVERT_MESSAGE_TO_ROMAJI", Bool),
    typed("CONVERT_MESSAGE_TO_HIRAGANA", Bool),
    typed("MAIN_WINDOW_SIDEBAR_COMPACT_MODE", Bool),
    typed("TRANSPARENCY", Int),
    typed("UI_SCALING", Int),
    typed("TEXTBOX_UI_SCALING", Int),
    now(typed("MESSAGE_BOX_RATIO", Number)),
    checked("SEND_MESSAGE_BUTTON_TYPE", Str, InList("SEND_MESSAGE_BUTTON_TYPE_LIST")),
    typed("SHOW_RESEND_BUTTON", Bool),
    typed("FONT_FAMILY", Str),
    checked("UI_LANGUAGE", Str, InList("SELECTABLE_UI_LANGUAGE_LIST")),
    now(validated("MAIN_WINDOW_GEOMETRY", validators::main_window_geometry)),
    typed("MIC_THRESHOLD", Int),
    typed("MIC_AUTOMATIC_THRESHOLD", Bool),
    typed("MIC_RECORD_TIMEOUT", Int),
    typed("MIC_PHRASE_TIMEOUT", Int),
    typed("MIC_MAX_PHRASES", Int),
    typed("MIC_AVG_LOGPROB", Number),
    typed("MIC_NO_SPEECH_PROB", Number),
    typed("MIC_NO_REPEAT_NGRAM_SIZE", Int),
    now(validated("HOTKEYS", validators::hotkeys)),
    typed("SPEAKER_THRESHOLD", Int),
    typed("SPEAKER_AUTOMATIC_THRESHOLD", Bool),
    typed("SPEAKER_RECORD_TIMEOUT", Int),
    typed("SPEAKER_PHRASE_TIMEOUT", Int),
    typed("SPEAKER_MAX_PHRASES", Int),
    typed("SPEAKER_AVG_LOGPROB", Number),
    typed("SPEAKER_NO_SPEECH_PROB", Number),
    typed("SPEAKER_NO_REPEAT_NGRAM_SIZE", Int),
    typed("MIC_ENABLE_VAD", Bool),
    typed("SPEAKER_ENABLE_VAD", Bool),
    validated("AUTH_KEYS", validators::auth_keys),
    typed("LMSTUDIO_URL", Str),
    typed("OPENAI_COMPATIBLE_URL", Str),
    validated("TRANSCRIPTION_AUTH_KEYS", validators::transcription_auth_keys),
    typed("TRANSCRIPTION_CUSTOM_URL", Str),
    validated("SELECTED_TRANSCRIPTION_COMPUTE_TYPE", validators::selected_transcription_compute_type),
    validated("OVERLAY_SMALL_LOG_SETTINGS", validators::overlay_small),
    validated("OVERLAY_LARGE_LOG_SETTINGS", validators::overlay_large),
    validated("SEND_MESSAGE_FORMAT_PARTS", validators::message_format),
    validated("RECEIVED_MESSAGE_FORMAT_PARTS", validators::message_format),
    typed("WEBSOCKET_SERVER", Bool),
    typed("OSC_IP_ADDRESS", Str),
    typed("OSC_PORT", Int),
    typed("AUTO_CLEAR_MESSAGE_BOX", Bool),
    typed("SEND_ONLY_TRANSLATED_MESSAGES", Bool),
    typed("OVERLAY_SMALL_LOG", Bool),
    typed("OVERLAY_LARGE_LOG", Bool),
    typed("OVERLAY_SHOW_ONLY_TRANSLATED_MESSAGES", Bool),
    typed("SEND_MESSAGE_TO_VRC", Bool),
    typed("SEND_RECEIVED_MESSAGE_TO_VRC", Bool),
    typed("LOGGER_FEATURE", Bool),
    typed("VRC_MIC_MUTE_SYNC", Bool),
    typed("NOTIFICATION_VRC_SFX", Bool),
    checked("WEBSOCKET_HOST", Str, WebsocketHost),
    typed("WEBSOCKET_PORT", Int),
    typed("WEBSOCKET_AUTH_TOKEN", Str),
    typed("OBS_BROWSER_SOURCE", Bool),
    typed("OBS_BROWSER_SOURCE_PORT", Int),
    typed("OBS_BROWSER_SOURCE_MAX_MESSAGES", Int),
    typed("OBS_BROWSER_SOURCE_DISPLAY_DURATION", Int),
    typed("OBS_BROWSER_SOURCE_FADEOUT_DURATION", Int),
    typed("OBS_BROWSER_SOURCE_FONT_SIZE", Int),
    typed("OBS_BROWSER_SOURCE_FONT_COLOR", Str),
    typed("OBS_BROWSER_SOURCE_FONT_OUTLINE_THICKNESS", Int),
    typed("OBS_BROWSER_SOURCE_FONT_OUTLINE_COLOR", Str),
    typed("ENABLE_TELEMETRY", Bool),
    checked("SELECTED_TAB_NO", Str, InList("SELECTABLE_TAB_NO_LIST")),
    checked("SELECTED_TRANSCRIPTION_ENGINE", Str, InList("SELECTABLE_TRANSCRIPTION_ENGINE_LIST")),
    checked("SELECTED_RELEASE_CHANNEL", Str, InList("SELECTABLE_RELEASE_CHANNEL_LIST")),
    checked("CTRANSLATE2_WEIGHT_TYPE", Str, InList("SELECTABLE_CTRANSLATE2_WEIGHT_TYPE_LIST")),
    checked("WHISPER_WEIGHT_TYPE", Str, InList("SELECTABLE_WHISPER_WEIGHT_TYPE_LIST")),
    model("SELECTED_PLAMO_MODEL", "SELECTABLE_PLAMO_MODEL_LIST"),
    model("SELECTED_GEMINI_MODEL", "SELECTABLE_GEMINI_MODEL_LIST"),
    model("SELECTED_OPENAI_MODEL", "SELECTABLE_OPENAI_MODEL_LIST"),
    model("SELECTED_GROQ_MODEL", "SELECTABLE_GROQ_MODEL_LIST"),
    model("SELECTED_OPENROUTER_MODEL", "SELECTABLE_OPENROUTER_MODEL_LIST"),
    model("SELECTED_LMSTUDIO_MODEL", "SELECTABLE_LMSTUDIO_MODEL_LIST"),
    model("SELECTED_OPENAI_COMPATIBLE_MODEL", "SELECTABLE_OPENAI_COMPATIBLE_MODEL_LIST"),
    model("SELECTED_OLLAMA_MODEL", "SELECTABLE_OLLAMA_MODEL_LIST"),
    model("SELECTED_GROQ_WHISPER_MODEL", "SELECTABLE_GROQ_WHISPER_MODEL_LIST"),
    model("SELECTED_OPENAI_WHISPER_MODEL", "SELECTABLE_OPENAI_WHISPER_MODEL_LIST"),
    model("SELECTED_CUSTOM_WHISPER_MODEL", "SELECTABLE_CUSTOM_WHISPER_MODEL_LIST"),
    model("SELECTED_DEEPGRAM_MODEL", "SELECTABLE_DEEPGRAM_MODEL_LIST"),
    validated("MIC_WORD_FILTER", validators::mic_word_filter),
    validated("SELECTED_TRANSLATION_ENGINES", validators::selected_translation_engines),
    validated("SELECTED_YOUR_LANGUAGES", validators::selected_your_languages),
    validated("SELECTED_TARGET_LANGUAGES", validators::selected_target_languages),
    validated("SELECTED_TRANSLATION_COMPUTE_TYPE", validators::selected_translation_compute_type),
    typed("AUTO_MIC_SELECT", Bool),
    typed("AUTO_SPEAKER_SELECT", Bool),
    validated("SELECTED_MIC_HOST", validators::selected_mic_host),
    validated("SELECTED_MIC_DEVICE", validators::selected_mic_device),
    validated("SELECTED_SPEAKER_HOST", validators::selected_speaker_host),
    validated("SELECTED_SPEAKER_DEVICE", validators::selected_speaker_device),
    validated("SELECTED_TRANSLATION_COMPUTE_DEVICE", validators::compute_device),
    validated("SELECTED_TRANSCRIPTION_COMPUTE_DEVICE", validators::compute_device),
    typed("ENABLE_CLIPBOARD", Bool),
    checked("OCR_SOURCE_LANGUAGE", Str, InList("SELECTABLE_OCR_SOURCE_LANGUAGE_LIST")),
    checked("OCR_WINDOW_TITLE", Str, NonEmptyText),
    checked("OCR_POLL_INTERVAL_MS", Int, Between(100.0, 5000.0)),
    checked("OCR_MIN_CONFIDENCE", Number, Between(0.1, 0.99)),
    checked("OCR_BUBBLE_MIN_TEXT_LENGTH", Int, Between(1.0, 50.0)),
];

pub fn find(name: &str) -> Option<&'static Prop> {
    PROPS.iter().find(|prop| prop.name == name)
}
