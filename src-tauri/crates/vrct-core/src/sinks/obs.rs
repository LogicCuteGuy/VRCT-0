//! The local web page an OBS "Browser Source" shows (`OBS_BROWSER_SOURCE`):
//! subtitles drawn from the WebSocket broadcast.
//!
//! Python still decides *when* it runs (port probes, enable/disable, host)
//! and sends `start`/`stop`; Rust owns the socket and builds the page from the
//! config replica on every request, as Python rebuilt it from `config`. The
//! page is the Python output with its settings left as placeholders
//! (`obs/page.html`, produced by `tests/fixtures/regenerate_obs_golden.py`),
//! and `tests/fixtures/obs_golden.json` pins that the rendering is identical.
//!
//! The page embeds the WebSocket token, so this server refuses wildcard
//! addresses like the WebSocket sink does.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::watch;

use super::net::{bind, is_wildcard};
use crate::config::ConfigReplica;

const PAGE_TEMPLATE: &str = include_str!("obs/page.html");
/// A request head larger than this, or slower than the timeout, is dropped.
const MAX_HEAD_BYTES: usize = 16 * 1024;
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
const NO_CACHE: &str = "no-store, max-age=0";

/// `int(value)` clamped to `min..=max`; anything that is not a number (or a
/// numeric string) gives `min`, as Python's `except Exception` did.
fn clamp_int(value: &Value, min: i64, max: i64) -> i64 {
    let number = match value {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().filter(|f| f.is_finite()).map(|f| f as i64)),
        Value::String(s) => s.trim().parse::<i64>().ok(),
        Value::Bool(b) => Some(i64::from(*b)),
        _ => None,
    };
    number.map_or(min, |n| n.clamp(min, max))
}

/// A `#rrggbb` colour (surrounding whitespace dropped), else `fallback`.
fn normalize_hex_color(value: &Value, fallback: &str) -> String {
    let Some(text) = value.as_str() else {
        return fallback.to_string();
    };
    let text = text.trim();
    let valid = text.len() == 7
        && text.starts_with('#')
        && text[1..].bytes().all(|b| b.is_ascii_hexdigit());
    if valid { text.to_string() } else { fallback.to_string() }
}

/// The token goes inside a JS string literal. Real tokens
/// (`secrets.token_urlsafe`) are `[A-Za-z0-9_-]` and pass through untouched;
/// anything else is escaped so a hand-edited config cannot break out of it.
fn js_string_body(token: &str) -> String {
    let mut out = String::with_capacity(token.len());
    for ch in token.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            let mut units = [0u16; 2];
            for unit in ch.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

/// The page for the settings currently in the replica.
pub fn render_page(replica: &ConfigReplica) -> String {
    let value = |key: &str, default: Value| replica.get(key).unwrap_or(default);
    let int = |key: &str, default: i64, min: i64, max: i64| clamp_int(&value(key, Value::from(default)), min, max);

    let settings = [
        ("@@WS_PORT@@", int("WEBSOCKET_PORT", 2231, 1, 65535).to_string()),
        ("@@MAX_MESSAGES@@", int("OBS_BROWSER_SOURCE_MAX_MESSAGES", 14, 1, 50).to_string()),
        ("@@DISPLAY_DURATION@@", int("OBS_BROWSER_SOURCE_DISPLAY_DURATION", 60, 1, 120).to_string()),
        ("@@FADEOUT_DURATION@@", int("OBS_BROWSER_SOURCE_FADEOUT_DURATION", 12, 0, 120).to_string()),
        ("@@FONT_SIZE@@", int("OBS_BROWSER_SOURCE_FONT_SIZE", 40, 10, 200).to_string()),
        (
            "@@FONT_COLOR@@",
            normalize_hex_color(&value("OBS_BROWSER_SOURCE_FONT_COLOR", Value::from("#FFFFFF")), "#FFFFFF"),
        ),
        (
            "@@OUTLINE_THICKNESS@@",
            int("OBS_BROWSER_SOURCE_FONT_OUTLINE_THICKNESS", 3, 0, 20).to_string(),
        ),
        (
            "@@OUTLINE_COLOR@@",
            normalize_hex_color(
                &value("OBS_BROWSER_SOURCE_FONT_OUTLINE_COLOR", Value::from("#000000")),
                "#000000",
            ),
        ),
        (
            "@@WS_TOKEN@@",
            js_string_body(&replica.get_str("WEBSOCKET_AUTH_TOKEN").unwrap_or_default()),
        ),
    ];

    // A checkout with CRLF conversion must not change the bytes served.
    let mut page = PAGE_TEMPLATE.replace("\r\n", "\n");
    for (placeholder, text) in settings {
        page = page.replace(placeholder, &text);
    }
    page
}

struct Reply {
    status: &'static str,
    content_type: Option<&'static str>,
    body: Vec<u8>,
}

fn reply(status: &'static str, content_type: Option<&'static str>, body: impl Into<Vec<u8>>) -> Reply {
    Reply { status, content_type, body: body.into() }
}

/// The path of an HTTP request target, without query or fragment.
fn request_path(target: &str) -> &str {
    let target = target.split(['?', '#']).next().unwrap_or("");
    // Absolute form ("GET http://host/obs HTTP/1.1"): drop the authority.
    match target.strip_prefix("http://").or_else(|| target.strip_prefix("https://")) {
        Some(rest) => rest.find('/').map_or("", |at| &rest[at..]),
        None => target,
    }
}

fn route(head: &str, replica: &ConfigReplica) -> Reply {
    let mut parts = head.lines().next().unwrap_or("").split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return reply("400 Bad Request", None, "");
    };
    if method != "GET" {
        return reply("501 Not Implemented", Some("text/plain; charset=utf-8"), "unsupported method\n");
    }
    match request_path(target) {
        "/" | "/obs" => reply("200 OK", Some("text/html; charset=utf-8"), render_page(replica)),
        "/health" => reply("200 OK", Some("text/plain; charset=utf-8"), "ok"),
        _ => reply("404 Not Found", None, ""),
    }
}

async fn read_head(stream: &mut TcpStream) -> Option<String> {
    let mut head = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        head.extend_from_slice(&chunk[..read]);
        let complete = head.windows(4).any(|w| w == b"\r\n\r\n") || head.windows(2).any(|w| w == b"\n\n");
        if complete {
            return Some(String::from_utf8_lossy(&head).into_owned());
        }
        if head.len() > MAX_HEAD_BYTES {
            return None;
        }
    }
}

async fn handle(mut stream: TcpStream, replica: Arc<ConfigReplica>) {
    let Ok(Some(head)) = tokio::time::timeout(HEAD_TIMEOUT, read_head(&mut stream)).await else {
        return;
    };
    let reply = route(&head, &replica);
    let mut response = format!("HTTP/1.1 {}\r\n", reply.status);
    if let Some(content_type) = reply.content_type {
        response.push_str(&format!("Content-Type: {content_type}\r\n"));
        if reply.status.starts_with("200") {
            response.push_str(&format!("Cache-Control: {NO_CACHE}\r\n"));
        }
    }
    response.push_str(&format!("Content-Length: {}\r\nConnection: close\r\n\r\n", reply.body.len()));
    let mut bytes = response.into_bytes();
    bytes.extend_from_slice(&reply.body);
    let _ = stream.write_all(&bytes).await;
    let _ = stream.shutdown().await;
}

struct Running {
    host: String,
    port: u16,
    shutdown: watch::Sender<bool>,
}

pub struct ObsSink {
    replica: Arc<ConfigReplica>,
    running: Mutex<Option<Running>>,
}

impl ObsSink {
    pub fn new(replica: Arc<ConfigReplica>) -> Self {
        Self { replica, running: Mutex::new(None) }
    }

    /// Start serving (replacing a server on another address). Must be called
    /// from inside a Tokio runtime; the bind happens in the background and a
    /// failure is reported on stderr.
    pub fn start(&self, host: &str, port: u16) -> Result<(), String> {
        if is_wildcard(host) {
            return Err(format!("refusing to serve the OBS page on wildcard address {host}"));
        }
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| "OBS sink needs a Tokio runtime".to_string())?;
        let mut running = self.running.lock().unwrap();
        if running.as_ref().is_some_and(|r| r.host == host && r.port == port) {
            return Ok(());
        }
        if let Some(previous) = running.take() {
            let _ = previous.shutdown.send(true);
        }
        let (shutdown, shutdown_rx) = watch::channel(false);
        runtime.spawn(serve(host.to_string(), port, Arc::clone(&self.replica), shutdown_rx));
        *running = Some(Running { host: host.to_string(), port, shutdown });
        Ok(())
    }

    pub fn stop(&self) {
        if let Some(running) = self.running.lock().unwrap().take() {
            let _ = running.shutdown.send(true);
        }
    }
}

async fn serve(host: String, port: u16, replica: Arc<ConfigReplica>, mut shutdown: watch::Receiver<bool>) {
    let listener = match bind(&host, port, &mut shutdown).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("[sinks] obs: {error}");
            return;
        }
    };
    loop {
        tokio::select! {
            _ = shutdown.changed() => return,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => { tokio::spawn(handle(stream, Arc::clone(&replica))); }
                Err(error) => eprintln!("[sinks] obs accept: {error}"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn clamp_follows_python_int_then_min_max() {
        assert_eq!(clamp_int(&json!(5), 1, 10), 5);
        assert_eq!(clamp_int(&json!(-3), 0, 10), 0);
        assert_eq!(clamp_int(&json!(99), 0, 10), 10);
        assert_eq!(clamp_int(&json!(2.9), 0, 10), 2);
        assert_eq!(clamp_int(&json!(" 7 "), 0, 10), 7);
        assert_eq!(clamp_int(&json!("12.5"), 0, 10), 0);
        assert_eq!(clamp_int(&json!(true), 0, 10), 1);
        assert_eq!(clamp_int(&Value::Null, 3, 10), 3);
        assert_eq!(clamp_int(&json!(1e30), 0, 10), 10);
    }

    #[test]
    fn colours_must_be_six_hex_digits() {
        assert_eq!(normalize_hex_color(&json!("#AbCdEf"), "#000000"), "#AbCdEf");
        assert_eq!(normalize_hex_color(&json!("  #abcdef\n"), "#000000"), "#abcdef");
        for bad in [json!("red"), json!("#12345"), json!("#1234567"), json!("#GGGGGG"), json!(5), Value::Null] {
            assert_eq!(normalize_hex_color(&bad, "#000000"), "#000000", "{bad}");
        }
    }

    #[test]
    fn a_token_cannot_break_out_of_the_js_string() {
        assert_eq!(js_string_body("abc_DEF-123"), "abc_DEF-123");
        let escaped = js_string_body("a\"b</script>\n\u{1F642}");
        assert!(!escaped.contains(['"', '<', '\n']), "{escaped}");
        assert_eq!(js_string_body("\"" ), "\\u0022");
        assert_eq!(js_string_body("\u{1F642}"), "\\ud83d\\ude42");
    }

    #[test]
    fn request_targets_lose_query_fragment_and_authority() {
        assert_eq!(request_path("/obs?x=1"), "/obs");
        assert_eq!(request_path("/obs#top"), "/obs");
        assert_eq!(request_path("http://localhost:2232/obs?x"), "/obs");
        assert_eq!(request_path("http://localhost:2232"), "");
        assert_eq!(request_path("/health/"), "/health/");
    }

    #[test]
    fn start_refuses_wildcard_hosts_before_touching_the_network() {
        let sink = ObsSink::new(Arc::new(ConfigReplica::default()));
        for host in ["0.0.0.0", "::"] {
            assert!(sink.start(host, 2232).unwrap_err().contains("wildcard"));
        }
    }
}
