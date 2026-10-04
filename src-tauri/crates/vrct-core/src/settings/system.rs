//! The `Env` the app runs with: what is really installed on this machine.
//!
//! * audio devices are asked of WASAPI each time a setting is checked (Windows only; elsewhere
//!   there are none, and the settings fall back to the same `NoHost` / `NoDevice` placeholders
//!   Python used when nothing was plugged in),
//! * the only compute device is the CPU: this build runs CTranslate2 on the CPU alone,
//! * the language, engine and weight tables are the constants in `tables`,
//! * the WebSocket token is a fresh random one, as `secrets.token_urlsafe(32)` gave.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::{json, Value};

use super::env::{Devices, Env, Paths};
use super::tables;
use crate::audio::devices::{DeviceList, NO_HOST, WASAPI_HOST};

/// Compute types the CPU backend (ruy) offers. `auto` first, the rest sorted, as Python listed them.
const CPU_COMPUTE_TYPES: [&str; 4] = ["auto", "float32", "int8", "int8_float32"];

/// The audio devices of this machine, listed afresh on every question: a device can be plugged in
/// between two settings changes.
pub struct SystemDevices;

impl SystemDevices {
    fn list() -> DeviceList {
        #[cfg(windows)]
        {
            crate::audio::wasapi::list_devices().unwrap_or_default()
        }
        #[cfg(not(windows))]
        {
            DeviceList::default()
        }
    }
}

impl Devices for SystemDevices {
    fn mic_hosts(&self) -> Vec<String> {
        #[cfg(windows)]
        { crate::audio::wasapi::host_names() }
        #[cfg(not(windows))]
        { Self::list().hosts().into_iter().map(str::to_string).collect() }
    }

    fn mic_device_names(&self, host: &str) -> Vec<String> {
        #[cfg(windows)]
        if host != WASAPI_HOST && host != NO_HOST {
            return crate::audio::wasapi::list_devices_for_host(host).unwrap_or_default().mic_names().into_iter().map(str::to_owned).collect();
        }
        let list = Self::list();
        // One host exists; asking for another gives nothing, so a saved MME choice is not accepted.
        let known = if list.mics.is_empty() { NO_HOST } else { WASAPI_HOST };
        if host == known {
            list.mic_names().into_iter().map(str::to_string).collect()
        } else {
            Vec::new()
        }
    }

    fn speaker_device_names(&self) -> Vec<String> {
        Self::list().speaker_names().into_iter().map(str::to_string).collect()
    }

    fn speaker_hosts(&self) -> Vec<String> { self.mic_hosts() }

    fn speaker_device_names_for_host(&self, host: &str) -> Vec<String> {
        #[cfg(windows)]
        { crate::audio::wasapi::list_devices_for_host(host).unwrap_or_default().speaker_names().into_iter().map(str::to_owned).collect() }
        #[cfg(not(windows))]
        { let _ = host; self.speaker_device_names() }
    }

    fn default_mic(&self) -> Option<(String, String)> {
        let list = Self::list();
        let name = list.default_mic.clone()?;
        Some((WASAPI_HOST.to_string(), name))
    }

    fn default_speaker(&self) -> Option<String> {
        Self::list().default_speaker
    }
}

/// The compute devices offered: the CPU.
pub fn compute_devices() -> Vec<Value> {
    vec![json!({
        "device": "cpu",
        "device_index": 0,
        "device_name": "cpu",
        "compute_types": CPU_COMPUTE_TYPES,
    })]
}

fn strings(table: &[&str]) -> Vec<String> {
    table.iter().map(|s| s.to_string()).collect()
}

/// 32 random bytes as URL-safe base64 without padding: 43 characters, like `token_urlsafe(32)`.
/// `None` when the system has no randomness to give.
pub fn random_token() -> Option<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).ok()?;
    Some(URL_SAFE_NO_PAD.encode(bytes))
}

/// The environment for `version` with config.json (and the logs folder) in `local`.
pub fn production_env(version: &str, local: impl Into<PathBuf>) -> Env {
    Env {
        version: version.to_string(),
        paths: Paths::in_dir(local),
        devices: Arc::new(SystemDevices),
        compute_devices: compute_devices(),
        transcription_languages: tables::TRANSCRIPTION_LANGUAGES
            .iter()
            .map(|(language, countries)| (language.to_string(), strings(countries)))
            .collect::<BTreeMap<_, _>>(),
        ctranslate2_weight_types: strings(tables::CTRANSLATE2_WEIGHT_TYPES),
        whisper_weight_types: strings(tables::WHISPER_WEIGHT_TYPES),
        translation_engines: strings(tables::TRANSLATION_ENGINES),
        transcription_engines: strings(tables::TRANSCRIPTION_ENGINES),
        ocr_source_languages: strings(tables::OCR_SOURCE_LANGUAGES),
        // Without randomness the token stays empty: the WebSocket server then has nobody it can
        // authenticate, which is safer than a guessable value.
        websocket_token: random_token().unwrap_or_default(),
    }
}
