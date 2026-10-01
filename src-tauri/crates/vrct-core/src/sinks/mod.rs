//! Outputs the Python pipeline fans a message out to (OSC, later WebSocket,
//! clipboard, overlay, ...). Python emits `/internal/<sink>/...` lines and Rust
//! performs the output.
//!
//! Each sink is switched on per name through `VRCT_RUST_SINKS`: Python only
//! stops doing an output itself when Rust says it has taken it over, so a
//! standalone Python run (and any sink not ported yet) is unchanged.

pub mod clipboard;
pub mod logger;
pub mod osc;
pub mod websocket;

use std::sync::Arc;

use serde_json::Value;

use crate::config::ConfigReplica;
use crate::protocol::Response;

use clipboard::ClipboardSink;
use logger::LoggerSink;
use osc::OscSink;
use websocket::WebSocketSink;

/// Env var telling the sidecar which sinks Rust performs (comma separated).
pub const SINKS_ENV_NAME: &str = "VRCT_RUST_SINKS";
/// Sinks this build implements; keep in step with `Sinks::ingest`. The
/// clipboard sink (focus the game, paste) is Windows-only; elsewhere Python
/// keeps its own clipboard code.
#[cfg(windows)]
pub const IMPLEMENTED: &[&str] = &["osc", "websocket", "logger", "clipboard"];
#[cfg(not(windows))]
pub const IMPLEMENTED: &[&str] = &["osc", "websocket", "logger"];

const OSC_TYPING: &str = "/internal/osc/typing";
const OSC_MESSAGE: &str = "/internal/osc/message";
const WS_START: &str = "/internal/websocket/start";
const WS_STOP: &str = "/internal/websocket/stop";
const WS_BROADCAST: &str = "/internal/websocket/broadcast";
const LOG_START: &str = "/internal/logger/start";
const LOG_STOP: &str = "/internal/logger/stop";
const LOG_LINE: &str = "/internal/logger/line";
const CLIPBOARD_COPY_PASTE: &str = "/internal/clipboard/copy_paste";

pub fn sinks_env_value() -> String {
    IMPLEMENTED.join(",")
}

pub struct Sinks {
    osc: OscSink,
    websocket: WebSocketSink,
    logger: LoggerSink,
    clipboard: ClipboardSink,
}

impl Sinks {
    pub fn new(replica: Arc<ConfigReplica>) -> Self {
        Self {
            osc: OscSink::new(Arc::clone(&replica)),
            websocket: WebSocketSink::new(replica),
            logger: LoggerSink::new(),
            clipboard: ClipboardSink::new(),
        }
    }

    /// Perform the output a sink line asks for. Returns true when the line
    /// was one, so the caller keeps it away from the UI. A failed output is
    /// reported on stderr and never blocks the sidecar's output loop.
    pub fn ingest(&self, response: &Response) -> bool {
        let outcome = match response.endpoint.as_str() {
            OSC_TYPING => self.osc.typing(flag(&response.result, "flag")),
            OSC_MESSAGE => match response.result.get("message").and_then(Value::as_str) {
                Some(message) => self
                    .osc
                    .message(message, flag(&response.result, "notification")),
                None => Err("malformed /internal/osc/message".to_string()),
            },
            WS_START => {
                let host = response.result.get("host").and_then(Value::as_str);
                let port = response
                    .result
                    .get("port")
                    .and_then(Value::as_u64)
                    .and_then(|port| u16::try_from(port).ok());
                match (host, port) {
                    (Some(host), Some(port)) => self.websocket.start(host, port),
                    _ => Err("malformed /internal/websocket/start".to_string()),
                }
            }
            WS_STOP => {
                self.websocket.stop();
                Ok(())
            }
            WS_BROADCAST => match response.result.get("text").and_then(Value::as_str) {
                Some(text) => {
                    self.websocket.broadcast(text);
                    Ok(())
                }
                None => Err("malformed /internal/websocket/broadcast".to_string()),
            },
            LOG_START => match response.result.get("path").and_then(Value::as_str) {
                Some(path) => self.logger.start(path),
                None => Err("malformed /internal/logger/start".to_string()),
            },
            LOG_STOP => {
                self.logger.stop();
                Ok(())
            }
            LOG_LINE => match response.result.get("text").and_then(Value::as_str) {
                Some(text) => self.logger.info(text),
                None => Err("malformed /internal/logger/line".to_string()),
            },
            CLIPBOARD_COPY_PASTE => match response.result.get("text").and_then(Value::as_str) {
                Some(text) => self
                    .clipboard
                    .copy_and_paste(text, response.result.get("window").and_then(Value::as_str)),
                None => Err("malformed /internal/clipboard/copy_paste".to_string()),
            },
            _ => return false,
        };
        if let Err(error) = outcome {
            eprintln!("[sinks] {error}");
        }
        true
    }
}

fn flag(result: &Value, key: &str) -> bool {
    result.get(key).and_then(Value::as_bool).unwrap_or(false)
}
