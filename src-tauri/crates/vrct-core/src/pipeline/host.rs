//! What the pipeline needs from the rest of the app: the translator, the outputs, and the UI.
//!
//! In Python these are `model.*` calls and `controller.run`. Each is a method here, so the pipeline's
//! decisions run in tests without a translator, a headset overlay or a VRChat to talk to.

use serde_json::Value;

use super::spec::Direction;

/// What a translator answered: one translation per target, and whether each worked. A `false` means the
/// engine itself failed (a limit, the network), not that it lacks the language pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Translated {
    pub translation: Vec<String>,
    pub success: Vec<bool>,
}

/// A translator that raised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranslateError {
    /// The GPU ran out of memory: `ValueError("VRAM_OUT_OF_MEMORY", message)`.
    VramOutOfMemory(Option<String>),
    /// Anything else, with its text.
    Failed(String),
}

impl TranslateError {
    /// `Model.detectVRAMError`: the message to show when this is an out-of-memory error. Other errors are
    /// recognised by what their text says (CUDA's and cuBLAS's own wording).
    pub fn vram_message(&self) -> Option<String> {
        match self {
            TranslateError::VramOutOfMemory(message) => Some(message.clone().unwrap_or_else(|| "VRAM out of memory".to_string())),
            TranslateError::Failed(text) if text.contains("CUDA out of memory") || text.contains("CUBLAS_STATUS_ALLOC_FAILED") => {
                Some(text.clone())
            }
            TranslateError::Failed(_) => None,
        }
    }
}

impl std::fmt::Display for TranslateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TranslateError::VramOutOfMemory(Some(message)) => f.write_str(message),
            TranslateError::VramOutOfMemory(None) => f.write_str("VRAM_OUT_OF_MEMORY"),
            TranslateError::Failed(text) => f.write_str(text),
        }
    }
}

/// The small overlay's update: `createOverlayImageSmallLog` + `updateOverlaySmallLog`. `message` and
/// `language` are absent when only translations are shown.
pub struct SmallLog<'a> {
    pub message: Option<&'a str>,
    pub language: Option<&'a str>,
    pub translation: &'a [String],
    /// `SELECTED_YOUR_LANGUAGES[tab]`.
    pub your_languages: &'a Value,
    pub transliteration_message: &'a [Value],
    pub transliteration_translation: &'a [Vec<Value>],
}

/// The large overlay's update: `createOverlayImageLargeLog` + `updateOverlayLargeLog`.
pub struct LargeLog<'a> {
    /// "send" | "receive".
    pub direction: &'a str,
    pub message: Option<&'a str>,
    pub language: Option<&'a str>,
    pub translation: &'a [String],
    /// The language slots of the other side of the conversation.
    pub languages: &'a Value,
    pub transliteration_message: &'a [Value],
    pub transliteration_translation: &'a [Vec<Value>],
}

pub trait Host: Send + Sync {
    /// `controller.run`: tells the UI.
    fn run(&self, status: u16, endpoint: &str, payload: Value);
    /// `printLog`.
    fn log(&self, text: &str);

    /// `model.getInputTranslate` / `getOutputTranslate`. `source_language` is the language the text is in,
    /// or none to use the one the settings say.
    fn translate(&self, direction: Direction, message: &str, source_language: Option<&str>) -> Result<Translated, TranslateError>;
    /// `model.convertMessageToTransliteration`.
    fn transliterate(&self, message: &str, hiragana: bool, romaji: bool) -> Vec<Value>;

    /// `model.oscSendMessage`: into the VRChat chatbox.
    fn send_osc(&self, message: &str);
    /// Whether there is an overlay to draw on (`Controller._is_overlay_available`).
    fn overlay_available(&self) -> bool;
    fn overlay_small_log(&self, log: &SmallLog<'_>);
    fn overlay_large_log(&self, log: &LargeLog<'_>);
    /// `model.setCopyToClipboardAndPasteFromClipboard`.
    fn set_clipboard(&self, text: &str);
    /// `model.checkWebSocketServerAlive`.
    fn websocket_alive(&self) -> bool;
    /// `model.websocketSendMessage`.
    fn websocket_send(&self, message: Value);
    /// `model.logger.info`.
    fn log_info(&self, text: &str);

    /// VRChat's "mute self" as last heard; none when not known.
    fn mic_mute_status(&self) -> Option<bool>;
    /// `config.X = value`.
    fn set_setting(&self, name: &str, value: Value);
    /// `Controller.changeToCTranslate2Process`: the engine that failed is replaced by the local one.
    fn fall_back_to_ctranslate2(&self);
    /// `Controller.setDisableTranslation`.
    fn disable_translation(&self);
}
