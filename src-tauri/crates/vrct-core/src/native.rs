//! Assembly of the full native controller, initialization and feature services.
//! No subprocess, Python bridge or optional legacy fallback is constructed here.
use crate::{
    auth::{Auth, HttpCatalog},
    controller::{Controller, Effects},
    models::Manager,
    protocol::Response,
    router::{Reply, ResponseSink, Router},
    runtime::Runtime,
    settings::{Devices, Settings},
    sinks::Sinks,
};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

pub struct Services {
    settings: Arc<Settings>,
    pub runtime: Arc<Runtime>,
    sinks: Arc<Sinks>,
    pub models: Arc<Manager>,
    sink: Arc<dyn ResponseSink>,
    resources: PathBuf,
    handle: tokio::runtime::Handle,
    transliterator: Mutex<bool>,
    overlay: Mutex<Option<Arc<crate::overlay::Overlay>>>,
    ocr: Mutex<Option<Arc<crate::ocr::OcrService>>>,
    stopped: AtomicBool,
    lifecycle: std::sync::RwLock<()>,
    closed: tokio::sync::watch::Sender<bool>,
    devices: Arc<crate::device_monitor::DeviceMonitor>,
    pub telemetry: Arc<crate::telemetry::Telemetry>,
    #[cfg(feature = "ct2")]
    engine: Arc<crate::translation::ct2::Engine>,
}
impl Services {
    pub fn new(
        settings: Arc<Settings>,
        runtime: Arc<Runtime>,
        sinks: Arc<Sinks>,
        sink: Arc<dyn ResponseSink>,
        resources: PathBuf,
        #[cfg(feature = "ct2")] engine: Arc<crate::translation::ct2::Engine>,
    ) -> Arc<Self> {
        let models = Arc::new(Manager::new(
            settings.get_str("PATH_LOCAL").unwrap_or_default(),
            sink.clone(),
        ));
        let devices = crate::device_monitor::DeviceMonitor::native(settings.clone(), sink.clone());
        let telemetry = crate::telemetry::Telemetry::new(
            settings.clone(),
            settings.get_str("PATH_LOCAL").unwrap_or_default(),
            &settings.get_str("VERSION").unwrap_or_default(),
            &settings
                .get_str("SELECTED_RELEASE_CHANNEL")
                .unwrap_or_default(),
            tokio::runtime::Handle::current(),
        );
        Arc::new(Self {
            settings,
            runtime,
            sinks,
            models,
            sink,
            resources,
            handle: tokio::runtime::Handle::current(),
            transliterator: Mutex::new(false),
            overlay: Mutex::new(None),
            ocr: Mutex::new(None),
            stopped: AtomicBool::new(false),
            lifecycle: std::sync::RwLock::new(()),
            closed: tokio::sync::watch::channel(false).0,
            devices,
            telemetry,
            #[cfg(feature = "ct2")]
            engine,
        })
    }
    pub fn resource(&self, name: &str) -> PathBuf {
        self.resources.join(name)
    }
    pub fn emit(&self, status: u16, endpoint: &str, result: Value) {
        self.sink.emit(Response::new(status, endpoint, result));
    }
    fn overlay(&self) -> Result<Arc<crate::overlay::Overlay>, String> {
        let mut slot = self.overlay.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(overlay) = slot.as_ref() {
            return Ok(overlay.clone());
        }
        let overlay = crate::overlay::Overlay::new(
            self.resource("fonts"),
            self.settings.clone(),
            Arc::new(move |error| {
                eprintln!("[overlay] {error}");
            }),
        )?;
        self.runtime.install_overlay(overlay.clone());
        *slot = Some(overlay.clone());
        Ok(overlay)
    }
    fn ocr(&self) -> Arc<crate::ocr::OcrService> {
        let mut slot = self.ocr.lock().unwrap_or_else(|p| p.into_inner());
        slot.get_or_insert_with(|| {
            let runtime = self.runtime.clone();
            let sink = self.sink.clone();
            let settings = self.settings.clone();
            Arc::new(crate::ocr::OcrService::new(
                self.settings.clone(),
                Arc::new(move |value| runtime.ocr_message(value)),
                Arc::new(move |error| {
                    let _ = settings.set("ENABLE_OCR_CAPTURE", json!(false));
                    let (_, result) = crate::errors::reply(
                        "OCR_DISABLED_MODEL_LOAD_FAILED",
                        json!(false),
                        Some(&error),
                    );
                    sink.emit(Response::new(400, "/run/enable_ocr_capture", result));
                }),
            ))
        })
        .clone()
    }
    pub fn load_translation(&self) -> Result<(), String> {
        if self.stopped.load(Ordering::SeqCst) {
            return Err("Backend has stopped".into());
        }
        let weight = self
            .settings
            .get_str("CTRANSLATE2_WEIGHT_TYPE")
            .unwrap_or_default();
        #[cfg(feature = "ct2")]
        {
            let _operation = self.handle.block_on(self.models.operation_guard());
            if self.stopped.load(Ordering::SeqCst) {
                return Err("Backend has stopped".into());
            }
            if !self.models.available("ctranslate2", &weight) {
                return Err(format!("Translation model {weight} is not downloaded"));
            }
            let device = self
                .settings
                .get("SELECTED_TRANSLATION_COMPUTE_DEVICE")
                .unwrap_or_default();
            let request = crate::translation::ct2::LoadRequest {
                path: PathBuf::from(self.settings.get_str("PATH_LOCAL").unwrap_or_default()),
                weight_type: weight,
                device: device["device"].as_str().unwrap_or("cpu").into(),
                device_index: device["device_index"].as_i64().unwrap_or(0) as i32,
                compute_type: self
                    .settings
                    .get_str("SELECTED_TRANSLATION_COMPUTE_TYPE")
                    .unwrap_or_else(|| "auto".into()),
            };
            if self.engine.is_configured(&request) {
                Ok(())
            } else {
                self.engine.load(&request)
            }
        }
        #[cfg(not(feature = "ct2"))]
        {
            let _ = weight;
            Err("This build must enable the ct2 feature for local models".into())
        }
    }
    pub fn refresh_models(&self) {
        for (kind, list, status, selected, engine) in [
            (
                "ctranslate2",
                "SELECTABLE_CTRANSLATE2_WEIGHT_TYPE_DICT",
                "SELECTABLE_TRANSLATION_ENGINE_STATUS",
                "CTRANSLATE2_WEIGHT_TYPE",
                "CTranslate2",
            ),
            (
                "whisper",
                "SELECTABLE_WHISPER_WEIGHT_TYPE_DICT",
                "SELECTABLE_TRANSCRIPTION_ENGINE_STATUS",
                "WHISPER_WEIGHT_TYPE",
                "Whisper",
            ),
        ] {
            let downloaded = self.models.all_status(kind);
            let weight = self.settings.get_str(selected).unwrap_or_default();
            let available = cfg!(feature = "ct2") && downloaded[&weight] == true;
            let _ = self.settings.set(list, downloaded);
            let mut statuses = self.settings.get(status).unwrap_or(json!({}));
            statuses[engine] = json!(available);
            let _ = self.settings.set(status, statuses);
        }
    }
    pub fn initialize(&self) -> Result<(), String> {
        let _startup = self.lifecycle.read().unwrap_or_else(|p| p.into_inner());
        if self.stopped.load(Ordering::SeqCst) {
            return Err("Backend has stopped".into());
        }
        if let Err(error) = self.runtime.install_osc() {
            eprintln!("[OSCQuery] {error}");
            let _ = self.settings.set("VRC_MIC_MUTE_SYNC", json!(false));
            self.emit(
                200,
                "/run/enable_osc_query",
                json!({"data":false,"disabled_functions":["vrc_mic_mute_sync"]}),
            );
        }
        for name in [
            "CONVERT_MESSAGE_TO_HIRAGANA",
            "OVERLAY_SMALL_LOG",
            "LOGGER_FEATURE",
            "WEBSOCKET_SERVER",
            "OBS_BROWSER_SOURCE",
        ] {
            if let Err(error) = self.apply_changed(name) {
                eprintln!("[initialization] {name}: {error}");
                let disable: &[&str] = match name {
                    "CONVERT_MESSAGE_TO_HIRAGANA" => {
                        &["CONVERT_MESSAGE_TO_HIRAGANA", "CONVERT_MESSAGE_TO_ROMAJI"]
                    }
                    "OVERLAY_SMALL_LOG" => &["OVERLAY_SMALL_LOG", "OVERLAY_LARGE_LOG"],
                    "WEBSOCKET_SERVER" | "OBS_BROWSER_SOURCE" => {
                        &["WEBSOCKET_SERVER", "OBS_BROWSER_SOURCE"]
                    }
                    _ => &[name],
                };
                for property in disable {
                    let _ = self.settings.set(property, json!(false));
                }
                let _ = self.apply_changed(name);
            }
        }
        self.refresh_models();
        if self
            .settings
            .get("SELECTABLE_TRANSLATION_ENGINE_STATUS")
            .unwrap_or_default()["CTranslate2"]
            == true
        {
            if let Err(error) = self.load_translation() {
                eprintln!("[initialization] local translation model: {error}");
                let mut status = self
                    .settings
                    .get("SELECTABLE_TRANSLATION_ENGINE_STATUS")
                    .unwrap_or_default();
                status["CTranslate2"] = json!(false);
                let _ = self
                    .settings
                    .set("SELECTABLE_TRANSLATION_ENGINE_STATUS", status);
            }
        }
        if self.stopped.load(Ordering::SeqCst) {
            return Err("Backend has stopped".into());
        }
        self.runtime.activate();
        self.devices.start()?;
        self.telemetry.start();
        Ok(())
    }
    pub fn shutdown(&self) {
        if self.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        self.models.cancel();
        self.closed.send_replace(true);
        let _close = self.lifecycle.write().unwrap_or_else(|p| p.into_inner());
        let _ = self.devices.shutdown();
        self.telemetry.shutdown();
        self.runtime.shutdown();
        if let Some(ocr) = self.ocr.lock().unwrap_or_else(|p| p.into_inner()).take() {
            ocr.shutdown();
        }
        if let Some(overlay) = self
            .overlay
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            if let Err(error) = overlay.shutdown() {
                eprintln!("[overlay shutdown] {error}");
            }
        }
        self.sinks.shutdown();
        let _ = self.settings.flush();
    }
    pub fn preview(&self, data: Value) -> Reply {
        let text = data.as_str().unwrap_or_default();
        if let Some(overlay) = self
            .overlay
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .filter(|o| o.available())
        {
            let language = self
                .settings
                .get("SELECTED_YOUR_LANGUAGES")
                .unwrap_or_default();
            let tab = self.settings.get_str("SELECTED_TAB_NO").unwrap_or_default();
            let language = language[&tab]["1"]["language"]
                .as_str()
                .unwrap_or("English");
            if self.settings.get_bool("OVERLAY_SMALL_LOG") == Some(true) {
                if let Err(e) = overlay.preview_small(text, language) {
                    return (500, json!(e));
                }
            }
            if self.settings.get_bool("OVERLAY_LARGE_LOG") == Some(true) {
                if let Err(e) = overlay.preview_large(text, language) {
                    return (500, json!(e));
                }
            }
        }
        (200, data)
    }
}
impl Effects for Services {
    fn changed(&self, name: &str) -> Result<(), String> {
        let _change = self.lifecycle.read().unwrap_or_else(|p| p.into_inner());
        self.apply_changed(name)
    }
}
impl Services {
    pub async fn download(&self, kind: &str, weight: &str) -> Result<PathBuf, String> {
        let mut closed = self.closed.subscribe();
        if *closed.borrow() {
            return Err("Backend has stopped".into());
        }
        tokio::select! {
            result=self.models.download(kind,weight)=>result,
            _=closed.wait_for(|closed|*closed)=>Err("Backend has stopped".into()),
        }
    }
    fn apply_changed(&self, name: &str) -> Result<(), String> {
        if self.stopped.load(Ordering::SeqCst) {
            return Err("Backend has stopped".into());
        }
        let _entered = self.handle.enter();
        match name {
            "CONVERT_MESSAGE_TO_HIRAGANA" | "CONVERT_MESSAGE_TO_ROMAJI" => {
                if self.settings.get_bool("CONVERT_MESSAGE_TO_HIRAGANA") == Some(true)
                    || self.settings.get_bool("CONVERT_MESSAGE_TO_ROMAJI") == Some(true)
                {
                    let mut loaded = self
                        .transliterator
                        .lock()
                        .unwrap_or_else(|p| p.into_inner());
                    if !*loaded {
                        self.runtime.install_transliterator(
                            crate::transliteration::Transliterator::load(
                                self.resource("transliteration"),
                            )?,
                        );
                        *loaded = true;
                    }
                }
            }
            "OVERLAY_SMALL_LOG" | "OVERLAY_LARGE_LOG" => {
                if self.settings.get_bool("OVERLAY_SMALL_LOG") == Some(true)
                    || self.settings.get_bool("OVERLAY_LARGE_LOG") == Some(true)
                {
                    self.overlay()?.start()?;
                } else if let Some(overlay) = self
                    .overlay
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .as_ref()
                {
                    overlay.shutdown()?;
                }
            }
            "ENABLE_OCR_CAPTURE" => {
                if self.settings.get_bool(name) == Some(true) {
                    self.ocr().start()?;
                } else if let Some(ocr) =
                    self.ocr.lock().unwrap_or_else(|p| p.into_inner()).as_ref()
                {
                    ocr.stop();
                }
            }
            name if name.starts_with("OCR_") => {
                if let Some(ocr) = self.ocr.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
                    ocr.configure();
                }
            }
            "OSC_IP_ADDRESS" | "OSC_PORT" => self.runtime.configure_osc()?,
            "VRC_MIC_MUTE_SYNC" => {
                if self.settings.get_bool(name) == Some(true) && !self.runtime.osc_query_available()
                {
                    return Err(
                        "OSCQuery mute synchronization requires a running local OSCQuery service"
                            .into(),
                    );
                }
            }
            "AUTO_MIC_SELECT" | "AUTO_SPEAKER_SELECT" => {
                self.devices.wake();
            }
            "ENABLE_TELEMETRY" => {
                if self.settings.get_bool(name) == Some(true) {
                    self.telemetry.start();
                } else {
                    self.telemetry.shutdown();
                }
            }
            "ENABLE_TRANSLATION"
            | "CTRANSLATE2_WEIGHT_TYPE"
            | "SELECTED_TRANSLATION_COMPUTE_TYPE"
            | "SELECTED_TRANSLATION_COMPUTE_DEVICE"
            | "SELECTED_TRANSLATION_ENGINES"
            | "SELECTED_TAB_NO" => {
                if self.settings.get_bool("ENABLE_TRANSLATION") == Some(true) {
                    let tab = self.settings.get_str("SELECTED_TAB_NO").unwrap_or_default();
                    if self
                        .settings
                        .get("SELECTED_TRANSLATION_ENGINES")
                        .unwrap_or_default()[&tab]
                        == "CTranslate2"
                    {
                        self.load_translation()?;
                    }
                }
            }
            "WEBSOCKET_SERVER" if self.settings.get_bool(name) == Some(false) => {
                let _ = self.settings.set("OBS_BROWSER_SOURCE", json!(false));
                self.sinks.configure("OBS_BROWSER_SOURCE", &self.settings)?;
            }
            _ => {}
        }
        self.sinks.configure(name, &self.settings)?;
        if name == "OBS_BROWSER_SOURCE" && self.settings.get_bool(name) == Some(true) {
            let _ = self.settings.set("WEBSOCKET_SERVER", json!(true));
        }
        if name.starts_with("OBS_BROWSER_SOURCE_")
            && self.settings.get_bool("OBS_BROWSER_SOURCE") == Some(true)
        {
            let get = |key: &str| self.settings.get(key).unwrap_or_default();
            self.sinks.broadcast(&json!({"type":"SETTINGS_UPDATED","settings":{"maxMessages":get("OBS_BROWSER_SOURCE_MAX_MESSAGES"),
                "displayDuration":get("OBS_BROWSER_SOURCE_DISPLAY_DURATION"),"fadeoutDuration":get("OBS_BROWSER_SOURCE_FADEOUT_DURATION"),
                "fontSize":get("OBS_BROWSER_SOURCE_FONT_SIZE"),"fontColor":get("OBS_BROWSER_SOURCE_FONT_COLOR"),
                "outlineThickness":get("OBS_BROWSER_SOURCE_FONT_OUTLINE_THICKNESS"),"outlineColor":get("OBS_BROWSER_SOURCE_FONT_OUTLINE_COLOR")}}).to_string());
        }
        Ok(())
    }
}

pub struct NativeController {
    pub services: Arc<Services>,
    pub controller: Arc<Controller>,
    pub auth: Arc<Auth>,
}
impl NativeController {
    pub fn new(services: Arc<Services>, devices: Arc<dyn Devices>) -> Arc<Self> {
        let controller = Controller::new(
            services.settings.clone(),
            services.sink.clone(),
            devices,
            services.clone(),
        );
        let auth = Auth::new(
            services.settings.clone(),
            controller.clone(),
            Arc::new(HttpCatalog),
        );
        Arc::new(Self {
            services,
            controller,
            auth,
        })
    }
    pub fn register(self: &Arc<Self>, router: Router) -> Router {
        let mut router = self.auth.register(self.controller.register(router));
        router = crate::runtime::register(router, &self.services.runtime);
        for (route, flag) in [
            ("/run/typing_message_box", true),
            ("/run/stop_typing_message_box", false),
        ] {
            let services = self.services.clone();
            router = router.handle(route, move |_| {
                let services = services.clone();
                async move {
                    if services.settings.get_bool("SEND_MESSAGE_TO_VRC") == Some(true) {
                        if let Err(e) = services.sinks.typing(flag) {
                            return (500, json!(e));
                        }
                    }
                    (200, json!(flag))
                }
            });
        }
        let services = self.services.clone();
        router = router.handle("/run/send_text_overlay", move |data| {
            let services = services.clone();
            async move {
                tokio::task::spawn_blocking(move || services.preview(data.unwrap_or(Value::Null)))
                    .await
                    .unwrap_or((500, json!("Internal error")))
            }
        });
        for (route, kind) in [
            ("/run/download_ctranslate2_weight", "ctranslate2"),
            ("/run/download_whisper_weight", "whisper"),
        ] {
            let services = self.services.clone();
            let controller = self.controller.clone();
            router = router.handle(route, move |data| {
                let services = services.clone();
                let controller = controller.clone();
                async move {
                    let weight = data
                        .as_ref()
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| {
                            services
                                .settings
                                .get_str(if kind == "whisper" {
                                    "WHISPER_WEIGHT_TYPE"
                                } else {
                                    "CTRANSLATE2_WEIGHT_TYPE"
                                })
                                .unwrap_or_default()
                        });
                    if let Err(error) = services.models.model_path(kind, &weight) {
                        return (400, json!(error));
                    }
                    tokio::spawn(async move {
                        if services.download(kind, &weight).await.is_ok() {
                            let service = services.clone();
                            let _ =
                                tokio::task::spawn_blocking(move || service.refresh_models()).await;
                            controller.refresh_engines();
                        }
                    });
                    (200, json!(true))
                }
            });
        }
        router
    }
    pub fn snapshot(&self) -> Result<Value, String> {
        let mut values = serde_json::Map::new();
        for endpoint in crate::controller::contract()
            .iter()
            .map(|row| row.endpoint.as_str())
            .chain(crate::controller::NATIVE_ENDPOINTS.iter().copied())
            .filter(|endpoint| {
                endpoint.starts_with("/get/data/") && *endpoint != "/get/data/available_releases"
            })
        {
            let value = if let Some(value) = self.auth.getter(endpoint) {
                value
            } else {
                let (status, value) = self.controller.answer(endpoint, Value::Null);
                if status != 200 {
                    return Err(format!(
                        "Native initialization getter failed: {}",
                        endpoint
                    ));
                }
                value
            };
            values.insert(endpoint.to_owned(), value);
        }
        Ok(Value::Object(values))
    }
    pub async fn initialize(self: &Arc<Self>) -> Result<(), String> {
        let mut closed = self.services.closed.subscribe();
        if *closed.borrow() {
            return Err("Backend has stopped".into());
        }
        tokio::select! {
            result=self.initialize_inner()=>result,
            _=closed.wait_for(|closed|*closed)=>Err("Backend has stopped".into()),
        }
    }
    async fn initialize_inner(self: &Arc<Self>) -> Result<(), String> {
        self.services
            .emit(200, "/run/initialization_progress", json!(1));
        let connected = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .map_err(|e| e.to_string())?
            .get("https://www.google.com/generate_204")
            .send()
            .await
            .is_ok_and(|response| response.status().is_success());
        self.services
            .emit(200, "/run/connected_network", json!(connected));
        let mut status = self
            .services
            .settings
            .get("SELECTABLE_TRANSLATION_ENGINE_STATUS")
            .unwrap_or(json!({}));
        for name in ["Google", "Bing", "Papago"] {
            status[name] = json!(connected);
        }
        let _ = self
            .services
            .settings
            .set("SELECTABLE_TRANSLATION_ENGINE_STATUS", status);
        let mut status = self
            .services
            .settings
            .get("SELECTABLE_TRANSCRIPTION_ENGINE_STATUS")
            .unwrap_or(json!({}));
        status["Google"] = json!(connected);
        let _ = self
            .services
            .settings
            .set("SELECTABLE_TRANSCRIPTION_ENGINE_STATUS", status);
        let services = self.services.clone();
        tokio::task::spawn_blocking(move || services.initialize())
            .await
            .map_err(|e| e.to_string())??;
        self.services
            .emit(200, "/run/initialization_progress", json!(2));
        self.auth.initialize().await;
        self.controller.refresh_engines();
        self.services
            .emit(200, "/run/initialization_progress", json!(4));
        self.services
            .emit(200, "/run/initialization_complete", self.snapshot()?);
        if connected && std::env::var("VRCT_SKIP_MODEL_DOWNLOAD").as_deref() != Ok("1") {
            let this = self.clone();
            tokio::spawn(async move {
                for (kind, key) in [
                    ("ctranslate2", "CTRANSLATE2_WEIGHT_TYPE"),
                    ("whisper", "WHISPER_WEIGHT_TYPE"),
                ] {
                    let weight = this.services.settings.get_str(key).unwrap_or_default();
                    if !this.services.models.available_async(kind, &weight).await
                        && this.services.download(kind, &weight).await.is_err()
                    {
                        return;
                    }
                }
                let services = this.services.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    services.refresh_models();
                    let _ = services.load_translation();
                })
                .await;
                this.controller.refresh_engines();
                let ready = this
                    .services
                    .settings
                    .get("SELECTABLE_TRANSLATION_ENGINE_STATUS")
                    .unwrap_or_default()["CTranslate2"]
                    == true
                    && this
                        .services
                        .settings
                        .get("SELECTABLE_TRANSCRIPTION_ENGINE_STATUS")
                        .unwrap_or_default()["Whisper"]
                        == true;
                this.services
                    .emit(200, "/run/enable_ai_models", json!(ready));
            });
        }
        Ok(())
    }
}

/// Resolve bundled resources first; source resources are a debug-only fallback.
pub fn resources(executable: &Path) -> PathBuf {
    let bundled = executable
        .parent()
        .unwrap_or(Path::new("."))
        .join("resources");
    if bundled.is_dir() {
        return bundled;
    }
    #[cfg(debug_assertions)]
    {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../resources")
    }
    #[cfg(not(debug_assertions))]
    {
        bundled
    }
}
