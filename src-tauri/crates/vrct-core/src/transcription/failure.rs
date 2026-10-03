//! Why a mic or speaker session stopped, as the UI is told (`AudioPipelineFailure` in `errors.py`).
//!
//! The text is a fixed summary per code. What the exception said stays in the log: it can hold paths
//! and device names the UI has no business showing.

use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    AudioOpenError,
    AudioReadError,
    VadInferenceError,
    TranscriberInitError,
    AsrError,
    CleanupTimeout,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::AudioOpenError => "AUDIO_OPEN_ERROR",
            ErrorCode::AudioReadError => "AUDIO_READ_ERROR",
            ErrorCode::VadInferenceError => "VAD_INFERENCE_ERROR",
            ErrorCode::TranscriberInitError => "TRANSCRIBER_INIT_ERROR",
            ErrorCode::AsrError => "ASR_ERROR",
            ErrorCode::CleanupTimeout => "CLEANUP_TIMEOUT",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            ErrorCode::AudioOpenError => "Audio device could not be opened",
            ErrorCode::AudioReadError => "Audio capture failed",
            ErrorCode::VadInferenceError => "Voice activity detection failed",
            ErrorCode::TranscriberInitError => "Speech recognizer initialization failed",
            ErrorCode::AsrError => "Speech recognition failed",
            ErrorCode::CleanupTimeout => "Audio transcription cleanup timed out",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub code: ErrorCode,
    /// `recording`, `vad`, `asr` or `cleanup`.
    pub stage: String,
    /// `mic` or `speaker`.
    pub source: String,
    /// The kind of the exception that caused it, for the log.
    pub exception_type: Option<String>,
}

impl Failure {
    pub fn new(code: ErrorCode, stage: &str, source: &str, exception_type: Option<&str>) -> Self {
        Failure { code, stage: stage.to_string(), source: source.to_string(), exception_type: exception_type.map(str::to_string) }
    }

    pub fn message(&self) -> &'static str {
        self.code.message()
    }

    /// What the UI receives in place of a transcript (`AudioPipelineFailure.to_notification`).
    pub fn notification(&self) -> Value {
        json!({
            "text": "",
            "language": null,
            "recognition_error": true,
            "error_code": self.code.as_str(),
            "stage": self.stage,
            "source": self.source,
            "message": self.message(),
            "recoverable": false,
        })
    }
}
