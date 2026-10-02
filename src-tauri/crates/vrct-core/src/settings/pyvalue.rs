//! JSON values with the semantics Python gave them.
//!
//! The settings rules were written in Python, where `True` is an `int`, `1 == 1.0` and
//! `1 == True`. config.json round-trips through those rules, so they are reproduced here
//! rather than approximated with `serde_json`'s stricter equality.

use serde_json::{Number, Value};

/// `isinstance(v, int)`: Python counts `bool` as an int.
pub fn is_int(value: &Value) -> bool {
    match value {
        Value::Bool(_) => true,
        Value::Number(n) => n.is_i64() || n.is_u64(),
        _ => false,
    }
}

/// `isinstance(v, float)`.
pub fn is_float(value: &Value) -> bool {
    matches!(value, Value::Number(n) if !n.is_i64() && !n.is_u64())
}

/// `isinstance(v, (int, float))`.
pub fn is_number(value: &Value) -> bool {
    is_int(value) || is_float(value)
}

/// `float(v)` for something `is_number`.
pub fn to_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

/// A float as JSON (`1.0` stays `1.0`); NaN and infinities cannot be JSON.
pub fn float_value(value: f64) -> Option<Value> {
    Number::from_f64(value).map(Value::Number)
}

fn as_i128(value: &Value) -> Option<i128> {
    match value {
        Value::Bool(b) => Some(i128::from(*b)),
        Value::Number(n) => n.as_i64().map(i128::from).or_else(|| n.as_u64().map(i128::from)),
        _ => None,
    }
}

/// Python `==`: numbers (and bools) compare by value, containers deeply.
pub fn py_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| py_eq(p, q)),
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| py_eq(v, w)))
        }
        _ if is_number(a) && is_number(b) => match (as_i128(a), as_i128(b)) {
            (Some(x), Some(y)) => x == y,
            _ => to_f64(a) == to_f64(b),
        },
        _ => false,
    }
}

/// `value in list`.
pub fn contains(list: &[Value], value: &Value) -> bool {
    list.iter().any(|item| py_eq(item, value))
}

/// `value in list_of_strings`.
pub fn contains_str(list: &[String], value: &Value) -> bool {
    matches!(value, Value::String(s) if list.iter().any(|item| item == s))
}
