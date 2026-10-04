//! What the settings depend on that is not a setting: the installed audio devices, the
//! compute devices, and the language and engine tables. Python read these from other modules
//! (`device_manager`, `translation_languages`, ...); here whoever builds `Settings` supplies them.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

/// The audio devices as the settings validators see them. Asked live, at validation time,
/// because a device can be plugged in between two settings changes.
pub trait Devices: Send + Sync {
    /// Microphone host names (a PortAudio host such as `MME`; one entry under WASAPI).
    fn mic_hosts(&self) -> Vec<String>;
    fn mic_device_names(&self, host: &str) -> Vec<String>;
    fn speaker_device_names(&self) -> Vec<String>;
    fn speaker_hosts(&self) -> Vec<String> {
        vec![crate::audio::devices::WASAPI_HOST.into()]
    }
    fn speaker_device_names_for_host(&self, host: &str) -> Vec<String> {
        if host == crate::audio::devices::WASAPI_HOST || host == "NoHost" {
            self.speaker_device_names()
        } else {
            Vec::new()
        }
    }
    /// `(host, device)` of the default microphone, if there is one.
    fn default_mic(&self) -> Option<(String, String)>;
    fn default_speaker(&self) -> Option<String>;
}

#[derive(Debug, Clone)]
pub struct Paths {
    /// Where `installer_language.txt` is looked for (next to the app).
    pub local: PathBuf,
    pub config: PathBuf,
    pub logs: PathBuf,
}

impl Paths {
    /// config.json and logs/ inside `local`.
    pub fn in_dir(local: impl Into<PathBuf>) -> Self {
        let local = local.into();
        Self { config: local.join("config.json"), logs: local.join("logs"), local }
    }
}

#[derive(Clone)]
pub struct Env {
    pub version: String,
    pub paths: Paths,
    pub devices: Arc<dyn Devices>,
    /// `[{"device": "cpu", "device_index": 0, "device_name": ..., "compute_types": [...]}, ...]`, CPU first.
    pub compute_devices: Vec<serde_json::Value>,
    /// Language name -> its countries (the transcription language table).
    pub transcription_languages: BTreeMap<String, Vec<String>>,
    pub ctranslate2_weight_types: Vec<String>,
    pub whisper_weight_types: Vec<String>,
    pub translation_engines: Vec<String>,
    pub transcription_engines: Vec<String>,
    pub ocr_source_languages: Vec<String>,
    /// The WebSocket token a first run starts with (a config.json that has one overrides it).
    pub websocket_token: String,
}
