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
use vrct_core::settings::{system::production_env, Settings};
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

/// Opens config.json next to the application, the folder the sidecar kept it in. A file that
/// cannot be read is reported and left alone; the settings then run on their defaults.
fn open_settings(app: &AppHandle) -> Result<Arc<Settings>, String> {
    let exe = std::env::current_exe().map_err(|error| format!("cannot find the application folder: {error}"))?;
    let folder = exe.parent().ok_or("the application has no folder")?;
    let version = app.package_info().version.to_string();
    let (settings, report) = Settings::open(production_env(&version, folder));
    if let Some(report) = report {
        eprintln!("[settings] config.json was not read: {report}");
    }
    Ok(Arc::new(settings))
}

pub struct Backend {
    router: Arc<Router>,
    sidecar: Arc<Sidecar>,
    settings: Arc<Settings>,
    replica: Arc<ConfigReplica>,
    sinks: Arc<Sinks>,
    rpc: Arc<Rpc>,
    started: AtomicBool,
}

impl Backend {
    pub fn new(app: &AppHandle) -> Result<Self, String> {
        let sink: Arc<dyn ResponseSink> = Arc::new(TauriSink(app.clone()));
        let sidecar = Arc::new(Sidecar::default());
        let settings = open_settings(app)?;
        let replica = Arc::new(ConfigReplica::over(Arc::clone(&settings)));
        let sinks = Arc::new(Sinks::new(Arc::clone(&replica)));
        let writer = sidecar.clone() as Arc<dyn LineWriter>;
        let rpc = Rpc::new(Arc::clone(&writer));
        // Audio is captured in Rust only where the ONNX Runtime for the VAD is installed (Windows);
        // anywhere else the methods are not advertised and Python keeps its own capture.
        #[cfg(windows)]
        let rpc = match vrct_core::audio::host::WasapiFactory::locate() {
            Some(factory) => rpc.with_audio(Arc::new(vrct_core::audio::host::AudioHost::new(Arc::new(factory), writer))),
            None => rpc,
        };
        let rpc = Arc::new(rpc);

        let router = Router::new(Arc::clone(&sink)).with_fallback(sidecar.clone());
        let router = config::register_getters(router, &replica);
        let router = updates::register(router, app.clone(), Arc::clone(&replica), sink)?;

        Ok(Self {
            router: Arc::new(router),
            sidecar,
            settings,
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

    /// Writes a setting that is still waiting for its debounce; called when the application exits.
    pub fn shutdown(&self) {
        if let Err(error) = self.settings.flush() {
            eprintln!("[settings] {error}");
        }
    }

    pub fn request(&self, endpoint: String, data: Option<String>) {
        self.router.dispatch(endpoint, data);
    }
}
