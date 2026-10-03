//! `Controller.messageFormatter`: a message and its translations as the text that goes to the chatbox.

use serde_json::Value;

/// Which of the two format settings applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatType {
    /// `SEND_MESSAGE_FORMAT_PARTS`: what I said.
    Send,
    /// `RECEIVED_MESSAGE_FORMAT_PARTS`: what I heard.
    Received,
}

impl FormatType {
    pub fn setting(self) -> &'static str {
        match self {
            FormatType::Send => "SEND_MESSAGE_FORMAT_PARTS",
            FormatType::Received => "RECEIVED_MESSAGE_FORMAT_PARTS",
        }
    }
}

fn piece<'a>(parts: &'a Value, path: &[&str]) -> &'a str {
    let mut value = parts;
    for key in path {
        value = value.get(key).unwrap_or(&Value::Null);
    }
    value.as_str().unwrap_or_default()
}

/// Joins message and translations the way `parts` (the setting's value) says. With translations but no
/// message only the translations come out; without translations only the message.
pub fn message_formatter(parts: &Value, translation: &[String], message: &str) -> String {
    let message_part = format!("{}{}{}", piece(parts, &["message", "prefix"]), message, piece(parts, &["message", "suffix"]));
    let translation_part = format!(
        "{}{}{}",
        piece(parts, &["translation", "prefix"]),
        translation.join(piece(parts, &["translation", "separator"])),
        piece(parts, &["translation", "suffix"])
    );
    let separator = piece(parts, &["separator"]);
    // The setting is validated to be a boolean.
    let translation_first = parts.get("translation_first") == Some(&Value::Bool(true));
    if !translation.is_empty() && !message.is_empty() {
        if translation_first {
            format!("{translation_part}{separator}{message_part}")
        } else {
            format!("{message_part}{separator}{translation_part}")
        }
    } else if !translation.is_empty() {
        translation_part
    } else {
        message_part
    }
}
