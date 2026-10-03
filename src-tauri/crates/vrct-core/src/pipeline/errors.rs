//! The error replies the pipeline sends (`VRCTError.create_error_response` for the translation codes).

use serde_json::{json, Value};

#[derive(Debug)]
pub struct ErrorInfo {
    pub code: &'static str,
    pub message: &'static str,
    pub category: &'static str,
    pub severity: &'static str,
}

pub const TRANSLATION_ENGINE_LIMIT: ErrorInfo = ErrorInfo {
    code: "TRANSLATION_ENGINE_LIMIT",
    message: "Translation engine limit error",
    category: "translation",
    severity: "warning",
};
pub const TRANSLATION_VRAM_CHAT: ErrorInfo = ErrorInfo {
    code: "TRANSLATION_VRAM_CHAT",
    message: "VRAM out of memory during translation of chat",
    category: "translation",
    severity: "critical",
};
pub const TRANSLATION_VRAM_MIC: ErrorInfo = ErrorInfo {
    code: "TRANSLATION_VRAM_MIC",
    message: "VRAM out of memory during translation of mic",
    category: "translation",
    severity: "critical",
};
pub const TRANSLATION_VRAM_SPEAKER: ErrorInfo = ErrorInfo {
    code: "TRANSLATION_VRAM_SPEAKER",
    message: "VRAM out of memory during translation of speaker",
    category: "translation",
    severity: "critical",
};
pub const TRANSLATION_DISABLED_VRAM: ErrorInfo = ErrorInfo {
    code: "TRANSLATION_DISABLED_VRAM",
    message: "Translation disabled due to VRAM overflow",
    category: "translation",
    severity: "critical",
};

impl ErrorInfo {
    /// The reply: status 400 and the code, its text and the value the UI should keep showing.
    pub fn response(&self, data: Value) -> (u16, Value) {
        (
            400,
            json!({
                "error_code": self.code,
                "message": self.message,
                "data": data,
                "details": {},
                "category": self.category,
                "severity": self.severity,
            }),
        )
    }
}
