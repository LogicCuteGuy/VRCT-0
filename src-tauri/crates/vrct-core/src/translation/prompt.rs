//! The chat messages an LLM translation sends, built exactly like the Python
//! clients (`translation_openai.py` and its siblings) so a switch of backend
//! does not change what the model is asked.
//!
//! `assets/prompts.json` is generated from Python's own YAML files by
//! `tests/fixtures/regenerate_translation_golden.py`, and
//! `fixtures/translation_golden.json` holds what the real clients built.

use std::collections::HashMap;
use std::sync::OnceLock;

use chrono::{NaiveDate, NaiveTime};
use serde::Deserialize;
use serde_json::Value;

const ASSETS: &str = include_str!("assets/prompts.json");

#[derive(Debug, Deserialize)]
pub struct History {
    pub use_history: bool,
    pub sources: Vec<String>,
    pub max_messages: i64,
    pub max_chars: i64,
    pub header_template: String,
    pub item_template: String,
}

#[derive(Debug, Deserialize)]
pub struct Prompt {
    pub system_prompt: String,
    pub history: History,
    pub supported_languages: Vec<String>,
}

pub fn prompts() -> &'static HashMap<String, Prompt> {
    static PROMPTS: OnceLock<HashMap<String, Prompt>> = OnceLock::new();
    PROMPTS.get_or_init(|| serde_json::from_str(ASSETS).expect("prompts.json is valid"))
}

/// What Python's `str.strip()` removes: Unicode white space plus the four
/// separator controls `str.isspace` also accepts.
fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

pub fn py_strip(text: &str) -> &str {
    text.trim_matches(is_py_space)
}

/// `repr(str)`: single quotes unless the text has a `'` and no `"`.
fn py_repr_str(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `str(list_of_str)`, which is how Python interpolated `supported_languages`.
pub fn py_list_repr(items: &[String]) -> String {
    let inner: Vec<String> = items.iter().map(|item| py_repr_str(item)).collect();
    format!("[{}]", inner.join(", "))
}

/// `template.format(**values)` for plain `{name}` fields. `{{` and `}}` are
/// literal braces; an unknown field or a stray brace is an error, as in Python.
fn py_format(template: &str, values: &[(&str, &str)]) -> Result<String, String> {
    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                out.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                out.push('}');
            }
            '{' => {
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) => name.push(c),
                        None => return Err("unclosed field in template".into()),
                    }
                }
                match values.iter().find(|(key, _)| *key == name) {
                    Some((_, value)) => out.push_str(value),
                    None => return Err(format!("unknown field {name:?} in template")),
                }
            }
            '}' => return Err("single '}' in template".into()),
            c => out.push(c),
        }
    }
    Ok(out)
}

/// `datetime.fromisoformat(value).strftime("%H:%M")`, or "" when Python's
/// parser would raise. Covers the extended and basic ISO 8601 forms of 3.12.
pub fn hour_minute(value: &Value) -> String {
    value.as_str().and_then(parse_hour_minute).unwrap_or_default()
}

fn digits(text: &str, count: usize) -> Option<(u32, &str)> {
    let head = text.get(..count)?;
    if !head.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((head.parse().ok()?, &text[count..]))
}

fn parse_hour_minute(text: &str) -> Option<String> {
    // Date: YYYY-MM-DD or YYYYMMDD.
    let (year, rest) = digits(text, 4)?;
    let (extended, rest) = match rest.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, rest),
    };
    let (month, rest) = digits(rest, 2)?;
    let rest = if extended { rest.strip_prefix('-')? } else { rest };
    let (day, rest) = digits(rest, 2)?;
    NaiveDate::from_ymd_opt(year as i32, month, day)?;
    if year == 0 {
        return None;
    }
    if rest.is_empty() {
        return Some("00:00".into());
    }
    // Any single character may separate the date from the time.
    let mut rest = rest.chars();
    rest.next()?;
    let time = rest.as_str();

    let (hour, time) = digits(time, 2)?;
    let (minute, time) = match time.strip_prefix(':') {
        Some(after) => {
            let (minute, after) = digits(after, 2)?;
            (minute, after)
        }
        None if time.is_empty() || !time.as_bytes()[0].is_ascii_digit() => (0, time),
        None => digits(time, 2)?,
    };
    let time = time.strip_prefix(':').unwrap_or(time);
    let (second, time) = if time.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        digits(time, 2)?
    } else {
        (0, time)
    };
    let time = match time.strip_prefix(['.', ',']) {
        Some(fraction) => {
            let end = fraction.bytes().take_while(u8::is_ascii_digit).count();
            if end == 0 {
                return None;
            }
            &fraction[end..]
        }
        None => time,
    };
    NaiveTime::from_hms_opt(hour, minute, second)?;
    if !valid_zone(time) {
        return None;
    }
    Some(format!("{hour:02}:{minute:02}"))
}

fn valid_zone(zone: &str) -> bool {
    if zone.is_empty() || zone == "Z" {
        return true;
    }
    let Some(offset) = zone.strip_prefix(['+', '-']) else {
        return false;
    };
    let Some((hours, rest)) = digits(offset, 2) else {
        return false;
    };
    if hours > 23 {
        return false;
    }
    let rest = rest.strip_prefix(':').unwrap_or(rest);
    if rest.is_empty() {
        return true;
    }
    let Some((minutes, rest)) = digits(rest, 2) else {
        return false;
    };
    if minutes > 59 {
        return false;
    }
    let rest = rest.strip_prefix(':').unwrap_or(rest);
    if rest.is_empty() {
        return true;
    }
    let Some((seconds, rest)) = digits(rest, 2) else {
        return false;
    };
    seconds <= 59
        && match rest.strip_prefix('.') {
            Some(fraction) => !fraction.is_empty() && fraction.bytes().all(|b| b.is_ascii_digit()),
            None => rest.is_empty(),
        }
}

/// Python's `str(value)` for a history field that was not a string.
fn display(value: Option<&Value>) -> String {
    match value {
        None => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Null) => "None".into(),
        Some(Value::Bool(true)) => "True".into(),
        Some(Value::Bool(false)) => "False".into(),
        Some(other) => other.to_string(),
    }
}

fn char_tail(text: &str, keep: usize) -> &str {
    let count = text.chars().count();
    if count <= keep {
        return text;
    }
    let skip = count - keep;
    let start = text.char_indices().nth(skip).map_or(text.len(), |(index, _)| index);
    &text[start..]
}

/// The system prompt for one translation, with the conversation context the
/// engine's history settings allow.
pub fn system_prompt(
    engine: &str,
    input_lang: &str,
    output_lang: &str,
    history: &[Value],
) -> Result<String, String> {
    let prompt = prompts().get(engine).ok_or_else(|| format!("no prompt for engine {engine:?}"))?;
    let supported = py_list_repr(&prompt.supported_languages);
    let mut system = py_format(
        &prompt.system_prompt,
        &[
            ("supported_languages", &supported),
            ("input_lang", input_lang),
            ("output_lang", output_lang),
        ],
    )?;

    let settings = &prompt.history;
    if !settings.use_history {
        return Ok(system);
    }
    let allowed = |item: &&Value| {
        item.get("source")
            .and_then(Value::as_str)
            .is_some_and(|source| settings.sources.iter().any(|allowed| allowed == source))
    };
    let filtered: Vec<&Value> = history.iter().filter(allowed).collect();
    let recent = if settings.max_messages > 0 {
        &filtered[filtered.len().saturating_sub(settings.max_messages as usize)..]
    } else {
        &filtered[..]
    };
    let mut items = Vec::with_capacity(recent.len());
    for item in recent {
        let stamp = item.get("timestamp").map(hour_minute).unwrap_or_default();
        let source = display(item.get("source"));
        let text = display(item.get("text"));
        items.push(py_format(
            &settings.item_template,
            &[("timestamp", &stamp), ("source", &source), ("text", &text)],
        )?);
    }
    let joined = items.join("\n");
    let mut blob = py_strip(&joined);
    if settings.max_chars > 0 {
        blob = char_tail(blob, settings.max_chars as usize);
    }
    let header = py_format(
        &settings.header_template,
        &[("max_messages", &settings.max_messages.to_string()), ("history", blob)],
    )?;
    if !header.is_empty() {
        system = format!("{system}\n\n{header}");
    }
    Ok(system)
}

/// What the Python clients did with a model reply: take a string as is, join
/// the string and `{"content": str}` parts of a list, then strip.
pub fn reply_text(content: &Value) -> String {
    let joined = match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(|part| match part {
                Value::String(text) => text.as_str(),
                Value::Object(map) => map.get("content").and_then(Value::as_str).unwrap_or(""),
                _ => "",
            })
            .collect(),
        _ => String::new(),
    };
    py_strip(&joined).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn repr_matches_python_for_awkward_names() {
        let list = |items: &[&str]| py_list_repr(&items.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(list(&["a", "b"]), "['a', 'b']");
        assert_eq!(list(&["it's"]), "[\"it's\"]");
        assert_eq!(list(&["it's \"x\""]), "['it\\'s \"x\"']");
        assert_eq!(list(&["back\\slash", "tab\t", "日本語"]), "['back\\\\slash', 'tab\\t', '日本語']");
        assert_eq!(list(&[]), "[]");
    }

    #[test]
    fn format_handles_escapes_and_rejects_unknown_fields() {
        assert_eq!(py_format("{{a}} {x}", &[("x", "1")]).unwrap(), "{a} 1");
        assert!(py_format("{nope}", &[("x", "1")]).is_err());
        assert!(py_format("}", &[]).is_err());
        assert!(py_format("{x", &[("x", "1")]).is_err());
        // Substituted values are not formatted again.
        assert_eq!(py_format("{x}", &[("x", "{y}")]).unwrap(), "{y}");
    }

    #[test]
    fn strip_follows_python_whitespace() {
        assert_eq!(py_strip("\u{1c} a \u{85}\u{2003}"), "a");
        assert_eq!(py_strip("\u{200b}a"), "\u{200b}a");
    }

    #[test]
    fn char_tail_counts_characters_not_bytes() {
        assert_eq!(char_tail("日本語テキスト", 3), "キスト");
        assert_eq!(char_tail("abc", 10), "abc");
        assert_eq!(char_tail("abc", 0), "");
    }

    #[test]
    fn unknown_engine_is_an_error() {
        assert!(system_prompt("Nope", "a", "b", &[]).is_err());
        assert!(system_prompt("OpenAI_API", "Japanese", "English", &[json!({})]).is_ok());
    }

    #[test]
    fn replies_are_joined_and_stripped_like_python() {
        assert_eq!(reply_text(&json!("  hi \n")), "hi");
        assert_eq!(reply_text(&json!(["a ", {"content": "b"}, {"x": 1}, 3, " c"])), "a b c");
        assert_eq!(reply_text(&json!(null)), "");
    }
}
