//! Native application backend: every public route is handled in this process.
mod pipeline;
mod updates;
use serde_json::json;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex, Weak,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};
use vrct_core::{
    config::ConfigReplica,
    native::{NativeController, Services},
    protocol::Response,
    router::{ResponseSink, Router},
    settings::{
        system::{production_env, SystemDevices},
        Settings,
    },
    sinks::Sinks,
};

struct TauriSink {
    app: AppHandle,
    telemetry: Mutex<Option<Weak<vrct_core::telemetry::Telemetry>>>,
}
impl ResponseSink for TauriSink {
    fn emit(&self, response: Response) {
        if response.status >= 400 {
            if let Some(code) = response.result["error_code"].as_str() {
                if let Some(telemetry) = self
                    .telemetry
                    .lock()
                    .unwrap()
                    .as_ref()
                    .and_then(Weak::upgrade)
                {
                    telemetry.track_error(code);
                }
            }
        }
        let _ = self.app.emit("backend-response", response);
    }
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn open_settings(app: &AppHandle) -> Result<Arc<Settings>, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let folder = exe.parent().ok_or("the application has no folder")?;
    let saved = std::fs::read(folder.join("config.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    let (settings, report) = Settings::open(production_env(
        &app.package_info().version.to_string(),
        folder,
    ));
    if let Some(report) = report {
        eprintln!("[settings] config.json was not read: {report}");
    }
    #[cfg(windows)]
    if let (Some(saved), Ok(devices)) = (saved, vrct_core::audio::wasapi::list_devices()) {
        if let Some(device) = saved["SELECTED_MIC_DEVICE"]
            .as_str()
            .filter(|_| saved["SELECTED_MIC_HOST"].as_str() != Some(vrct_core::audio::devices::ASIO_HOST))
            .and_then(|name| devices.resolve_mic(name))
        {
            let _ = settings.set(
                "SELECTED_MIC_HOST",
                json!(vrct_core::audio::devices::WASAPI_HOST),
            );
            let _ = settings.set("SELECTED_MIC_DEVICE", json!(device.name));
        }
        if let Some(device) = saved["SELECTED_SPEAKER_DEVICE"]
            .as_str()
            .filter(|_| saved["SELECTED_SPEAKER_HOST"].as_str() != Some(vrct_core::audio::devices::ASIO_HOST))
            .and_then(|name| devices.resolve_speaker(name))
        {
            let _ = settings.set("SELECTED_SPEAKER_DEVICE", json!(device.name));
        }
    }
    #[cfg(not(windows))]
    let _ = saved;
    Ok(Arc::new(settings))
}
pub struct Backend {
    router: Arc<Router>,
    native: Arc<NativeController>,
    started: AtomicBool,
    heartbeat: Arc<AtomicU64>,
    stopped: Arc<AtomicBool>,
}
impl Backend {
    pub fn new(app: &AppHandle) -> Result<Self, String> {
        let handle = tauri::async_runtime::handle().inner().clone();
        let _entered = handle.enter();
        let sink = Arc::new(TauriSink {
            app: app.clone(),
            telemetry: Mutex::new(None),
        });
        let settings = open_settings(app)?;
        let replica = Arc::new(ConfigReplica::over(settings.clone()));
        let sinks = Arc::new(Sinks::new(replica.clone()));
        #[cfg(feature = "ct2")]
        let engine = Arc::new(vrct_core::translation::ct2::Engine::default());
        let runtime = pipeline::create(
            settings.clone(),
            sinks.clone(),
            sink.clone(),
            #[cfg(feature = "ct2")]
            engine.clone(),
        );
        let services = Services::new(
            settings.clone(),
            runtime,
            sinks,
            sink.clone(),
            vrct_core::native::resources(&std::env::current_exe().map_err(|e| e.to_string())?),
            #[cfg(feature = "ct2")]
            engine,
        );
        *sink.telemetry.lock().unwrap() = Some(Arc::downgrade(&services.telemetry));
        let native = NativeController::new(services, Arc::new(SystemDevices));
        let mut router = native.register(Router::new(sink.clone()));
        router = updates::register(router, app.clone(), replica, sink)?;
        let heartbeat = Arc::new(AtomicU64::new(now()));
        let stopped = Arc::new(AtomicBool::new(false));
        let beat = heartbeat.clone();
        router = router.handle("/run/feed_watchdog", move |_| {
            let beat = beat.clone();
            async move {
                beat.store(now(), Ordering::Relaxed);
                (200, json!(true))
            }
        });
        for (route, key) in [
            ("/run/open_filepath_logs", "PATH_LOGS"),
            ("/run/open_filepath_config_file", "PATH_CONFIG"),
        ] {
            let settings = settings.clone();
            router = router.handle(route, move |_| {
                let settings = settings.clone();
                async move {
                    let path = settings.get_str(key).unwrap_or_default();
                    #[cfg(windows)]
                    let result = std::process::Command::new("explorer.exe")
                        .arg(&path)
                        .spawn()
                        .map(|_| ())
                        .map_err(|e| e.to_string());
                    #[cfg(not(windows))]
                    let result = tauri_plugin_opener::open_path(&path, None::<&str>)
                        .map_err(|e| e.to_string());
                    match result {
                        Ok(()) => (200, json!(true)),
                        Err(e) => (500, json!(e)),
                    }
                }
            });
        }
        let exit = app.clone();
        let services = native.services.clone();
        router = router.handle("/run/shutdown", move |_| {
            let exit = exit.clone();
            let services = services.clone();
            async move {
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    let _ = tokio::task::spawn_blocking(move || services.shutdown()).await;
                    exit.exit(0);
                });
                (200, json!(true))
            }
        });
        let missing: Vec<_> = vrct_core::controller::contract()
            .iter()
            .filter(|row| !router.is_owned(&row.endpoint))
            .map(|row| row.endpoint.clone())
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "Native route coverage is incomplete: {}",
                missing.join(", ")
            ));
        }
        crate::startup_log(&format!(
            "Native backend registered {} routes; Python is not used",
            vrct_core::controller::contract().len()
        ));
        Ok(Self {
            router: Arc::new(router),
            native,
            started: AtomicBool::new(false),
            heartbeat,
            stopped,
        })
    }
    pub fn start(&self, app: &AppHandle) -> Result<(), String> {
        if self.started.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.heartbeat.store(now(), Ordering::Relaxed);
        let native = self.native.clone();
        let events = app.clone();
        let router = self.router.clone();
        tauri::async_runtime::spawn(async move {
            match native.initialize().await {
                Ok(()) => {
                    crate::startup_log("Native initialization completed");
                    router.dispatch("/run/software_update_info".into(), None);
                }
                Err(error) => {
                    crate::startup_log(&format!("Native initialization failed: {error}"));
                    let _ = events.emit("backend-error", error);
                }
            }
        });
        let beat = self.heartbeat.clone();
        let stopped = self.stopped.clone();
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;
                if stopped.load(Ordering::Relaxed) {
                    return;
                }
                if now().saturating_sub(beat.load(Ordering::Relaxed)) > 90 {
                    crate::startup_log("UI watchdog expired");
                    app.exit(0);
                    return;
                }
            }
        });
        Ok(())
    }
    pub fn shutdown(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        self.native.services.shutdown();
    }
    pub fn request(&self, endpoint: String, data: Option<String>) {
        self.router.dispatch(endpoint, data);
    }
}
