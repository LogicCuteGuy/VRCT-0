//! In-process backend. Serves ported endpoints in Rust and forwards the rest
//! to the legacy Python sidecar until it is removed.

mod sidecar;
mod updates;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tauri::{AppHandle, Emitter};
use vrct_core::config::{self, ConfigReplica};
use vrct_core::protocol::Response;
use vrct_core::router::{ResponseSink, Router};
use vrct_core::rpc::{LineWriter, Rpc};
use vrct_core::sinks::Sinks;

use sidecar::Sidecar;

/// UI event carrying one `{status, endpoint, result}` response.
const RESPONSE_EVENT: &str = "backend-response";

struct TauriSink(AppHandle);

impl ResponseSink for TauriSink {
    fn emit(&self, response: Response) {
        let _ = self.0.emit(RESPONSE_EVENT, response);
    }
}

pub struct Backend {
    router: Arc<Router>,
    sidecar: Arc<Sidecar>,
    replica: Arc<ConfigReplica>,
    sinks: Arc<Sinks>,
    rpc: Arc<Rpc>,
    started: AtomicBool,
}

impl Backend {
    pub fn new(app: &AppHandle) -> Result<Self, String> {
        let sink: Arc<dyn ResponseSink> = Arc::new(TauriSink(app.clone()));
        let sidecar = Arc::new(Sidecar::default());
        let replica = Arc::new(ConfigReplica::default());
        let sinks = Arc::new(Sinks::new(Arc::clone(&replica)));
        let rpc = Arc::new(Rpc::new(sidecar.clone() as Arc<dyn LineWriter>));

        let router = Router::new(Arc::clone(&sink)).with_fallback(sidecar.clone());
        let router = config::register_getters(router, &replica);
        let router = updates::register(router, app.clone(), Arc::clone(&replica), sink)?;

        Ok(Self {
            router: Arc::new(router),
            sidecar,
            replica,
            sinks,
            rpc,
            started: AtomicBool::new(false),
        })
    }

    /// Start the sidecar once. The UI calls this after it has subscribed to
    /// `backend-response`, so no early response is lost.
    pub fn start(&self, app: &AppHandle) -> Result<(), String> {
        if self.started.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.sidecar
            .spawn(
                app,
                Arc::clone(&self.router),
                Arc::clone(&self.replica),
                Arc::clone(&self.sinks),
                Arc::clone(&self.rpc),
            )
            .inspect_err(|_| self.started.store(false, Ordering::SeqCst))
    }

    pub fn request(&self, endpoint: String, data: Option<String>) {
        self.router.dispatch(endpoint, data);
    }
}
