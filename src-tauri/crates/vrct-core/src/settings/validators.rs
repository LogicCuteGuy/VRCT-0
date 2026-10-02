//! The `ValidatedProperty` validators of `config.py`, one function each.
//!
//! A validator gets the incoming value, the current state (some validators fall back to the
//! current value of the property itself, or look at another setting) and the environment, and
//! returns the value to store or `None` to reject. Python's validators could also raise (a
//! wrong-shaped value hitting `.items()`, say) which rejected the same way; `?` on an
//! `Option` is that here.

use std::collections::HashMap;

use serde_json::{Map, Value};

use super::env::Env;
use super::pyvalue::{contains, contains_str, float_value, is_float, is_int, py_eq, to_f64};

pub type State = HashMap<String, Value>;

pub type Validator = fn(&Value, &State, &Env) -> Option<Value>;

static NULL: Value = Value::Null;

/// `inst.NAME`: the current value, `null` when the state has none.
pub fn get<'a>(state: &'a State, name: &str) -> &'a Value {
    state.get(name).unwrap_or(&NULL)
}

fn same_keys(a: &Map<String, Value>, b: &Map<String, Value>) -> bool {
    a.len() == b.len() && a.keys().all(|key| b.contains_key(key))
}

pub fn main_window_geometry(val: &Value, st: &State, _: &Env) -> Option<Value> {
    let val = val.as_object()?;
    let current = get(st, "MAIN_WINDOW_GEOMETRY").as_object()?;
    if !same_keys(val, current) {
        return None;
    }
    let mut new = Map::new();
    for (key, value) in val {
        new.insert(key.clone(), if is_int(value) { value.clone() } else { current[key].clone() });
    }
    Some(Value::Object(new))
}

fn compute_type(device_setting: &'static str) -> impl Fn(&Value, &State) -> Option<Value> {
    move |val, st| {
        val.as_str()?;
        let types = get(st, device_setting).as_object()?.get("compute_types").and_then(Value::as_array);
        match types {
            Some(types) if contains(types, val) => Some(val.clone()),
            _ => None,
        }
    }
}

pub fn selected_transcription_compute_type(val: &Value, st: &State, _: &Env) -> Option<Value> {
    compute_type("SELECTED_TRANSCRIPTION_COMPUTE_DEVICE")(val, st)
}

pub fn selected_translation_compute_type(val: &Value, st: &State, _: &Env) -> Option<Value> {
    compute_type("SELECTED_TRANSLATION_COMPUTE_DEVICE")(val, st)
}

const TRACKERS: [&str; 3] = ["HMD", "LeftHand", "RightHand"];
const OVERLAY_FLOATS: [&str; 6] = ["x_pos", "y_pos", "z_pos", "x_rotation", "y_rotation", "z_rotation"];

fn overlay(name: &'static str, val: &Value, st: &State) -> Option<Value> {
    let val = val.as_object()?;
    let base = get(st, name).as_object()?;
    if !same_keys(val, base) {
        return None;
    }
    let mut new = base.clone();
    for (key, v) in val {
        let key = key.as_str();
        if key == "tracker" {
            if let Some(tracker) = v.as_str().filter(|t| TRACKERS.contains(t)) {
                new.insert(key.to_string(), Value::String(tracker.to_string()));
            }
        } else if OVERLAY_FLOATS.contains(&key) || key == "opacity" || key == "ui_scaling" {
            if let Some(float) = to_f64(v).filter(|_| is_int(v) || is_float(v)).and_then(float_value) {
                new.insert(key.to_string(), float);
            }
        } else if (key == "display_duration" || key == "fadeout_duration") && is_int(v) {
            new.insert(key.to_string(), v.clone());
        }
    }
    Some(Value::Object(new))
}

pub fn overlay_small(val: &Value, st: &State, _: &Env) -> Option<Value> {
    overlay("OVERLAY_SMALL_LOG_SETTINGS", val, st)
}

pub fn overlay_large(val: &Value, st: &State, _: &Env) -> Option<Value> {
    overlay("OVERLAY_LARGE_LOG_SETTINGS", val, st)
}

/// `validateDictStructure`'s expected shapes.
enum Shape {
    Text,
    Flag,
    Dict(&'static [(&'static str, Shape)]),
}

const MESSAGE_FORMAT: Shape = Shape::Dict(&[
    ("message", Shape::Dict(&[("prefix", Shape::Text), ("suffix", Shape::Text)])),
    ("separator", Shape::Text),
    ("translation", Shape::Dict(&[("prefix", Shape::Text), ("separator", Shape::Text), ("suffix", Shape::Text)])),
    ("translation_first", Shape::Flag),
]);

fn matches_shape(data: &Value, shape: &Shape) -> bool {
    match shape {
        Shape::Text => data.is_string(),
        Shape::Flag => data.is_boolean(),
        Shape::Dict(fields) => {
            let Some(object) = data.as_object() else { return false };
            object.len() == fields.len()
                && fields.iter().all(|(key, shape)| object.get(*key).is_some_and(|value| matches_shape(value, shape)))
        }
    }
}

/// Both message-format settings: the value must have exactly this shape, and is stored as given.
pub fn message_format(val: &Value, _: &State, _: &Env) -> Option<Value> {
    (val.is_object() && matches_shape(val, &MESSAGE_FORMAT)).then(|| val.clone())
}

/// Strings only, first occurrence of each.
pub fn mic_word_filter(val: &Value, _: &State, _: &Env) -> Option<Value> {
    let list = val.as_array()?;
    let mut seen: Vec<&str> = Vec::new();
    for item in list {
        if let Some(text) = item.as_str() {
            if !seen.contains(&text) {
                seen.push(text);
            }
        }
    }
    Some(Value::Array(seen.into_iter().map(|s| Value::String(s.to_string())).collect()))
}

/// An engine that is not selectable falls back to what that tab had.
pub fn selected_translation_engines(val: &Value, st: &State, env: &Env) -> Option<Value> {
    let val = val.as_object()?;
    let old = get(st, "SELECTED_TRANSLATION_ENGINES").as_object();
    let mut new = Map::new();
    for (tab, engine) in val {
        let chosen = if contains_str(&env.translation_engines, engine) {
            engine.clone()
        } else {
            old.and_then(|old| old.get(tab)).cloned().unwrap_or(Value::Null)
        };
        new.insert(tab.clone(), chosen);
    }
    Some(Value::Object(new))
}

/// `{tab: {slot: {language, country, enable}}}`; an entry that is not a known language and
/// country, or whose `enable` is not a bool, falls back to what that slot had.
fn languages(name: &'static str, val: &Value, st: &State, env: &Env) -> Option<Value> {
    let val = val.as_object()?;
    let old = get(st, name).as_object();
    let mut new = Map::new();
    for (tab, slots) in val {
        let slots = slots.as_object()?;
        let mut kept = Map::new();
        for (slot, entry) in slots {
            let entry = entry.as_object()?;
            let language = entry.get("language").and_then(Value::as_str);
            let country = entry.get("country").and_then(Value::as_str);
            let known = language
                .and_then(|language| env.transcription_languages.get(language))
                .is_some_and(|countries| country.is_some_and(|country| countries.iter().any(|c| c == country)));
            let enable = entry.get("enable").filter(|e| e.is_boolean());
            kept.insert(
                slot.clone(),
                match (known, enable) {
                    (true, Some(enable)) => {
                        let mut clean = Map::new();
                        clean.insert("language".into(), Value::String(language?.to_string()));
                        clean.insert("country".into(), Value::String(country?.to_string()));
                        clean.insert("enable".into(), enable.clone());
                        Value::Object(clean)
                    }
                    _ => old
                        .and_then(|old| old.get(tab))
                        .and_then(Value::as_object)
                        .and_then(|old| old.get(slot))
                        .cloned()
                        .unwrap_or(Value::Null),
                },
            );
        }
        new.insert(tab.clone(), Value::Object(kept));
    }
    Some(Value::Object(new))
}

pub fn selected_your_languages(val: &Value, st: &State, env: &Env) -> Option<Value> {
    languages("SELECTED_YOUR_LANGUAGES", val, st, env)
}

pub fn selected_target_languages(val: &Value, st: &State, env: &Env) -> Option<Value> {
    languages("SELECTED_TARGET_LANGUAGES", val, st, env)
}

pub fn selected_mic_host(val: &Value, _: &State, env: &Env) -> Option<Value> {
    let host = val.as_str()?;
    (host == "NoHost" || env.devices.mic_hosts().iter().any(|h| h == host)).then(|| val.clone())
}

pub fn selected_mic_device(val: &Value, st: &State, env: &Env) -> Option<Value> {
    let name = val.as_str()?;
    if name == "NoDevice" {
        return Some(val.clone());
    }
    let host = get(st, "SELECTED_MIC_HOST").as_str()?;
    env.devices.mic_device_names(host).iter().any(|n| n == name).then(|| val.clone())
}

pub fn selected_speaker_device(val: &Value, _: &State, env: &Env) -> Option<Value> {
    let name = val.as_str()?;
    (name == "NoDevice" || env.devices.speaker_device_names().iter().any(|n| n == name)).then(|| val.clone())
}

/// One of the machine's compute devices, as it is listed.
pub fn compute_device(val: &Value, st: &State, _: &Env) -> Option<Value> {
    if !val.is_object() {
        return None;
    }
    let devices = get(st, "SELECTABLE_COMPUTE_DEVICE_LIST").as_array()?;
    devices.iter().any(|device| py_eq(device, val)).then(|| val.clone())
}

/// Hotkeys: exactly the known actions; a binding that is neither a list nor `null` keeps the old one.
pub fn hotkeys(val: &Value, st: &State, _: &Env) -> Option<Value> {
    let val = val.as_object()?;
    let current = get(st, "HOTKEYS").as_object()?;
    if !same_keys(val, current) {
        return None;
    }
    let mut new = Map::new();
    for (action, binding) in val {
        let kept = if binding.is_array() || binding.is_null() { binding.clone() } else { current[action].clone() };
        new.insert(action.clone(), kept);
    }
    Some(Value::Object(new))
}

fn keys(name: &'static str, val: &Value, st: &State) -> Option<Value> {
    let val = val.as_object()?;
    let current = get(st, name).as_object()?;
    let mut new = Map::new();
    for (key, old) in current {
        let incoming = val.get(key).filter(|v| v.is_string() || v.is_null());
        new.insert(key.clone(), incoming.unwrap_or(old).clone());
    }
    Some(Value::Object(new))
}

/// API keys: the known services only; anything that is not a string or `null` keeps the old key.
pub fn auth_keys(val: &Value, st: &State, _: &Env) -> Option<Value> {
    keys("AUTH_KEYS", val, st)
}

pub fn transcription_auth_keys(val: &Value, st: &State, _: &Env) -> Option<Value> {
    keys("TRANSCRIPTION_AUTH_KEYS", val, st)
}
