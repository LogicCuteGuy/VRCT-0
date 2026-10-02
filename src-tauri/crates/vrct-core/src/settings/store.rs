//! `Settings`: the single owner of config.json, replacing the Python `Config` singleton.
//!
//! Values live in one map, checked on every change by the rules in `schema`. Persisted
//! settings are written back to config.json: after a short debounce (so a slider being dragged
//! is one write), or at once for the few settings marked `immediate`. The file is replaced
//! atomically, so a crash never leaves half a config.json.

use std::fmt;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use super::defaults::initial_state;
use super::env::Env;
use super::schema::{self, Prop, Rejected, PROPS};
use super::validators::State;

/// How long after the last change a non-immediate setting is written.
pub const DEBOUNCE: Duration = Duration::from_secs(2);

const INSTALLER_LANGUAGE_MARKER: &str = "installer_language.txt";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetError {
    UnknownProperty,
    ReadOnly,
    /// The value was refused (wrong type, not allowed, or a validator said no).
    Invalid,
}

impl fmt::Display for SetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SetError::UnknownProperty => f.write_str("unknown setting"),
            SetError::ReadOnly => f.write_str("read-only setting"),
            SetError::Invalid => f.write_str("invalid value"),
        }
    }
}

type Listener = Box<dyn Fn(&str, &Value) + Send + Sync>;

struct SaveSlot {
    due: Option<Instant>,
    stop: bool,
}

struct Inner {
    env: Env,
    state: RwLock<State>,
    debounce: Duration,
    slot: Mutex<SaveSlot>,
    wake: Condvar,
    /// Serialises writes of the file.
    file: Mutex<()>,
    listeners: RwLock<Vec<Listener>>,
}

pub struct Settings {
    inner: Arc<Inner>,
    saver: Option<JoinHandle<()>>,
}

impl Settings {
    /// Defaults, then config.json. A config.json that cannot be read is reported and left alone
    /// (Python logged and carried on with the defaults); the second value is that report.
    pub fn open(env: Env) -> (Settings, Option<String>) {
        Self::open_with(env, DEBOUNCE)
    }

    pub fn open_with(env: Env, debounce: Duration) -> (Settings, Option<String>) {
        let _ = std::fs::create_dir_all(&env.paths.logs);
        let inner = Arc::new(Inner {
            state: RwLock::new(initial_state(&env)),
            env,
            debounce,
            slot: Mutex::new(SaveSlot { due: None, stop: false }),
            wake: Condvar::new(),
            file: Mutex::new(()),
            listeners: RwLock::new(Vec::new()),
        });
        let worker = Arc::clone(&inner);
        let saver = std::thread::Builder::new().name("vrct-settings-save".into()).spawn(move || worker.save_loop()).ok();
        let settings = Settings { inner, saver };
        let report = settings.load().err();
        (settings, report)
    }

    /// A copy of the current value.
    pub fn get(&self, name: &str) -> Option<Value> {
        self.inner.state.read().unwrap().get(name).cloned()
    }

    pub fn get_str(&self, name: &str) -> Option<String> {
        match self.get(name) {
            Some(Value::String(text)) => Some(text),
            _ => None,
        }
    }

    pub fn get_bool(&self, name: &str) -> Option<bool> {
        self.get(name).and_then(|v| v.as_bool())
    }

    /// Changes a setting if the value passes its rules (what is stored may differ from what was
    /// given: a validator can normalise). A persisted one is scheduled for writing, and
    /// subscribers hear about it.
    pub fn set(&self, name: &str, value: Value) -> Result<(), SetError> {
        let prop = schema::find(name).ok_or(SetError::UnknownProperty)?;
        let stored = self.store(prop, &value)?;
        if prop.persisted {
            self.inner.schedule(prop.immediate);
            for listener in self.inner.listeners.read().unwrap().iter() {
                listener(prop.name, &stored);
            }
        }
        Ok(())
    }

    /// Takes a persisted value as it is, without checking it. Only for the time the sidecar still
    /// runs: its `Config` has already checked the value against its own device lists, and what
    /// it holds is what the program is really using, so the file must say the same. Removed with
    /// the sidecar. An unchanged value is left alone (no write, no notification).
    pub fn adopt(&self, name: &str, value: Value) -> Result<(), SetError> {
        let prop = schema::find(name).ok_or(SetError::UnknownProperty)?;
        if prop.is_read_only() || !prop.persisted {
            return Err(SetError::ReadOnly);
        }
        {
            let mut state = self.inner.state.write().unwrap();
            if state.get(prop.name) == Some(&value) {
                return Ok(());
            }
            state.insert(prop.name.to_string(), value.clone());
        }
        self.inner.schedule(prop.immediate);
        for listener in self.inner.listeners.read().unwrap().iter() {
            listener(prop.name, &value);
        }
        Ok(())
    }

    /// Called with `(name, new value)` after each change to a persisted setting, from the thread
    /// that made the change. Replaces the sidecar's `/internal/config/changed` bridge.
    pub fn subscribe(&self, listener: impl Fn(&str, &Value) + Send + Sync + 'static) {
        self.inner.listeners.write().unwrap().push(Box::new(listener));
    }

    /// The persisted settings in config.json order.
    pub fn snapshot(&self) -> Vec<(String, Value)> {
        self.inner.snapshot()
    }

    /// Writes config.json now.
    pub fn save_now(&self) -> Result<(), String> {
        self.inner.slot.lock().unwrap().due = None;
        self.inner.write_file()
    }

    /// Writes config.json now if a change is still waiting for its debounce.
    pub fn flush(&self) -> Result<(), String> {
        let pending = self.inner.slot.lock().unwrap().due.take().is_some();
        if pending {
            self.inner.write_file()
        } else {
            Ok(())
        }
    }

    /// `Config.revalidate_selected_models`: after the model lists were refreshed, a selected model
    /// that is no longer on its list becomes the first one on it.
    pub fn revalidate_selected_models(&self) {
        const PAIRS: [(&str, &str); 12] = [
            ("SELECTED_PLAMO_MODEL", "SELECTABLE_PLAMO_MODEL_LIST"),
            ("SELECTED_GEMINI_MODEL", "SELECTABLE_GEMINI_MODEL_LIST"),
            ("SELECTED_OPENAI_MODEL", "SELECTABLE_OPENAI_MODEL_LIST"),
            ("SELECTED_GROQ_MODEL", "SELECTABLE_GROQ_MODEL_LIST"),
            ("SELECTED_OPENROUTER_MODEL", "SELECTABLE_OPENROUTER_MODEL_LIST"),
            ("SELECTED_LMSTUDIO_MODEL", "SELECTABLE_LMSTUDIO_MODEL_LIST"),
            ("SELECTED_OPENAI_COMPATIBLE_MODEL", "SELECTABLE_OPENAI_COMPATIBLE_MODEL_LIST"),
            ("SELECTED_OLLAMA_MODEL", "SELECTABLE_OLLAMA_MODEL_LIST"),
            ("SELECTED_GROQ_WHISPER_MODEL", "SELECTABLE_GROQ_WHISPER_MODEL_LIST"),
            ("SELECTED_OPENAI_WHISPER_MODEL", "SELECTABLE_OPENAI_WHISPER_MODEL_LIST"),
            ("SELECTED_CUSTOM_WHISPER_MODEL", "SELECTABLE_CUSTOM_WHISPER_MODEL_LIST"),
            ("SELECTED_DEEPGRAM_MODEL", "SELECTABLE_DEEPGRAM_MODEL_LIST"),
        ];
        for (selected, list) in PAIRS {
            let (Some(current), Some(Value::Array(models))) = (self.get(selected), self.get(list)) else {
                continue;
            };
            if !models.is_empty() && !current.is_null() && !super::pyvalue::contains(&models, &current) {
                let _ = self.set(selected, models[0].clone());
            }
        }
    }

    /// Checks and stores, without scheduling a write or telling anyone.
    fn store(&self, prop: &Prop, value: &Value) -> Result<Value, SetError> {
        let mut state = self.inner.state.write().unwrap();
        let stored = prop.check(value, &state, &self.inner.env).map_err(|rejected| match rejected {
            Rejected::ReadOnly => SetError::ReadOnly,
            Rejected::Invalid => SetError::Invalid,
        })?;
        state.insert(prop.name.to_string(), stored.clone());
        Ok(stored)
    }

    /// `Config.load_config`. An error means the file could not be parsed and the rest of the
    /// start-up steps (release channel, installer language, the write-back) were skipped.
    fn load(&self) -> Result<(), String> {
        let env = &self.inner.env;
        if let Some(entries) = read_config_file(&env.paths.config)? {
            for (key, value) in entries {
                // Unknown keys are dropped, and so are read-only and run-time ones.
                if let Some(prop) = schema::find(&key).filter(|p| p.persisted && !p.is_read_only()) {
                    let _ = self.store(prop, &value);
                }
            }
        }

        // The channel is whatever the running version says, not what an interrupted update left behind.
        if let Some(prop) = schema::find("SELECTED_RELEASE_CHANNEL") {
            let _ = self.store(prop, &Value::String(channel_for_version(&env.version).to_string()));
        }

        // The installer leaves its UI language in a marker file; it is used once.
        let marker = env.paths.local.join(INSTALLER_LANGUAGE_MARKER);
        if marker.is_file() {
            if let Ok(text) = std::fs::read_to_string(&marker) {
                if let Some(prop) = schema::find("UI_LANGUAGE") {
                    let _ = self.store(prop, &Value::String(text.trim().to_string()));
                }
            }
            let _ = std::fs::remove_file(&marker);
        }

        self.save_now()
    }
}

impl Drop for Settings {
    fn drop(&mut self) {
        let _ = self.flush();
        self.inner.slot.lock().unwrap().stop = true;
        self.inner.wake.notify_all();
        if let Some(saver) = self.saver.take() {
            let _ = saver.join();
        }
    }
}

impl Inner {
    fn snapshot(&self) -> Vec<(String, Value)> {
        let state = self.state.read().unwrap();
        PROPS
            .iter()
            .filter(|prop| prop.persisted)
            .filter_map(|prop| state.get(prop.name).map(|value| (prop.name.to_string(), value.clone())))
            .collect()
    }

    /// Schedules the write, replacing any earlier schedule. Immediate means: wake the saver now.
    fn schedule(&self, immediate: bool) {
        let mut slot = self.slot.lock().unwrap();
        slot.due = Some(Instant::now() + if immediate { Duration::ZERO } else { self.debounce });
        self.wake.notify_all();
    }

    fn save_loop(&self) {
        let mut slot = self.slot.lock().unwrap();
        loop {
            if slot.stop {
                return;
            }
            match slot.due {
                None => slot = self.wake.wait(slot).unwrap(),
                Some(due) => {
                    let now = Instant::now();
                    if due <= now {
                        slot.due = None;
                        drop(slot);
                        if let Err(error) = self.write_file() {
                            eprintln!("[settings] cannot write config.json: {error}");
                        }
                        slot = self.slot.lock().unwrap();
                    } else {
                        slot = self.wake.wait_timeout(slot, due - now).unwrap().0;
                    }
                }
            }
        }
    }

    fn write_file(&self) -> Result<(), String> {
        let _writing = self.file.lock().unwrap();
        let text = config_text(&self.snapshot());
        let path = &self.env.paths.config;
        let mut tmp = path.clone().into_os_string();
        tmp.push(".tmp");
        let tmp = std::path::PathBuf::from(tmp);
        let write = || -> std::io::Result<()> {
            use std::io::Write;
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            std::fs::rename(&tmp, path)
        };
        write().map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// `"beta"` for a version with a `-beta` or `-rc` suffix, the rule the installer applies too.
pub fn channel_for_version(version: &str) -> &'static str {
    if ["-beta", "-rc"].iter().any(|marker| version.contains(marker)) {
        "beta"
    } else {
        "stable"
    }
}

/// config.json as Python wrote it: an object with 4-space indentation, keys in the given order,
/// non-ASCII text as is, no trailing newline.
pub fn config_text(entries: &[(String, Value)]) -> String {
    if entries.is_empty() {
        return "{}".to_string();
    }
    let mut out = String::from("{");
    for (index, (key, value)) in entries.iter().enumerate() {
        out.push_str(if index == 0 { "\n    " } else { ",\n    " });
        out.push_str(&serde_json::to_string(key).unwrap_or_default());
        out.push_str(": ");
        out.push_str(&pretty(value).replace('\n', "\n    "));
    }
    out.push_str("\n}");
    out
}

fn pretty(value: &Value) -> String {
    let mut buffer = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"    ");
    let mut serializer = serde_json::Serializer::with_formatter(&mut buffer, formatter);
    if value.serialize(&mut serializer).is_err() {
        return "null".to_string();
    }
    String::from_utf8(buffer).unwrap_or_else(|_| "null".to_string())
}

/// The top-level entries of a JSON object in file order (`serde_json::Map` would sort them, and
/// the order matters because later settings are checked against earlier ones).
struct Ordered(Vec<(String, Value)>);

impl<'de> Deserialize<'de> for Ordered {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Entries;
        impl<'de> Visitor<'de> for Entries {
            type Value = Ordered;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a JSON object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Ordered, A::Error> {
                let mut entries: Vec<(String, Value)> = Vec::new();
                while let Some((key, value)) = map.next_entry::<String, Value>()? {
                    // A repeated key keeps its first position and takes the last value, like a Python dict.
                    match entries.iter_mut().find(|(existing, _)| *existing == key) {
                        Some(entry) => entry.1 = value,
                        None => entries.push((key, value)),
                    }
                }
                Ok(Ordered(entries))
            }
        }
        deserializer.deserialize_map(Entries)
    }
}

/// None when there is no file or it is empty; an error when it is not a JSON object.
fn read_config_file(path: &Path) -> Result<Option<Vec<(String, Value)>>, String> {
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if text.is_empty() {
        return Ok(None);
    }
    serde_json::from_str::<Ordered>(&text).map(|ordered| Some(ordered.0)).map_err(|e| format!("{}: {e}", path.display()))
}
