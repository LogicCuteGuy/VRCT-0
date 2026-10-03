//! Aptabase telemetry with VRCT's current two-event contract: daily app_started
//! and daily error_code. No text, audio, persistent identity or feature events.
//! Native transport failures are swallowed and never retried.
use crate::transcription::native::Config;
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{runtime::Handle, task::JoinHandle};
pub const ENDPOINT: &str = "https://us.aptabase.com/api/v0/events";
pub const STABLE_APP_KEY: &str = "A-US-3414271507";
pub const BETA_APP_KEY: &str = "A-US-6044063021";

pub trait Clock: Send + Sync {
    fn date(&self) -> String;
    fn timestamp(&self) -> String;
    fn epoch_seconds(&self) -> u64;
}
pub struct LocalClock;
impl Clock for LocalClock {
    fn date(&self) -> String {
        chrono::Local::now().format("%Y-%m-%d").to_string()
    }
    // Keep the old SDK's local-time serialization (naive timestamp plus Z).
    fn timestamp(&self) -> String {
        chrono::Local::now()
            .format("%Y-%m-%dT%H:%M:%S%.6fZ")
            .to_string()
    }
    fn epoch_seconds(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }
}
pub trait Transport: Send + Sync {
    fn send(&self, app_key: &str, body: Value) -> BoxFuture<'static, Result<(), String>>;
}
pub struct HttpTransport;
impl Transport for HttpTransport {
    fn send(&self, app_key: &str, body: Value) -> BoxFuture<'static, Result<(), String>> {
        let key = app_key.to_owned();
        Box::pin(async move {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .map_err(|e| e.to_string())?;
            client
                .post(ENDPOINT)
                .header("App-Key", key)
                .json(&body)
                .send()
                .await
                .map_err(|e| e.to_string())?
                .error_for_status()
                .map_err(|e| e.to_string())?;
            Ok(())
        })
    }
}
struct State {
    started: bool,
    data: Value,
    session: String,
    last_touch: u64,
    tasks: Vec<JoinHandle<()>>,
}
pub struct Telemetry {
    config: Arc<dyn Config>,
    path: PathBuf,
    version: String,
    key: &'static str,
    runtime: Handle,
    clock: Arc<dyn Clock>,
    transport: Arc<dyn Transport>,
    state: Mutex<State>,
}
impl Telemetry {
    pub fn new(
        config: Arc<dyn Config>,
        local: impl AsRef<Path>,
        version: &str,
        channel: &str,
        runtime: Handle,
    ) -> Arc<Self> {
        Self::with_transport(
            config,
            local,
            version,
            channel,
            runtime,
            Arc::new(LocalClock),
            Arc::new(HttpTransport),
        )
    }
    pub fn with_transport(
        config: Arc<dyn Config>,
        local: impl AsRef<Path>,
        version: &str,
        channel: &str,
        runtime: Handle,
        clock: Arc<dyn Clock>,
        transport: Arc<dyn Transport>,
    ) -> Arc<Self> {
        Arc::new(Self {
            config,
            path: local.as_ref().join("telemetry_state.json"),
            version: version.to_owned(),
            key: if channel == "stable" {
                STABLE_APP_KEY
            } else {
                BETA_APP_KEY
            },
            runtime,
            clock,
            transport,
            state: Mutex::new(State {
                started: false,
                data: json!({"last_app_started_date":null,"errors_sent":{}}),
                session: String::new(),
                last_touch: 0,
                tasks: Vec::new(),
            }),
        })
    }
    pub fn is_enabled(&self) -> bool {
        self.config.get("ENABLE_TELEMETRY") == Some(Value::Bool(true))
    }
    pub fn start(&self) {
        if !self.is_enabled() {
            self.shutdown();
            return;
        }
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.started {
            return;
        }
        state.started = true;
        state.data = load(&self.path);
        let today = self.clock.date();
        if state
            .data
            .get("last_app_started_date")
            .and_then(Value::as_str)
            != Some(&today)
        {
            state.data["last_app_started_date"] = json!(today);
            let errors = state.data["errors_sent"]
                .get(&today)
                .cloned()
                .unwrap_or_else(|| json!([]));
            state.data["errors_sent"] = json!({today:errors});
            save(&self.path, &state.data);
            self.schedule(&mut state, "app_started", json!({}));
        }
    }
    pub fn track_error(&self, error_code: &str) {
        if !self.is_enabled() || error_code.is_empty() {
            return;
        }
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if !state.started {
            return;
        }
        let today = self.clock.date();
        let mut errors = state.data["errors_sent"]
            .get(&today)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if errors.iter().any(|v| v.as_str() == Some(error_code)) {
            return;
        }
        errors.push(json!(error_code));
        state.data["errors_sent"] = json!({today:errors});
        save(&self.path, &state.data);
        self.schedule(&mut state, "error", json!({"error_code":error_code}));
    }
    fn schedule(&self, state: &mut State, event: &str, properties: Value) {
        state.tasks.retain(|task| !task.is_finished());
        // Bound native in-flight work; telemetry must never consume unlimited
        // memory when thousands of distinct errors arrive at once.
        if state.tasks.len() >= 25 {
            return;
        }
        let now = self.clock.epoch_seconds();
        if state.session.is_empty() || now.saturating_sub(state.last_touch) > 3600 {
            let mut random = [0u8; 8];
            if getrandom::fill(&mut random).is_err() {
                return;
            }
            state.session = format!(
                "{}",
                now as u128 * 100_000_000 + (u64::from_le_bytes(random) % 100_000_000) as u128
            );
        }
        state.last_touch = now;
        let body = json!([{"timestamp":self.clock.timestamp(),"sessionId":state.session,"eventName":event,"systemProps":system_properties(&self.version),"props":properties}]);
        let config = self.config.clone();
        let transport = self.transport.clone();
        let key = self.key;
        state.tasks.push(self.runtime.spawn(async move {
            if config.get("ENABLE_TELEMETRY") == Some(Value::Bool(true)) {
                let _ = transport.send(key, body).await;
            }
        }));
    }
    /// Cancel in-flight work when disabled/closed. There is deliberately no
    /// app_closed event: it was removed by the existing Python implementation.
    pub fn shutdown(&self) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        for task in state.tasks.drain(..) {
            task.abort();
        }
        state.started = false;
        state.session.clear();
    }
    pub fn state(&self) -> Value {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .data
            .clone()
    }
}
impl Drop for Telemetry {
    fn drop(&mut self) {
        self.shutdown();
    }
}
fn load(path: &Path) -> Value {
    let value = std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    json!({"last_app_started_date":value.get("last_app_started_date").filter(|v|v.is_string()).cloned().unwrap_or(Value::Null),"errors_sent":value.get("errors_sent").filter(|v|v.is_object()).cloned().unwrap_or_else(||json!({}))})
}
fn save(path: &Path, data: &Value) {
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    if let Ok(bytes) = serde_json::to_vec_pretty(data) {
        let _ = std::fs::write(path, bytes);
    }
}
fn system_properties(version: &str) -> Value {
    let os = if cfg!(windows) {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "Darwin"
    } else {
        "Linux"
    };
    let model = if cfg!(all(windows, target_arch = "x86_64")) {
        "AMD64"
    } else {
        std::env::consts::ARCH
    };
    json!({"locale":"en-US","osName":os,"osVersion":os_version(),"deviceModel":model,"isDebug":false,"appVersion":version,"sdkVersion":concat!("vrct-rust@",env!("CARGO_PKG_VERSION"))})
}
fn os_version() -> String {
    #[cfg(windows)]
    {
        #[repr(C)]
        struct Version {
            size: u32,
            major: u32,
            minor: u32,
            build: u32,
            platform: u32,
            service: [u16; 128],
        }
        #[link(name = "ntdll")]
        unsafe extern "system" {
            fn RtlGetVersion(version: *mut Version) -> i32;
        }
        let mut version = Version {
            size: std::mem::size_of::<Version>() as u32,
            major: 0,
            minor: 0,
            build: 0,
            platform: 0,
            service: [0; 128],
        };
        // Same OS release label Python platform.release reports on Windows.
        if unsafe { RtlGetVersion(&mut version) } == 0 {
            return if version.major == 10 && version.build >= 22000 {
                "11".into()
            } else {
                version.major.to_string()
            };
        }
    }
    String::new()
}
