//! `Config.init_config`: every setting's value before config.json is read.

use serde_json::{json, Map, Value};

use super::env::Env;
use super::validators::State;

/// The tabs every language and engine setting is kept per.
const TABS: [&str; 3] = ["1", "2", "3"];

fn strings(list: &[String]) -> Value {
    Value::Array(list.iter().map(|s| Value::String(s.clone())).collect())
}

/// `{name: false}` for each name: the "is it downloaded / usable" status tables.
fn all_false(names: &[String]) -> Value {
    Value::Object(names.iter().map(|name| (name.clone(), Value::Bool(false))).collect::<Map<_, _>>())
}

fn overlay(tracker: &str) -> Value {
    json!({
        "x_pos": 0.0, "y_pos": 0.0, "z_pos": 0.0,
        "x_rotation": 0.0, "y_rotation": 0.0, "z_rotation": 0.0,
        "display_duration": 5, "fadeout_duration": 2,
        "opacity": 1.0, "ui_scaling": 1.0,
        "tracker": tracker,
    })
}

fn message_format() -> Value {
    json!({
        "message": {"prefix": "", "suffix": ""},
        "separator": "\n",
        "translation": {"prefix": "", "separator": "\n", "suffix": ""},
        "translation_first": false,
    })
}

pub fn initial_state(env: &Env) -> State {
    let mut state = State::new();
    let mut set = |name: &str, value: Value| {
        state.insert(name.to_string(), value);
    };
    let empty_list = || json!([]);

    set("VERSION", json!(env.version));
    set("PATH_LOCAL", json!(env.paths.local.to_string_lossy()));
    set("PATH_CONFIG", json!(env.paths.config.to_string_lossy()));
    set("PATH_LOGS", json!(env.paths.logs.to_string_lossy()));
    set("GITHUB_URL", json!("https://api.github.com/repos/misyaguziya/VRCT/releases/latest"));
    set("GITHUB_RELEASES_LIST_URL", json!("https://api.github.com/repos/misyaguziya/VRCT/releases"));
    set("MIN_SUPPORTED_VERSION", json!("3.4.3"));
    set("SELECTABLE_RELEASE_CHANNEL_LIST", json!(["stable", "beta"]));
    set("SELECTED_RELEASE_CHANNEL", json!("stable"));
    set("MAX_MIC_THRESHOLD", json!(2000));
    set("MAX_SPEAKER_THRESHOLD", json!(2000));
    set("WATCHDOG_TIMEOUT", json!(60));
    set("WATCHDOG_INTERVAL", json!(20));
    set("SELECTABLE_TAB_NO_LIST", json!(TABS));
    set("SELECTABLE_CTRANSLATE2_WEIGHT_TYPE_LIST", strings(&env.ctranslate2_weight_types));
    set("SELECTABLE_WHISPER_WEIGHT_TYPE_LIST", strings(&env.whisper_weight_types));
    set("SELECTABLE_TRANSLATION_ENGINE_LIST", strings(&env.translation_engines));
    set("SELECTABLE_TRANSCRIPTION_ENGINE_LIST", strings(&env.transcription_engines));
    set("SELECTABLE_UI_LANGUAGE_LIST", json!(["en", "ja", "ko", "zh-Hant", "zh-Hans"]));
    set("SELECTABLE_OCR_SOURCE_LANGUAGE_LIST", strings(&env.ocr_source_languages));
    set("SELECTABLE_COMPUTE_DEVICE_LIST", Value::Array(env.compute_devices.clone()));
    let has_cuda = env.compute_devices.iter().any(|device| device.get("device").and_then(Value::as_str) == Some("cuda"));
    set("COMPUTE_MODE", json!(if has_cuda { "cuda" } else { "cpu" }));
    set("SEND_MESSAGE_BUTTON_TYPE_LIST", json!(["show", "hide", "show_and_disable_enter_key"]));

    for flag in [
        "ENABLE_TRANSLATION",
        "ENABLE_TRANSCRIPTION_SEND",
        "ENABLE_TRANSCRIPTION_RECEIVE",
        "ENABLE_OCR_CAPTURE",
        "ENABLE_FOREGROUND",
        "ENABLE_CHECK_ENERGY_SEND",
        "ENABLE_CHECK_ENERGY_RECEIVE",
    ] {
        set(flag, json!(false));
    }
    set("SELECTABLE_CTRANSLATE2_WEIGHT_TYPE_DICT", all_false(&env.ctranslate2_weight_types));
    set("SELECTABLE_WHISPER_WEIGHT_TYPE_DICT", all_false(&env.whisper_weight_types));
    set("SELECTABLE_TRANSLATION_ENGINE_STATUS", all_false(&env.translation_engines));
    set("SELECTABLE_TRANSCRIPTION_ENGINE_STATUS", all_false(&env.transcription_engines));
    for list in [
        "SELECTABLE_PLAMO_MODEL_LIST",
        "SELECTABLE_GEMINI_MODEL_LIST",
        "SELECTABLE_OPENAI_MODEL_LIST",
        "SELECTABLE_GROQ_MODEL_LIST",
        "SELECTABLE_OPENROUTER_MODEL_LIST",
        "SELECTABLE_LMSTUDIO_MODEL_LIST",
        "SELECTABLE_OPENAI_COMPATIBLE_MODEL_LIST",
        "SELECTABLE_OLLAMA_MODEL_LIST",
        "SELECTABLE_GROQ_WHISPER_MODEL_LIST",
        "SELECTABLE_OPENAI_WHISPER_MODEL_LIST",
        "SELECTABLE_CUSTOM_WHISPER_MODEL_LIST",
        "SELECTABLE_DEEPGRAM_MODEL_LIST",
    ] {
        set(list, empty_list());
    }
    set("DEEPGRAM_MODEL_LANGUAGES", json!({}));

    // Main window
    set("SELECTED_TAB_NO", json!("1"));
    set("SELECTED_TRANSLATION_ENGINES", json!({"1": "CTranslate2", "2": "CTranslate2", "3": "CTranslate2"}));
    let yours = json!({"1": {"language": "Japanese", "country": "Japan", "enable": true}});
    set("SELECTED_YOUR_LANGUAGES", json!({"1": yours.clone(), "2": yours.clone(), "3": yours}));
    set("SELECTED_TAB_TARGET_LANGUAGES_NO_LIST", json!(TABS));
    let target = |enable: bool| json!({"language": "English", "country": "United States", "enable": enable});
    let slots = json!({"1": target(true), "2": target(false), "3": target(false)});
    set("SELECTED_TARGET_LANGUAGES", json!({"1": slots.clone(), "2": slots.clone(), "3": slots}));
    set("SELECTED_TRANSCRIPTION_ENGINE", json!("Google"));
    set("CONVERT_MESSAGE_TO_ROMAJI", json!(false));
    set("CONVERT_MESSAGE_TO_HIRAGANA", json!(false));
    set("MAIN_WINDOW_SIDEBAR_COMPACT_MODE", json!(false));

    // Config window
    set("TRANSPARENCY", json!(100));
    set("UI_SCALING", json!(100));
    set("TEXTBOX_UI_SCALING", json!(100));
    set("MESSAGE_BOX_RATIO", json!(10));
    set("SEND_MESSAGE_BUTTON_TYPE", json!("show"));
    set("SHOW_RESEND_BUTTON", json!(false));
    set("FONT_FAMILY", json!("Yu Gothic UI"));
    set("UI_LANGUAGE", json!("en"));
    set("MAIN_WINDOW_GEOMETRY", json!({"x_pos": 0, "y_pos": 0, "width": 870, "height": 654}));
    set("AUTO_MIC_SELECT", json!(true));
    let (host, device) = env.devices.default_mic().unwrap_or(("NoHost".into(), "NoDevice".into()));
    set("SELECTED_MIC_HOST", json!(host));
    set("SELECTED_MIC_DEVICE", json!(device));
    set("MIC_THRESHOLD", json!(300));
    set("MIC_AUTOMATIC_THRESHOLD", json!(false));
    set("MIC_RECORD_TIMEOUT", json!(3));
    set("MIC_PHRASE_TIMEOUT", json!(3));
    set("MIC_MAX_PHRASES", json!(10));
    set("MIC_WORD_FILTER", json!([]));
    set(
        "HOTKEYS",
        json!({
            "toggle_vrct_visibility": null,
            "toggle_translation": null,
            "toggle_transcription_send": null,
            "toggle_transcription_receive": null,
        }),
    );
    set("MIC_AVG_LOGPROB", json!(-0.8));
    set("MIC_NO_SPEECH_PROB", json!(0.6));
    set("MIC_NO_REPEAT_NGRAM_SIZE", json!(0));
    set("AUTO_SPEAKER_SELECT", json!(true));
    set("SELECTED_SPEAKER_DEVICE", json!(env.devices.default_speaker().unwrap_or_else(|| "NoDevice".into())));
    set("SPEAKER_THRESHOLD", json!(300));
    set("SPEAKER_AUTOMATIC_THRESHOLD", json!(false));
    set("SPEAKER_RECORD_TIMEOUT", json!(3));
    set("SPEAKER_PHRASE_TIMEOUT", json!(3));
    set("SPEAKER_MAX_PHRASES", json!(10));
    set("SPEAKER_AVG_LOGPROB", json!(-0.8));
    set("SPEAKER_NO_SPEECH_PROB", json!(0.6));
    set("SPEAKER_NO_REPEAT_NGRAM_SIZE", json!(0));
    set("MIC_ENABLE_VAD", json!(false));
    set("SPEAKER_ENABLE_VAD", json!(false));
    set("OSC_IP_ADDRESS", json!("127.0.0.1"));
    set("OSC_PORT", json!(9000));
    set(
        "AUTH_KEYS",
        json!({
            "DeepL_API": null, "Plamo_API": null, "Gemini_API": null, "OpenAI_API": null,
            "OpenAI_Compatible": null, "Groq_API": null, "OpenRouter_API": null,
        }),
    );
    set(
        "TRANSCRIPTION_AUTH_KEYS",
        json!({"Groq_Whisper": null, "OpenAI_Whisper": null, "Custom_Whisper": null, "Deepgram": null}),
    );
    set("TRANSCRIPTION_CUSTOM_URL", json!(""));
    let first_compute_device = env.compute_devices.first().cloned().unwrap_or(Value::Null);
    set("SELECTED_TRANSLATION_COMPUTE_DEVICE", first_compute_device.clone());
    set("SELECTED_TRANSCRIPTION_COMPUTE_DEVICE", first_compute_device);
    set("CTRANSLATE2_WEIGHT_TYPE", json!("nllb-200-distilled-600M-ct2-int8"));
    for model in [
        "SELECTED_PLAMO_MODEL",
        "SELECTED_GEMINI_MODEL",
        "SELECTED_OPENAI_MODEL",
        "SELECTED_GROQ_MODEL",
        "SELECTED_OPENROUTER_MODEL",
        "SELECTED_LMSTUDIO_MODEL",
        "SELECTED_OPENAI_COMPATIBLE_MODEL",
        "SELECTED_OLLAMA_MODEL",
        "SELECTED_GROQ_WHISPER_MODEL",
        "SELECTED_OPENAI_WHISPER_MODEL",
        "SELECTED_CUSTOM_WHISPER_MODEL",
        "SELECTED_DEEPGRAM_MODEL",
    ] {
        set(model, Value::Null);
    }
    set("LMSTUDIO_URL", json!("http://127.0.0.1:1234/v1"));
    set("OPENAI_COMPATIBLE_URL", json!("https://api.openai.com/v1"));
    set("SELECTED_TRANSLATION_COMPUTE_TYPE", json!("auto"));
    set("WHISPER_WEIGHT_TYPE", json!("base"));
    set("SELECTED_TRANSCRIPTION_COMPUTE_TYPE", json!("auto"));
    set("AUTO_CLEAR_MESSAGE_BOX", json!(true));
    set("SEND_ONLY_TRANSLATED_MESSAGES", json!(false));
    set("OVERLAY_SMALL_LOG", json!(false));
    set("OVERLAY_SMALL_LOG_SETTINGS", overlay("HMD"));
    set("OVERLAY_LARGE_LOG", json!(false));
    set("OVERLAY_LARGE_LOG_SETTINGS", overlay("LeftHand"));
    set("OVERLAY_SHOW_ONLY_TRANSLATED_MESSAGES", json!(false));
    set("SEND_MESSAGE_TO_VRC", json!(true));
    set("SEND_RECEIVED_MESSAGE_TO_VRC", json!(false));
    set("LOGGER_FEATURE", json!(false));
    set("VRC_MIC_MUTE_SYNC", json!(false));
    set("NOTIFICATION_VRC_SFX", json!(true));
    set("SEND_MESSAGE_FORMAT_PARTS", message_format());
    set("RECEIVED_MESSAGE_FORMAT_PARTS", message_format());
    set("WEBSOCKET_SERVER", json!(false));
    set("WEBSOCKET_HOST", json!("127.0.0.1"));
    set("WEBSOCKET_PORT", json!(2231));
    set("WEBSOCKET_AUTH_TOKEN", json!(env.websocket_token));
    set("OBS_BROWSER_SOURCE", json!(false));
    set("OBS_BROWSER_SOURCE_PORT", json!(2232));
    set("OBS_BROWSER_SOURCE_MAX_MESSAGES", json!(14));
    set("OBS_BROWSER_SOURCE_DISPLAY_DURATION", json!(60));
    set("OBS_BROWSER_SOURCE_FADEOUT_DURATION", json!(12));
    set("OBS_BROWSER_SOURCE_FONT_SIZE", json!(40));
    set("OBS_BROWSER_SOURCE_FONT_COLOR", json!("#FFFFFF"));
    set("OBS_BROWSER_SOURCE_FONT_OUTLINE_THICKNESS", json!(3));
    set("OBS_BROWSER_SOURCE_FONT_OUTLINE_COLOR", json!("#000000"));
    set("ENABLE_CLIPBOARD", json!(false));
    set("ENABLE_TELEMETRY", json!(true));

    // OCR (VRChat chat bubbles)
    set("OCR_SOURCE_LANGUAGE", json!("auto"));
    set("OCR_WINDOW_TITLE", json!("VRChat"));
    set("OCR_POLL_INTERVAL_MS", json!(750));
    set("OCR_MIN_CONFIDENCE", json!(0.85));
    set("OCR_BUBBLE_MIN_TEXT_LENGTH", json!(2));

    state
}
