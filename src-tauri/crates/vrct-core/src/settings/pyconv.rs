//! Python's conversions: what `int(x)`, `float(x)`, `str(x)` and `dict(x)` do to a JSON value.
//!
//! The `/set/data/*` handlers were written as `int(data)` and the like, so what they accept is
//! whatever Python accepted: `"1_000"`, `" 7 "`, `2.9` and `True` are all fine integers.
//! Not reproduced: digits of other scripts (`int("٣")`), and numbers that do not fit a JSON value
//! here (beyond 64 bits, NaN, infinity); those count as not convertible.

use serde_json::{Map, Value};

/// `int(value)` for a JSON value, or `None` where Python raised (or the result cannot be kept).
pub fn py_int(value: &Value) -> Option<i128> {
    match value {
        Value::Bool(b) => Some(i128::from(*b)),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                return Some(i128::from(i));
            }
            if let Some(u) = n.as_u64() {
                return Some(i128::from(u));
            }
            let truncated = n.as_f64().filter(|f| f.is_finite())?.trunc();
            // Anything outside what i64 / u64 can hold is not kept (see above).
            (truncated >= i64::MIN as f64 && truncated < u64::MAX as f64).then_some(truncated as i128)
        }
        Value::String(text) => parse_int_text(text),
        _ => None,
    }
}

/// An integer as a JSON value, if i64 or u64 can hold it.
pub fn int_value(value: i128) -> Option<Value> {
    i64::try_from(value).map(Value::from).ok().or_else(|| u64::try_from(value).map(Value::from).ok())
}

/// The text rules of `int()`: surrounding whitespace, a sign, digits with single underscores between.
fn parse_int_text(text: &str) -> Option<i128> {
    let text = text.trim();
    let (negative, digits) = match text.as_bytes().first()? {
        b'-' => (true, &text[1..]),
        b'+' => (false, &text[1..]),
        _ => (false, text),
    };
    let digits = without_underscores(digits)?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let magnitude: i128 = digits.parse().ok()?;
    Some(if negative { -magnitude } else { magnitude })
}

/// Python allows one `_` between two digits (`1_000`); anywhere else it is an error.
fn without_underscores(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(chars.len());
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' {
            let between_digits =
                i > 0 && chars[i - 1].is_ascii_digit() && chars.get(i + 1).is_some_and(char::is_ascii_digit);
            if !between_digits {
                return None;
            }
        } else {
            out.push(c);
        }
    }
    Some(out)
}

/// `float(value)` for a JSON value, or `None` where Python raised (or the result is not finite).
pub fn py_float(value: &Value) -> Option<f64> {
    let number = match value {
        Value::Bool(b) => f64::from(u8::from(*b)),
        Value::Number(n) => n.as_f64()?,
        Value::String(text) => {
            let text = without_underscores(text.trim())?;
            if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit() || b"+-.eE".contains(&b)) {
                return None;
            }
            text.parse::<f64>().ok()?
        }
        _ => return None,
    };
    number.is_finite().then_some(number)
}

/// `repr(float)`: the shortest text that reads back as the same number, laid out as Python does.
pub fn py_float_repr(value: f64) -> String {
    if value == 0.0 {
        return if value.is_sign_negative() { "-0.0" } else { "0.0" }.to_string();
    }
    let sign = if value < 0.0 { "-" } else { "" };
    // "d.ddde<exp>": the shortest digits, with the exponent of the first digit.
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    if (-4..16).contains(&exponent) {
        let body = if exponent >= 0 {
            let integer_len = exponent as usize + 1;
            if digits.len() <= integer_len {
                format!("{digits}{}.0", "0".repeat(integer_len - digits.len()))
            } else {
                format!("{}.{}", &digits[..integer_len], &digits[integer_len..])
            }
        } else {
            format!("0.{}{digits}", "0".repeat((-exponent - 1) as usize))
        };
        format!("{sign}{body}")
    } else {
        let fraction = if digits.len() > 1 { format!(".{}", &digits[1..]) } else { String::new() };
        let exponent_sign = if exponent < 0 { '-' } else { '+' };
        format!("{sign}{}{fraction}e{exponent_sign}{:02}", &digits[..1], exponent.abs())
    }
}

/// `repr(str)`: single quotes unless the text has a `'` and no `"`.
fn py_str_repr(text: &str) -> String {
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

/// `repr(value)` of the Python object a JSON value loads as.
pub fn py_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(n) if n.is_i64() || n.is_u64() => n.to_string(),
        Value::Number(n) => py_float_repr(n.as_f64().unwrap_or(0.0)),
        Value::String(text) => py_str_repr(text),
        Value::Array(items) => format!("[{}]", items.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter().map(|(k, v)| format!("{}: {}", py_str_repr(k), py_repr(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// `str(value)`: a string as it is, anything else as its `repr`.
pub fn py_str(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => py_repr(other),
    }
}

/// `dict(value)`: an object is copied; a list is read as pairs (a pair is a 2-item list or a
/// 2-character string); `""` is empty. `None` where Python raised. A key that is a number, `True`
/// or `None` is written the way `json.dumps` writes it (as text), which is also how the UI would see it.
pub fn py_dict(value: &Value) -> Option<Map<String, Value>> {
    let mut map = Map::new();
    match value {
        Value::Object(object) => map = object.clone(),
        // A string is read character by character, and a character is never a pair: only "" works.
        Value::String(text) if text.is_empty() => {}
        Value::Array(items) => {
            for item in items {
                let (key, entry) = match item {
                    Value::Array(pair) if pair.len() == 2 => (pair[0].clone(), pair[1].clone()),
                    Value::String(text) if text.chars().count() == 2 => {
                        let mut chars = text.chars();
                        (Value::String(chars.next()?.to_string()), Value::String(chars.next()?.to_string()))
                    }
                    _ => return None,
                };
                let key = match key {
                    Value::String(text) => text,
                    Value::Array(_) | Value::Object(_) => return None, // unhashable
                    Value::Bool(b) => b.to_string(),
                    Value::Null => "null".to_string(),
                    number => py_repr(&number),
                };
                map.insert(key, entry);
            }
        }
        _ => return None,
    }
    Some(map)
}
