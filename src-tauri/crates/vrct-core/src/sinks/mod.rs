//! Outputs the Python pipeline fans a message out to (OSC, later WebSocket,
//! clipboard, overlay, ...). Python emits `/internal/<sink>/...` lines and Rust
//! performs the output.
//!
//! Each sink is switched on per name through `VRCT_RUST_SINKS`: Python only
//! stops doing an output itself when Rust says it has taken it over, so a
//! standalone Python run (and any sink not ported yet) is unchanged.

pub mod osc;

use std::sync::Arc;

use serde_json::Value;

use crate::config::ConfigReplica;
use crate::protocol::Response;

use osc::OscSink;

/// Env var telling the sidecar which sinks Rust performs (comma separated).
pub const SINKS_ENV_NAME: &str = "VRCT_RUST_SINKS";
/// Sinks this build implements; keep in step with `Sinks::ingest`.
pub const IMPLEMENTED: &[&str] = &["osc"];

const OSC_TYPING: &str = "/internal/osc/typing";
const OSC_MESSAGE: &str = "/internal/osc/message";

pub fn sinks_env_value() -> String {
    IMPLEMENTED.join(",")
}

pub struct Sinks {
    osc: OscSink,
}

impl Sinks {
    pub fn new(replica: Arc<ConfigReplica>) -> Self {
        Self { osc: OscSink::new(replica) }
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
