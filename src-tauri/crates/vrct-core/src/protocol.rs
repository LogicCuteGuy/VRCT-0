use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One backend response, serialised exactly like the sidecar's stdout lines.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub status: u16,
    pub endpoint: String,
    #[serde(default)]
    pub result: Value,
}

impl Response {
    pub fn new(status: u16, endpoint: impl Into<String>, result: Value) -> Self {
        Self {
            status,
            endpoint: endpoint.into(),
            result,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("payload is not valid base64: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("payload is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// Decode the UI's `base64(JSON.stringify(value))` payload.
pub fn decode_payload(encoded: &str) -> Result<Value, ProtocolError> {
    let bytes = STANDARD.decode(encoded)?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Build the stdin line the legacy sidecar expects. The payload stays base64
/// because the Python side decodes it itself.
pub fn sidecar_line(endpoint: &str, data: Option<&str>) -> String {
    let mut object = serde_json::Map::new();
    object.insert("endpoint".into(), Value::String(endpoint.into()));
    if let Some(data) = data {
        object.insert("data".into(), Value::String(data.into()));
    }
    let mut line = Value::Object(object).to_string();
    line.push('\n');
    line
}

/// Parse one sidecar stdout line. Blank lines (Windows CRLF split by the shell
/// plugin) and non-JSON noise yield `None`.
pub fn parse_sidecar_line(line: &str) -> Option<Response> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    serde_json::from_str(line).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_sidecar_lines_and_skips_noise() {
        let parsed = parse_sidecar_line("{\"status\":200,\"endpoint\":\"/a\",\"result\":[1]}\r\n").unwrap();
        assert_eq!(parsed, Response::new(200, "/a", json!([1])));
        assert_eq!(parse_sidecar_line("\n"), None);
        assert_eq!(parse_sidecar_line("Traceback (most recent call last):"), None);
        // `result` may be omitted by the sidecar.
        assert_eq!(
            parse_sidecar_line("{\"status\":200,\"endpoint\":\"/b\"}").unwrap().result,
            Value::Null
        );
    }

    #[test]
    fn decodes_utf8_json_payload() {
        let encoded = STANDARD.encode(r#"{"name":"日本語"}"#);
        assert_eq!(decode_payload(&encoded).unwrap(), json!({"name": "日本語"}));
    }

    #[test]
    fn rejects_garbage_payload() {
        assert!(decode_payload("!!!").is_err());
        assert!(decode_payload(&STANDARD.encode("not json")).is_err());
    }

    #[test]
    fn sidecar_line_omits_missing_data_and_ends_with_newline() {
        assert_eq!(
            sidecar_line("/run/feed_watchdog", None),
            "{\"endpoint\":\"/run/feed_watchdog\"}\n"
        );
        let line = sidecar_line("/set/data/x", Some("eyJhIjoxfQ=="));
        assert!(line.ends_with('\n'));
        let parsed: Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(parsed["data"], "eyJhIjoxfQ==");
    }

    #[test]
    fn response_serialises_in_sidecar_shape() {
        let json = serde_json::to_value(Response::new(200, "/a", json!(true))).unwrap();
        assert_eq!(json, json!({"status": 200, "endpoint": "/a", "result": true}));
    }
}
