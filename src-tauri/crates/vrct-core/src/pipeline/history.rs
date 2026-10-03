//! The messages the LLM translators get as context: `Model.addTranslationHistory` and its list.

use std::sync::{Arc, Mutex, MutexGuard};

use chrono::{Local, NaiveDateTime, Timelike};
use serde_json::{json, Value};

use crate::translation::prompt::py_strip;

/// `translation_history_max_items`.
pub const MAX_ITEMS: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub source: String,
    pub text: String,
    pub timestamp: String,
}

#[derive(Debug, Default)]
pub struct History {
    items: Vec<Item>,
}

impl History {
    /// Adds a message ("chat", "mic", "speaker", "ocr") unless it is blank; only the newest [`MAX_ITEMS`] stay.
    pub fn add(&mut self, source: &str, text: &str, timestamp: impl FnOnce() -> String) {
        let text = py_strip(text);
        if text.is_empty() {
            return;
        }
        self.items.push(Item { source: source.to_string(), text: text.to_string(), timestamp: timestamp() });
        if self.items.len() > MAX_ITEMS {
            self.items.drain(..self.items.len() - MAX_ITEMS);
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// The list as the translators take it: `{"source", "text", "timestamp"}` per message.
    pub fn to_values(&self) -> Vec<Value> {
        self.items
            .iter()
            .map(|item| json!({"source": item.source, "text": item.text, "timestamp": item.timestamp}))
            .collect()
    }
}

/// The history the pipeline writes and the translator reads, from different threads.
#[derive(Debug, Clone, Default)]
pub struct SharedHistory(Arc<Mutex<History>>);

impl SharedHistory {
    fn lock(&self) -> MutexGuard<'_, History> {
        self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Adds a message stamped with the current local time.
    pub fn add(&self, source: &str, text: &str) {
        self.lock().add(source, text, isoformat_now);
    }

    pub fn clear(&self) {
        self.lock().clear();
    }

    /// `getTranslationHistory`: a copy of the list as the translators take it.
    pub fn snapshot(&self) -> Vec<Value> {
        self.lock().to_values()
    }
}

/// `datetime.isoformat()`: microseconds only when there are some.
pub fn isoformat(moment: &NaiveDateTime) -> String {
    let micros = moment.nanosecond() / 1000 % 1_000_000;
    let base = moment.format("%Y-%m-%dT%H:%M:%S");
    if micros == 0 {
        base.to_string()
    } else {
        format!("{base}.{micros:06}")
    }
}

/// `datetime.now().isoformat()`: local time.
pub fn isoformat_now() -> String {
    isoformat(&Local::now().naive_local())
}
