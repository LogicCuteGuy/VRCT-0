//! What differs between the four directions a message can travel (`models/message_pipeline.py`).

use super::errors::{ErrorInfo, TRANSLATION_VRAM_CHAT, TRANSLATION_VRAM_MIC, TRANSLATION_VRAM_SPEAKER};
use super::format::FormatType;

/// The UI endpoints the pipeline answers on (`run_mapping` in `mainloop.py`).
pub mod endpoints {
    pub const WORD_FILTER: &str = "/run/word_filter";
    pub const TRANSCRIPTION_MIC: &str = "/run/transcription_send_mic_message";
    pub const TRANSCRIPTION_SPEAKER: &str = "/run/transcription_receive_speaker_message";
    pub const TRANSCRIPTION_OCR: &str = "/run/transcription_ocr_message";
    pub const RECOGNITION_ERROR: &str = "/run/transcription_recognition_error";
    pub const ERROR_DEVICE: &str = "/run/error_device";
    pub const ERROR_TRANSLATION_ENGINE: &str = "/run/error_translation_engine";
    pub const ERROR_TRANSLATION_MIC_VRAM: &str = "/run/error_translation_mic_vram_overflow";
    pub const ERROR_TRANSLATION_SPEAKER_VRAM: &str = "/run/error_translation_speaker_vram_overflow";
    pub const ERROR_TRANSLATION_CHAT_VRAM: &str = "/run/error_translation_chat_vram_overflow";
    pub const ENABLE_TRANSLATION: &str = "/run/enable_translation";
    pub const DISABLE_TRANSCRIPTION_SEND: &str = "/set/disable/transcription_send";
    pub const DISABLE_TRANSCRIPTION_RECEIVE: &str = "/set/disable/transcription_receive";
}

/// Which `Model.getXTranslate` translates it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// `getInputTranslate`: my language into each enabled target language.
    Input,
    /// `getOutputTranslate`: the other side's language into mine.
    Output,
}

/// Which `Model.detectRepeat*Message` drops repeats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repeat {
    Send,
    Receive,
}

/// How the result reaches the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Pushed to this endpoint (mic, speaker, OCR).
    Push(&'static str),
    /// Handed back to the caller (chat).
    Return,
}

/// Where the transliteration of my own message is decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnTransliteration {
    /// By the language I selected (mic, chat: I wrote it).
    YourLanguage,
    /// By the language the recogniser detected (speaker, OCR: they said it).
    DetectedLanguage,
}

#[derive(Debug)]
pub struct Spec {
    /// "mic" | "speaker" | "ocr" | "chat": the history source and the log tag.
    pub kind: &'static str,
    pub has_word_filter: bool,
    pub repeat: Option<Repeat>,
    pub translate: Direction,
    /// Several target languages (mic, chat) or just mine (speaker, OCR).
    pub multi_target: bool,
    pub own_transliteration: OwnTransliteration,
    pub vram_error: &'static ErrorInfo,
    pub vram_endpoint: &'static str,
    /// The setting that switches the whole direction on; none for chat.
    pub feature_gate: Option<&'static str>,
    /// The setting that lets it into the VRChat chatbox; none keeps it out (OCR).
    pub osc_gate: Option<&'static str>,
    pub osc_format: FormatType,
    /// "send" | "receive": which large-log layout.
    pub overlay_direction: &'static str,
    pub overlay_small_log: bool,
    pub clipboard: bool,
    pub ws_type: &'static str,
    pub ws_src_languages: &'static str,
    pub ws_dst_languages: &'static str,
    pub logger_prefix: &'static str,
    pub delivery: Delivery,
    /// Added to the pushed payload so the UI can tell where it came from.
    pub payload_source: Option<&'static str>,
}

pub const MIC: Spec = Spec {
    kind: "mic",
    has_word_filter: true,
    repeat: Some(Repeat::Send),
    translate: Direction::Input,
    multi_target: true,
    own_transliteration: OwnTransliteration::YourLanguage,
    vram_error: &TRANSLATION_VRAM_MIC,
    vram_endpoint: endpoints::ERROR_TRANSLATION_MIC_VRAM,
    feature_gate: Some("ENABLE_TRANSCRIPTION_SEND"),
    osc_gate: Some("SEND_MESSAGE_TO_VRC"),
    osc_format: FormatType::Send,
    overlay_direction: "send",
    overlay_small_log: false,
    clipboard: true,
    ws_type: "SENT",
    ws_src_languages: "SELECTED_YOUR_LANGUAGES",
    ws_dst_languages: "SELECTED_TARGET_LANGUAGES",
    logger_prefix: "[SENT]",
    delivery: Delivery::Push(endpoints::TRANSCRIPTION_MIC),
    payload_source: None,
};

pub const SPEAKER: Spec = Spec {
    kind: "speaker",
    has_word_filter: true,
    repeat: Some(Repeat::Receive),
    translate: Direction::Output,
    multi_target: false,
    own_transliteration: OwnTransliteration::DetectedLanguage,
    vram_error: &TRANSLATION_VRAM_SPEAKER,
    vram_endpoint: endpoints::ERROR_TRANSLATION_SPEAKER_VRAM,
    feature_gate: Some("ENABLE_TRANSCRIPTION_RECEIVE"),
    osc_gate: Some("SEND_RECEIVED_MESSAGE_TO_VRC"),
    osc_format: FormatType::Received,
    overlay_direction: "receive",
    overlay_small_log: true,
    clipboard: false,
    ws_type: "RECEIVED",
    ws_src_languages: "SELECTED_TARGET_LANGUAGES",
    ws_dst_languages: "SELECTED_YOUR_LANGUAGES",
    logger_prefix: "[RECEIVED]",
    delivery: Delivery::Push(endpoints::TRANSCRIPTION_SPEAKER),
    payload_source: None,
};

/// Text read off the screen. It never goes to the chatbox: someone else's words would be sent back out
/// under my name, which is spam.
pub const OCR: Spec = Spec {
    kind: "ocr",
    has_word_filter: true,
    // The OCR pipeline drops repeats of the same bubble itself.
    repeat: None,
    translate: Direction::Output,
    multi_target: false,
    own_transliteration: OwnTransliteration::DetectedLanguage,
    vram_error: &TRANSLATION_VRAM_SPEAKER,
    vram_endpoint: endpoints::ERROR_TRANSLATION_SPEAKER_VRAM,
    feature_gate: Some("ENABLE_OCR_CAPTURE"),
    osc_gate: None,
    osc_format: FormatType::Received,
    overlay_direction: "receive",
    overlay_small_log: false,
    clipboard: false,
    ws_type: "RECEIVED",
    ws_src_languages: "SELECTED_TARGET_LANGUAGES",
    ws_dst_languages: "SELECTED_YOUR_LANGUAGES",
    logger_prefix: "[OCR]",
    delivery: Delivery::Push(endpoints::TRANSCRIPTION_OCR),
    payload_source: Some("ocr"),
};

pub const CHAT: Spec = Spec {
    kind: "chat",
    has_word_filter: false,
    repeat: None,
    translate: Direction::Input,
    multi_target: true,
    own_transliteration: OwnTransliteration::YourLanguage,
    vram_error: &TRANSLATION_VRAM_CHAT,
    vram_endpoint: endpoints::ERROR_TRANSLATION_CHAT_VRAM,
    feature_gate: None,
    osc_gate: Some("SEND_MESSAGE_TO_VRC"),
    osc_format: FormatType::Send,
    overlay_direction: "send",
    overlay_small_log: false,
    clipboard: false,
    ws_type: "CHAT",
    ws_src_languages: "SELECTED_YOUR_LANGUAGES",
    ws_dst_languages: "SELECTED_TARGET_LANGUAGES",
    logger_prefix: "[CHAT]",
    delivery: Delivery::Return,
    payload_source: None,
};
