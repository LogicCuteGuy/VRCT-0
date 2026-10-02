//! Calls from the Python sidecar into Rust, with an answer.
//!
//! Sinks are fire-and-forget; translation and (later) transcription need a
//! result back. Python writes `/internal/rpc/request {id, method, params}` to
//! stdout and waits; Rust runs the method and writes
//! `/internal/rpc/response {id, ok, result | error}` to the sidecar's stdin.
//! The correlation id is what lets the pipeline's worker threads each wait on
//! their own call, since the UI protocol itself routes by endpoint only.
//!
//! Like sinks, each method is switched on by name through `VRCT_RUST_RPC`, so a
//! standalone Python run (or a method not ported yet) is unchanged.
//!
//! Request lines carry chat text and API keys: they are never logged, and
//! never reach the UI.

use std::collections::HashMap;
use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD, Engine};
use futures_util::future::BoxFuture;
use serde_json::{json, Value};

use crate::protocol::{sidecar_line, Response};
#[cfg(feature = "ct2")]
use crate::translation::ct2;
use crate::translation::{catalog, deepl, llm, text};

/// Env var telling the sidecar which methods Rust implements (comma separated).
pub const RPC_ENV_NAME: &str = "VRCT_RUST_RPC";
/// Methods this build implements; every entry needs a handler in `Rpc::new`.
pub const IMPLEMENTED: &[&str] = &[
    "translate.llm",
    "translate.deepl",
    "translate.deepl.check",
    "llm.auth_check",
    "llm.models",
    "translate.text",
    #[cfg(feature = "ct2")]
    "ct2.load",
    #[cfg(feature = "ct2")]
    "ct2.translate",
];

const REQUEST: &str = "/internal/rpc/request";
const RESPONSE: &str = "/internal/rpc/response";

pub fn rpc_env_value() -> String {
    IMPLEMENTED.join(",")
}

/// Where answers go: a line on the sidecar's stdin.
pub trait LineWriter: Send + Sync {
    fn write_line(&self, line: &str) -> Result<(), String>;
}

type Handler = Arc<dyn Fn(Value) -> BoxFuture<'static, Result<Value, String>> + Send + Sync>;

pub struct Rpc {
    handlers: HashMap<&'static str, Handler>,
    writer: Arc<dyn LineWriter>,
}

impl Rpc {
    pub fn new(writer: Arc<dyn LineWriter>) -> Self {
        let rpc = Self { handlers: HashMap::new(), writer };
        let rpc = rpc.method("translate.llm", |params| async move {
            let request: llm::Request = serde_json::from_value(params).map_err(|e| format!("bad params: {e}"))?;
            llm::translate(request).await.map(Value::String)
        })
        .method("translate.deepl", |params| async move {
            let request: deepl::Request = serde_json::from_value(params).map_err(|e| format!("bad params: {e}"))?;
            deepl::translate(request).await.map(Value::String)
        })
        .method("translate.deepl.check", |params| async move {
            let request: deepl::Check = serde_json::from_value(params).map_err(|e| format!("bad params: {e}"))?;
            deepl::check(request).await.map(Value::Bool)
        })
        .method("translate.text", |params| async move {
            let request: text::Request = serde_json::from_value(params).map_err(|e| format!("bad params: {e}"))?;
            text::translate(request).await.map(|outcome| outcome.to_json())
        })
        .method("llm.auth_check", |params| async move {
            let target: catalog::Target = serde_json::from_value(params).map_err(|e| format!("bad params: {e}"))?;
            catalog::auth_check(target).await.map(Value::Bool)
        })
        .method("llm.models", |params| async move {
            let target: catalog::Target = serde_json::from_value(params).map_err(|e| format!("bad params: {e}"))?;
            catalog::models(target).await.map(|ids| json!(ids))
        });
        #[cfg(feature = "ct2")]
        let rpc = rpc.ct2_methods();
        rpc
    }

    /// The local model is loaded and run on blocking threads: loading reads
    /// hundreds of MB and a translation keeps the CPU busy for a while.
    #[cfg(feature = "ct2")]
    fn ct2_methods(self) -> Self {
        let engine = Arc::new(ct2::Engine::default());
        let loader = Arc::clone(&engine);
        self.method("ct2.load", move |params| {
            let engine = Arc::clone(&loader);
            async move {
                let request: ct2::LoadRequest = serde_json::from_value(params).map_err(|e| format!("bad params: {e}"))?;
                tokio::task::spawn_blocking(move || engine.load(&request))
                    .await
                    .map_err(|_| "internal error".to_string())?
                    .map(|()| Value::Bool(true))
            }
        })
        .method("ct2.translate", move |params| {
            let engine = Arc::clone(&engine);
            async move {
                let request: ct2::TranslateRequest =
                    serde_json::from_value(params).map_err(|e| format!("bad params: {e}"))?;
                tokio::task::spawn_blocking(move || engine.translate(&request))
                    .await
                    .map_err(|_| "internal error".to_string())?
                    .map(Value::String)
            }
        })
    }

    /// Register a method. Anything a build advertises in `IMPLEMENTED` must be
    /// registered by `new`.
    pub fn method<F, Fut>(mut self, name: &'static str, handler: F) -> Self
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<Value, String>> + Send + 'static,
    {
        self.handlers.insert(name, Arc::new(move |params| Box::pin(handler(params))));
        self
    }

    /// Run the call a request line asks for. Returns true when the line was an
    /// RPC request, so the caller keeps it away from the UI. Must be called
    /// from inside a Tokio runtime; the answer is written when the method ends.
    pub fn ingest(&self, response: &Response) -> bool {
        if response.endpoint != REQUEST {
            return false;
        }
        let Some(id) = response.result.get("id").and_then(Value::as_u64) else {
            // Nobody to answer; Python's call will time out.
            eprintln!("[rpc] request without an id");
            return true;
        };
        let method = response.result.get("method").and_then(Value::as_str).unwrap_or_default();
        let params = response.result.get("params").cloned().unwrap_or(Value::Null);

        let Some(handler) = self.handlers.get(method).cloned() else {
            answer(self.writer.as_ref(), id, Err(format!("unknown method {method:?}")));
            return true;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            answer(self.writer.as_ref(), id, Err("no async runtime".into()));
            return true;
        };
        let writer = Arc::clone(&self.writer);
        let method = method.to_string();
        runtime.spawn(async move {
            // Own task so a panicking method answers with an error instead of
            // leaving Python waiting for its timeout.
            let outcome = match tokio::spawn(handler(params)).await {
                Ok(outcome) => outcome,
                Err(_) => Err("internal error".to_string()),
            };
            if let Err(message) = &outcome {
                eprintln!("[rpc] {method} failed: {message}");
            }
            answer(writer.as_ref(), id, outcome);
        });
        true
    }
}

fn answer(writer: &dyn LineWriter, id: u64, outcome: Result<Value, String>) {
    let body = match outcome {
        Ok(result) => json!({"id": id, "ok": true, "result": result}),
        Err(error) => json!({"id": id, "ok": false, "error": error}),
    };
    let line = sidecar_line(RESPONSE, Some(&STANDARD.encode(body.to_string())));
    if let Err(error) = writer.write_line(&line) {
        eprintln!("[rpc] cannot answer call {id}: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_implemented_method_has_a_handler() {
        struct Nowhere;
        impl LineWriter for Nowhere {
            fn write_line(&self, _: &str) -> Result<(), String> {
                Ok(())
            }
        }
        let rpc = Rpc::new(Arc::new(Nowhere));
        for name in IMPLEMENTED {
            assert!(rpc.handlers.contains_key(name), "{name} is advertised but has no handler");
        }
    }
}
