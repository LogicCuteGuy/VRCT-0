//! How the code that reads settings reaches them while the legacy sidecar still runs.
//!
//! A replica is either a plain copy of what the sidecar reports (`default`), or a view over
//! [`Settings`] (`over`), which is what the app uses. The sidecar still runs its own `Config` for
//! the features not ported yet, so it sends a full snapshot after loading the file and one message
//! per later change; those are adopted into the settings. Ported features read their settings
//! here, and ported endpoints that only return a setting are served from it.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use serde_json::{json, Value};

use crate::protocol::Response;
use crate::router::Router;
use crate::settings::Settings;

/// Environment variable (name, value) that switches the sidecar's bridge on.
pub const BRIDGE_ENV: (&str, &str) = ("VRCT_CONFIG_BRIDGE", "1");

const INTERNAL_PREFIX: &str = "/internal/config/";
const SNAPSHOT_ENDPOINT: &str = "/internal/config/snapshot";
const CHANGED_ENDPOINT: &str = "/internal/config/changed";

#[derive(Default)]
pub struct ConfigReplica {
    values: RwLock<HashMap<String, Value>>,
    settings: Option<Arc<Settings>>,
}

impl ConfigReplica {
    /// A view over the host's settings: reads come from them, sidecar reports are adopted into them.
    pub fn over(settings: Arc<Settings>) -> Self {
        Self { values: RwLock::default(), settings: Some(settings) }
    }

    /// Apply a bridge message from the sidecar. Returns true when the line was
    /// one, so the caller keeps it away from the UI; malformed bridge lines are
    /// swallowed too because they may carry secrets.
    pub fn ingest(&self, response: &Response) -> bool {
        if !response.endpoint.starts_with(INTERNAL_PREFIX) {
            return false;
        }
        if response.status != 200 {
            return true;
        }
        match response.endpoint.as_str() {
            SNAPSHOT_ENDPOINT => {
                if let Value::Object(map) = &response.result {
                    match &self.settings {
                        Some(settings) => {
                            for (key, value) in map {
                                let _ = settings.adopt(key, value.clone());
                            }
                        }
                        None => {
                            *self.values.write().unwrap() =
                                map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                        }
                    }
                }
            }
            CHANGED_ENDPOINT => {
                if let (Some(Value::String(key)), Some(value)) =
                    (response.result.get("key"), response.result.get("value"))
                {
                    match &self.settings {
                        Some(settings) => {
                            let _ = settings.adopt(key, value.clone());
                        }
                        None => {
                            self.values.write().unwrap().insert(key.clone(), value.clone());
                        }
                    }
                }
            }
            _ => {}
        }
        true
    }

    pub fn get(&self, key: &str) -> Option<Value> {
        match &self.settings {
            Some(settings) => settings.get(key),
            None => self.values.read().unwrap().get(key).cloned(),
        }
    }

    pub fn get_str(&self, key: &str) -> Option<String> {
        match self.get(key) {
            Some(Value::String(value)) => Some(value),
            _ => None,
        }
    }

    pub fn contains(&self, key: &str) -> bool {
        self.get(key).is_some()
    }
}

/// `/get/data/*` endpoints that return one persisted setting unchanged, with
/// the config key they read. Derived from `controller._SIMPLE_CONFIG_GETTERS`
/// and `mainloop.mapping`; `test_rust_config_getters.py` fails if this drifts.
pub const SIMPLE_GETTERS: &[(&str, &str)] = &[
    ("/get/data/auto_clear_message_box", "AUTO_CLEAR_MESSAGE_BOX"),
    ("/get/data/auto_mic_select", "AUTO_MIC_SELECT"),
    ("/get/data/auto_speaker_select", "AUTO_SPEAKER_SELECT"),
    ("/get/data/clipboard", "ENABLE_CLIPBOARD"),
    ("/get/data/convert_message_to_hiragana", "CONVERT_MESSAGE_TO_HIRAGANA"),
    ("/get/data/convert_message_to_romaji", "CONVERT_MESSAGE_TO_ROMAJI"),
    ("/get/data/custom_whisper_url", "TRANSCRIPTION_CUSTOM_URL"),
    ("/get/data/font_family", "FONT_FAMILY"),
    ("/get/data/hotkeys", "HOTKEYS"),
    ("/get/data/lmstudio_url", "LMSTUDIO_URL"),
    ("/get/data/logger_feature", "LOGGER_FEATURE"),
    ("/get/data/main_window_geometry", "MAIN_WINDOW_GEOMETRY"),
    ("/get/data/main_window_sidebar_compact_mode", "MAIN_WINDOW_SIDEBAR_COMPACT_MODE"),
    ("/get/data/message_box_ratio", "MESSAGE_BOX_RATIO"),
    ("/get/data/mic_automatic_threshold", "MIC_AUTOMATIC_THRESHOLD"),
    ("/get/data/mic_avg_logprob", "MIC_AVG_LOGPROB"),
    ("/get/data/mic_max_phrases", "MIC_MAX_PHRASES"),
    ("/get/data/mic_no_speech_prob", "MIC_NO_SPEECH_PROB"),
    ("/get/data/mic_phrase_timeout", "MIC_PHRASE_TIMEOUT"),
    ("/get/data/mic_record_timeout", "MIC_RECORD_TIMEOUT"),
    ("/get/data/mic_threshold", "MIC_THRESHOLD"),
    ("/get/data/mic_word_filter", "MIC_WORD_FILTER"),
    ("/get/data/notification_vrc_sfx", "NOTIFICATION_VRC_SFX"),
    ("/get/data/obs_browser_source", "OBS_BROWSER_SOURCE"),
    ("/get/data/obs_browser_source_display_duration", "OBS_BROWSER_SOURCE_DISPLAY_DURATION"),
    ("/get/data/obs_browser_source_fadeout_duration", "OBS_BROWSER_SOURCE_FADEOUT_DURATION"),
    ("/get/data/obs_browser_source_font_color", "OBS_BROWSER_SOURCE_FONT_COLOR"),
    ("/get/data/obs_browser_source_font_outline_color", "OBS_BROWSER_SOURCE_FONT_OUTLINE_COLOR"),
    ("/get/data/obs_browser_source_font_outline_thickness", "OBS_BROWSER_SOURCE_FONT_OUTLINE_THICKNESS"),
    ("/get/data/obs_browser_source_font_size", "OBS_BROWSER_SOURCE_FONT_SIZE"),
    ("/get/data/obs_browser_source_max_messages", "OBS_BROWSER_SOURCE_MAX_MESSAGES"),
    ("/get/data/obs_browser_source_port", "OBS_BROWSER_SOURCE_PORT"),
    ("/get/data/ocr_bubble_min_text_length", "OCR_BUBBLE_MIN_TEXT_LENGTH"),
    ("/get/data/ocr_min_confidence", "OCR_MIN_CONFIDENCE"),
    ("/get/data/ocr_poll_interval_ms", "OCR_POLL_INTERVAL_MS"),
    ("/get/data/ocr_source_language", "OCR_SOURCE_LANGUAGE"),
    ("/get/data/ocr_window_title", "OCR_WINDOW_TITLE"),
    ("/get/data/openai_compatible_url", "OPENAI_COMPATIBLE_URL"),
    ("/get/data/osc_ip_address", "OSC_IP_ADDRESS"),
    ("/get/data/osc_port", "OSC_PORT"),
    ("/get/data/overlay_large_log", "OVERLAY_LARGE_LOG"),
    ("/get/data/overlay_large_log_settings", "OVERLAY_LARGE_LOG_SETTINGS"),
    ("/get/data/overlay_show_only_translated_messages", "OVERLAY_SHOW_ONLY_TRANSLATED_MESSAGES"),
    ("/get/data/overlay_small_log", "OVERLAY_SMALL_LOG"),
    ("/get/data/overlay_small_log_settings", "OVERLAY_SMALL_LOG_SETTINGS"),
    ("/get/data/received_message_format_parts", "RECEIVED_MESSAGE_FORMAT_PARTS"),
    ("/get/data/release_channel", "SELECTED_RELEASE_CHANNEL"),
    ("/get/data/selected_ctranslate2_weight_type", "CTRANSLATE2_WEIGHT_TYPE"),
    ("/get/data/selected_custom_whisper_model", "SELECTED_CUSTOM_WHISPER_MODEL"),
    ("/get/data/selected_deepgram_model", "SELECTED_DEEPGRAM_MODEL"),
    ("/get/data/selected_groq_whisper_model", "SELECTED_GROQ_WHISPER_MODEL"),
    ("/get/data/selected_mic_device", "SELECTED_MIC_DEVICE"),
    ("/get/data/selected_mic_host", "SELECTED_MIC_HOST"),
    ("/get/data/selected_openai_compatible_model", "SELECTED_OPENAI_COMPATIBLE_MODEL"),
    ("/get/data/selected_openai_whisper_model", "SELECTED_OPENAI_WHISPER_MODEL"),
    ("/get/data/selected_speaker_device", "SELECTED_SPEAKER_DEVICE"),
    ("/get/data/selected_tab_no", "SELECTED_TAB_NO"),
    ("/get/data/selected_target_languages", "SELECTED_TARGET_LANGUAGES"),
    ("/get/data/selected_transcription_compute_device", "SELECTED_TRANSCRIPTION_COMPUTE_DEVICE"),
    ("/get/data/selected_transcription_compute_type", "SELECTED_TRANSCRIPTION_COMPUTE_TYPE"),
    ("/get/data/selected_transcription_engine", "SELECTED_TRANSCRIPTION_ENGINE"),
    ("/get/data/selected_translation_compute_device", "SELECTED_TRANSLATION_COMPUTE_DEVICE"),
    ("/get/data/selected_translation_compute_type", "SELECTED_TRANSLATION_COMPUTE_TYPE"),
    ("/get/data/selected_translation_engines", "SELECTED_TRANSLATION_ENGINES"),
    ("/get/data/selected_whisper_weight_type", "WHISPER_WEIGHT_TYPE"),
    ("/get/data/selected_your_languages", "SELECTED_YOUR_LANGUAGES"),
    ("/get/data/send_message_button_type", "SEND_MESSAGE_BUTTON_TYPE"),
    ("/get/data/send_message_format_parts", "SEND_MESSAGE_FORMAT_PARTS"),
    ("/get/data/send_message_to_vrc", "SEND_MESSAGE_TO_VRC"),
    ("/get/data/send_only_translated_messages", "SEND_ONLY_TRANSLATED_MESSAGES"),
    ("/get/data/send_received_message_to_vrc", "SEND_RECEIVED_MESSAGE_TO_VRC"),
    ("/get/data/show_resend_button", "SHOW_RESEND_BUTTON"),
    ("/get/data/speaker_automatic_threshold", "SPEAKER_AUTOMATIC_THRESHOLD"),
    ("/get/data/speaker_avg_logprob", "SPEAKER_AVG_LOGPROB"),
    ("/get/data/speaker_max_phrases", "SPEAKER_MAX_PHRASES"),
    ("/get/data/speaker_no_speech_prob", "SPEAKER_NO_SPEECH_PROB"),
    ("/get/data/speaker_phrase_timeout", "SPEAKER_PHRASE_TIMEOUT"),
    ("/get/data/speaker_record_timeout", "SPEAKER_RECORD_TIMEOUT"),
    ("/get/data/speaker_threshold", "SPEAKER_THRESHOLD"),
    ("/get/data/textbox_ui_scaling", "TEXTBOX_UI_SCALING"),
    ("/get/data/transparency", "TRANSPARENCY"),
    ("/get/data/ui_language", "UI_LANGUAGE"),
    ("/get/data/ui_scaling", "UI_SCALING"),
    ("/get/data/vrc_mic_mute_sync", "VRC_MIC_MUTE_SYNC"),
    ("/get/data/websocket_host", "WEBSOCKET_HOST"),
    ("/get/data/websocket_port", "WEBSOCKET_PORT"),
    ("/get/data/websocket_server", "WEBSOCKET_SERVER"),
];

/// Serve [`SIMPLE_GETTERS`] from the replica. Until the replica holds a key the
/// request still goes to Python, so nothing is answered with a guess.
pub fn register_getters(mut router: Router, replica: &Arc<ConfigReplica>) -> Router {
    for &(endpoint, key) in SIMPLE_GETTERS {
        let gate = Arc::clone(replica);
        let reader = Arc::clone(replica);
        router = router.handle_when(
            endpoint,
            move || gate.contains(key),
            move |_| {
                let value = reader.get(key);
                async move {
                    value.map_or_else(
                        || (500, json!("Config value unavailable")),
                        |value| (200, value),
                    )
                }
            },
        );
    }
    router
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router::{Fallback, ResponseSink};
    use std::sync::Mutex;

    fn snapshot(value: Value) -> Response {
        Response::new(200, SNAPSHOT_ENDPOINT, value)
    }

    fn changed(key: &str, value: Value) -> Response {
        Response::new(200, CHANGED_ENDPOINT, json!({"key": key, "value": value}))
    }

    #[test]
    fn snapshot_replaces_everything_and_changes_update_one_key() {
        let replica = ConfigReplica::default();
        assert!(replica.ingest(&snapshot(json!({"UI_LANGUAGE": "en", "OSC_PORT": 9000}))));
        assert!(replica.ingest(&changed("UI_LANGUAGE", json!("ja"))));
        assert_eq!(replica.get_str("UI_LANGUAGE").as_deref(), Some("ja"));
        assert_eq!(replica.get("OSC_PORT"), Some(json!(9000)));

        assert!(replica.ingest(&snapshot(json!({"OSC_PORT": 9001}))));
        assert!(!replica.contains("UI_LANGUAGE"));
        assert_eq!(replica.get("OSC_PORT"), Some(json!(9001)));
    }

    #[test]
    fn ordinary_responses_are_not_bridge_messages() {
        let replica = ConfigReplica::default();
        assert!(!replica.ingest(&Response::new(200, "/get/data/ui_language", json!("ja"))));
        assert!(!replica.contains("ui_language"));
    }

    #[test]
    fn malformed_bridge_lines_are_swallowed_without_effect() {
        let replica = ConfigReplica::default();
        replica.ingest(&snapshot(json!({"A": 1})));
        assert!(replica.ingest(&snapshot(json!("not an object"))));
        assert!(replica.ingest(&Response::new(200, CHANGED_ENDPOINT, json!({"key": 5, "value": 1}))));
        assert!(replica.ingest(&Response::new(200, CHANGED_ENDPOINT, json!({"key": "B"}))));
        assert!(replica.ingest(&Response::new(500, SNAPSHOT_ENDPOINT, json!({"A": 2}))));
        assert!(replica.ingest(&Response::new(200, "/internal/config/unknown", json!(null))));
        assert_eq!(replica.get("A"), Some(json!(1)));
        assert!(!replica.contains("B"));
    }

    #[test]
    fn get_str_only_returns_strings() {
        let replica = ConfigReplica::default();
        replica.ingest(&snapshot(json!({"S": "x", "N": 1})));
        assert_eq!(replica.get_str("S").as_deref(), Some("x"));
        assert_eq!(replica.get_str("N"), None);
        assert_eq!(replica.get_str("missing"), None);
    }

    #[test]
    fn getter_table_has_no_duplicates() {
        let mut endpoints: Vec<_> = SIMPLE_GETTERS.iter().map(|(e, _)| *e).collect();
        endpoints.sort_unstable();
        let count = endpoints.len();
        endpoints.dedup();
        assert_eq!(endpoints.len(), count);
        assert!(SIMPLE_GETTERS.iter().all(|(e, _)| e.starts_with("/get/data/")));
    }

    #[derive(Default)]
    struct Recorder(Mutex<Vec<Response>>);
    impl ResponseSink for Recorder {
        fn emit(&self, response: Response) {
            self.0.lock().unwrap().push(response);
        }
    }

    #[derive(Default)]
    struct Forwards(Mutex<Vec<String>>);
    impl Fallback for Forwards {
        fn forward(&self, endpoint: &str, _: Option<&str>) -> Result<(), String> {
            self.0.lock().unwrap().push(endpoint.to_string());
            Ok(())
        }
    }

    #[tokio::test]
    async fn getters_wait_for_the_replica_then_serve_from_it() {
        let sink = Arc::new(Recorder::default());
        let forwards = Arc::new(Forwards::default());
        let replica = Arc::new(ConfigReplica::default());
        let router = Arc::new(register_getters(
            Router::new(sink.clone()).with_fallback(forwards.clone()),
            &replica,
        ));

        router.dispatch("/get/data/ui_language".into(), None);
        assert_eq!(forwards.0.lock().unwrap().as_slice(), ["/get/data/ui_language"]);

        replica.ingest(&snapshot(json!({"UI_LANGUAGE": "ko"})));
        router.dispatch("/get/data/ui_language".into(), None);
        for _ in 0..200 {
            if !sink.0.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        {
            let responses = sink.0.lock().unwrap();
            assert_eq!(responses.len(), 1);
            assert_eq!(responses[0], Response::new(200, "/get/data/ui_language", json!("ko")));
        }

        // A key the replica lacks still goes to Python.
        router.dispatch("/get/data/osc_port".into(), None);
        assert_eq!(forwards.0.lock().unwrap().len(), 2);
    }
}
