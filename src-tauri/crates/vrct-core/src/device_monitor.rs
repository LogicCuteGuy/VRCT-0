//! Device-list monitoring without opening any audio stream. A single snapshot
//! supplies lists, defaults and selection decisions for each refresh.
use crate::{
    audio::devices::{DeviceList, NO_DEVICE, NO_HOST, WASAPI_HOST},
    protocol::Response,
    router::ResponseSink,
    settings::{Devices, Settings},
};
use serde_json::{json, Value};
use std::{
    sync::{
        mpsc::{self, SyncSender},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub hosts: Vec<String>,
    pub mics: Vec<String>,
    pub speakers: Vec<String>,
    pub default_mic: Option<String>,
    pub default_speaker: Option<String>,
}
impl From<DeviceList> for Snapshot {
    fn from(list: DeviceList) -> Self {
        Self {
            hosts: list.hosts().into_iter().map(str::to_owned).collect(),
            mics: list.mic_names().into_iter().map(str::to_owned).collect(),
            speakers: list
                .speaker_names()
                .into_iter()
                .map(str::to_owned)
                .collect(),
            default_mic: list.default_mic,
            default_speaker: list.default_speaker,
        }
    }
}
pub trait Source: Send + Sync {
    fn snapshot(&self) -> Result<Snapshot, String>;
    fn snapshot_for_hosts(&self, _mic_host: &str, _speaker_host: &str) -> Result<Snapshot, String> {
        self.snapshot()
    }
}
struct DeviceSource(Arc<dyn Devices>);
impl Source for DeviceSource {
    fn snapshot(&self) -> Result<Snapshot, String> {
        let hosts = self.0.mic_hosts();
        let mics = hosts
            .iter()
            .flat_map(|h| self.0.mic_device_names(h))
            .collect();
        Ok(Snapshot {
            hosts,
            mics,
            speakers: self.0.speaker_device_names(),
            default_mic: self.0.default_mic().map(|(_, name)| name),
            default_speaker: self.0.default_speaker(),
        })
    }
}
struct SystemSource;
impl Source for SystemSource {
    fn snapshot_for_hosts(&self, mic_host: &str, speaker_host: &str) -> Result<Snapshot, String> {
        #[cfg(windows)]
        {
            let hosts = crate::audio::wasapi::host_names();
            let mic_host = if hosts.iter().any(|h| h == mic_host) { mic_host } else { WASAPI_HOST };
            let speaker_host = if hosts.iter().any(|h| h == speaker_host) { speaker_host } else { WASAPI_HOST };
            let mic = crate::audio::wasapi::list_devices_for_host(mic_host)?;
            let speaker = crate::audio::wasapi::list_devices_for_host(speaker_host)?;
            Ok(Snapshot {
                hosts,
                mics: mic.mic_names().into_iter().map(str::to_owned).collect(),
                speakers: speaker.speaker_names().into_iter().map(str::to_owned).collect(),
                default_mic: mic.default_mic,
                default_speaker: speaker.default_speaker,
            })
        }
        #[cfg(not(windows))]
        { let _ = (mic_host, speaker_host); self.snapshot() }
    }
    fn snapshot(&self) -> Result<Snapshot, String> {
        #[cfg(windows)]
        {
            crate::audio::wasapi::list_devices().map(Into::into)
        }
        #[cfg(not(windows))]
        {
            Ok(DeviceList::default().into())
        }
    }
}
enum Message {
    Wake,
    Stop,
}
struct Worker {
    sender: SyncSender<Message>,
    thread: JoinHandle<()>,
}
#[derive(Default)]
struct WakeSignal(Mutex<Option<SyncSender<Message>>>);
impl WakeSignal {
    fn wake(&self) {
        if let Some(sender) = self.0.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
            let _ = sender.try_send(Message::Wake);
        }
    }
}
struct Core {
    settings: Arc<Settings>,
    source: Arc<dyn Source>,
    sink: Arc<dyn ResponseSink>,
    previous: Mutex<Option<Snapshot>>,
    refresh_gate: Mutex<()>,
}
pub struct DeviceMonitor {
    core: Arc<Core>,
    lifecycle: Mutex<()>,
    worker: Mutex<Option<Worker>>,
    wake: Arc<WakeSignal>,
}
impl DeviceMonitor {
    pub fn new(
        settings: Arc<Settings>,
        devices: Arc<dyn Devices>,
        sink: Arc<dyn ResponseSink>,
    ) -> Arc<Self> {
        Self::with_source(settings, Arc::new(DeviceSource(devices)), sink)
    }
    pub fn native(settings: Arc<Settings>, sink: Arc<dyn ResponseSink>) -> Arc<Self> {
        Self::with_source(settings, Arc::new(SystemSource), sink)
    }
    pub fn with_source(
        settings: Arc<Settings>,
        source: Arc<dyn Source>,
        sink: Arc<dyn ResponseSink>,
    ) -> Arc<Self> {
        let monitor = Arc::new(Self {
            core: Arc::new(Core {
                settings: settings.clone(),
                source,
                sink,
                previous: Mutex::new(None),
                refresh_gate: Mutex::new(()),
            }),
            lifecycle: Mutex::new(()),
            worker: Mutex::new(None),
            wake: Arc::new(WakeSignal::default()),
        });
        // A callback must never become the monitor's final owner and join its
        // worker while Settings still holds the listener lock. Only the inert
        // wake signal is retained here, and settings retains it weakly.
        let weak = Arc::downgrade(&monitor.wake);
        settings.subscribe(move |name, _| {
            if matches!(name, "AUTO_MIC_SELECT" | "AUTO_SPEAKER_SELECT" | "SELECTED_MIC_HOST" | "SELECTED_SPEAKER_HOST") {
                if let Some(signal) = weak.upgrade() {
                    signal.wake();
                }
            }
        });
        monitor
    }
    pub fn start(self: &Arc<Self>) -> Result<(), String> {
        let _guard = self.lifecycle.lock().unwrap_or_else(|p| p.into_inner());
        let mut worker = self.worker.lock().unwrap_or_else(|p| p.into_inner());
        if worker.is_some() {
            return Ok(());
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        let core = self.core.clone();
        let thread = thread::Builder::new()
            .name("vrct-device-monitor".into())
            .spawn(move || loop {
                let _ = core.refresh();
                match receiver.recv_timeout(Duration::from_secs(2)) {
                    Ok(Message::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    _ => {}
                }
                let mut stopped = false;
                for message in receiver.try_iter() {
                    if matches!(message, Message::Stop) {
                        stopped = true;
                        break;
                    }
                }
                if stopped {
                    break;
                }
            })
            .map_err(|e| e.to_string())?;
        *self.wake.0.lock().unwrap_or_else(|p| p.into_inner()) = Some(sender.clone());
        *worker = Some(Worker { sender, thread });
        Ok(())
    }
    pub fn wake(&self) {
        self.wake.wake();
    }
    pub fn stop(&self) -> Result<(), String> {
        let _guard = self.lifecycle.lock().unwrap_or_else(|p| p.into_inner());
        let worker = self.worker.lock().unwrap_or_else(|p| p.into_inner()).take();
        self.wake.0.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(worker) = worker {
            let _ = worker.sender.send(Message::Stop);
            worker
                .thread
                .join()
                .map_err(|_| "Device monitor worker panicked".to_owned())?;
        }
        Ok(())
    }
    pub fn shutdown(&self) -> Result<(), String> {
        self.stop()
    }
    pub fn refresh(&self) -> Result<(), String> {
        self.core.refresh()
    }
}
impl Core {
    fn refresh(&self) -> Result<(), String> {
        let _guard = self.refresh_gate.lock().unwrap_or_else(|p| p.into_inner());
        let old_host = self.settings.get_str("SELECTED_MIC_HOST").unwrap_or_default();
        let old_speaker_host = self.settings.get_str("SELECTED_SPEAKER_HOST").unwrap_or_else(|| WASAPI_HOST.to_owned());
        let automatic_mic = self.settings.get_bool("AUTO_MIC_SELECT") == Some(true);
        let automatic_speaker = self.settings.get_bool("AUTO_SPEAKER_SELECT") == Some(true);
        let mic_host = if automatic_mic { WASAPI_HOST } else { old_host.as_str() };
        let speaker_host = if automatic_speaker { WASAPI_HOST } else { old_speaker_host.as_str() };
        let snapshot = self.source.snapshot_for_hosts(mic_host, speaker_host)?;
        let speaker_host = if snapshot.hosts.iter().any(|h| h == speaker_host) { speaker_host } else { WASAPI_HOST };
        let previous = self
            .previous
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let old_mic = self
            .settings
            .get_str("SELECTED_MIC_DEVICE")
            .unwrap_or_default();
        let old_speaker = self
            .settings
            .get_str("SELECTED_SPEAKER_DEVICE")
            .unwrap_or_default();
        let mic = select(
            &snapshot.mics,
            snapshot.default_mic.as_deref(),
            &old_mic,
            self.settings.get_bool("AUTO_MIC_SELECT") == Some(true),
        );
        let speaker = select(
            &snapshot.speakers,
            snapshot.default_speaker.as_deref(),
            &old_speaker,
            self.settings.get_bool("AUTO_SPEAKER_SELECT") == Some(true),
        );
        let host = if !automatic_mic && snapshot.hosts.contains(&old_host) && old_host != NO_HOST {
            old_host.clone()
        } else if mic == NO_DEVICE {
            NO_HOST.to_owned()
        } else {
            snapshot
                .hosts
                .iter()
                .find(|h| h.as_str() == WASAPI_HOST)
                .or_else(|| snapshot.hosts.iter().find(|h| h.as_str() != NO_HOST))
                .cloned()
                .unwrap_or_else(|| NO_HOST.to_owned())
        };
        self.assign(
            "SELECTED_MIC_HOST",
            "/run/selected_mic_host",
            &old_host,
            &host,
        )?;
        self.assign(
            "SELECTED_MIC_DEVICE",
            "/run/selected_mic_device",
            &old_mic,
            &mic,
        )?;
        self.assign(
            "SELECTED_SPEAKER_HOST", "/run/selected_speaker_host", &old_speaker_host, speaker_host,
        )?;
        self.assign(
            "SELECTED_SPEAKER_DEVICE",
            "/run/selected_speaker_device",
            &old_speaker,
            &speaker,
        )?;
        if previous.as_ref().is_none_or(|p| p.hosts != snapshot.hosts) {
            self.emit("/run/selectable_mic_host_list", json!(snapshot.hosts));
            self.emit("/run/selectable_speaker_host_list", json!(snapshot.hosts));
        }
        if previous.as_ref().is_none_or(|p| p.mics != snapshot.mics) || old_host != host {
            self.emit("/run/selectable_mic_device_list", json!(snapshot.mics));
        }
        if previous
            .as_ref()
            .is_none_or(|p| p.speakers != snapshot.speakers) || old_speaker_host != speaker_host
        {
            self.emit(
                "/run/selectable_speaker_device_list",
                json!(snapshot.speakers),
            );
        }
        *self.previous.lock().unwrap_or_else(|p| p.into_inner()) = Some(snapshot);
        Ok(())
    }
    fn assign(&self, setting: &str, endpoint: &str, old: &str, new: &str) -> Result<(), String> {
        if old != new {
            self.settings
                .set(setting, json!(new))
                .map_err(|e| e.to_string())?;
            self.emit(endpoint, json!(new));
        }
        Ok(())
    }
    fn emit(&self, endpoint: &str, data: Value) {
        self.sink.emit(Response::new(200, endpoint, data));
    }
}
impl Drop for DeviceMonitor {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// Exact selections win, then an unambiguous legacy MME prefix. AUTO follows
/// the default; lost devices fall back even with AUTO off. No real device means
/// the same NoDevice placeholder the old controller used.
pub fn select(names: &[String], default: Option<&str>, saved: &str, automatic: bool) -> String {
    let real: Vec<&str> = names
        .iter()
        .map(String::as_str)
        .filter(|n| *n != NO_DEVICE)
        .collect();
    let default = default.filter(|name| real.contains(name));
    if automatic {
        return default
            .or_else(|| real.first().copied())
            .unwrap_or(NO_DEVICE)
            .to_owned();
    }
    if real.contains(&saved) {
        return saved.to_owned();
    }
    if !saved.is_empty() && saved != NO_DEVICE {
        let candidates: Vec<_> = real.iter().filter(|name| name.starts_with(saved)).collect();
        if candidates.len() == 1 {
            return (*candidates[0]).to_owned();
        }
    }
    default
        .or_else(|| real.first().copied())
        .unwrap_or(NO_DEVICE)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    struct EmptyDevices;
    impl Devices for EmptyDevices {
        fn mic_hosts(&self) -> Vec<String> {
            vec![NO_HOST.into()]
        }
        fn mic_device_names(&self, _: &str) -> Vec<String> {
            vec![NO_DEVICE.into()]
        }
        fn speaker_device_names(&self) -> Vec<String> {
            vec![NO_DEVICE.into()]
        }
        fn default_mic(&self) -> Option<(String, String)> {
            None
        }
        fn default_speaker(&self) -> Option<String> {
            None
        }
    }
    impl Source for EmptyDevices {
        fn snapshot(&self) -> Result<Snapshot, String> {
            Ok(DeviceList::default().into())
        }
    }
    struct NullSink;
    impl ResponseSink for NullSink {
        fn emit(&self, _: Response) {}
    }

    #[test]
    fn settings_callback_retains_only_wake_signal_while_final_owner_is_dropped() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../../target/device-monitor-tests/{}/callback-drop",
            std::process::id()
        ));
        let mut env = crate::settings::system::production_env("1.0.0-beta", &path);
        env.devices = Arc::new(EmptyDevices);
        let (settings, _) = Settings::open_with(env, Duration::from_secs(60));
        let settings = Arc::new(settings);
        let monitor = DeviceMonitor::with_source(
            settings.clone(),
            Arc::new(EmptyDevices),
            Arc::new(NullSink),
        );
        monitor.start().unwrap();
        let signal = monitor.wake.clone();
        let wake_guard = signal.0.lock().unwrap();
        let setter_settings = settings.clone();
        let setter =
            thread::spawn(move || setter_settings.set("AUTO_MIC_SELECT", json!(true)).unwrap());
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        // The paused callback has upgraded its signal, while the monitor still
        // has exactly one owner. Upgrading the monitor instead would make this
        // two and allow its final Drop to run under Settings' listener lock.
        while Arc::strong_count(&signal) != 3 {
            assert!(
                std::time::Instant::now() < deadline,
                "callback did not enter wake signal"
            );
            thread::yield_now();
        }
        assert_eq!(Arc::strong_count(&monitor), 1);
        let weak = Arc::downgrade(&monitor);
        let (finished, receiver) = mpsc::channel();
        let dropper = thread::spawn(move || {
            drop(monitor);
            finished.send(()).unwrap();
        });
        drop(wake_guard);
        receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("final owner Drop must stop worker");
        setter.join().unwrap();
        dropper.join().unwrap();
        assert!(weak.upgrade().is_none());
        settings.flush().unwrap();
    }
}
