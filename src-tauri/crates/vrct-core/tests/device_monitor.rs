use serde_json::json;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use vrct_core::{
    audio::devices::{NO_DEVICE, NO_HOST, WASAPI_HOST},
    device_monitor::{select, DeviceMonitor, Snapshot, Source},
    protocol::Response,
    router::ResponseSink,
    settings::{system::production_env, Devices, Settings},
};
#[derive(Clone)]
struct FakeDevices(Arc<Mutex<Snapshot>>, Arc<AtomicUsize>);
impl Devices for FakeDevices {
    fn mic_hosts(&self) -> Vec<String> {
        self.0.lock().unwrap().hosts.clone()
    }
    fn mic_device_names(&self, host: &str) -> Vec<String> {
        let s = self.0.lock().unwrap();
        if s.hosts.iter().any(|h| h == host) {
            s.mics.clone()
        } else {
            vec![]
        }
    }
    fn speaker_device_names(&self) -> Vec<String> {
        self.0.lock().unwrap().speakers.clone()
    }
    fn speaker_hosts(&self) -> Vec<String> { self.mic_hosts() }
    fn speaker_device_names_for_host(&self, _: &str) -> Vec<String> { self.speaker_device_names() }
    fn default_mic(&self) -> Option<(String, String)> {
        self.0
            .lock()
            .unwrap()
            .default_mic
            .clone()
            .map(|d| (WASAPI_HOST.into(), d))
    }
    fn default_speaker(&self) -> Option<String> {
        self.0.lock().unwrap().default_speaker.clone()
    }
}
impl Source for FakeDevices {
    fn snapshot(&self) -> Result<Snapshot, String> {
        self.1.fetch_add(1, Ordering::SeqCst);
        Ok(self.0.lock().unwrap().clone())
    }
}
#[derive(Default)]
struct Sink(Mutex<Vec<Response>>);
impl ResponseSink for Sink {
    fn emit(&self, response: Response) {
        self.0.lock().unwrap().push(response)
    }
}
fn snapshot(
    mics: &[&str],
    speakers: &[&str],
    default_mic: Option<&str>,
    default_speaker: Option<&str>,
) -> Snapshot {
    Snapshot {
        hosts: vec![if mics.is_empty() {
            NO_HOST
        } else {
            WASAPI_HOST
        }
        .into()],
        mics: if mics.is_empty() {
            vec![NO_DEVICE.into()]
        } else {
            mics.iter().map(|s| s.to_string()).collect()
        },
        speakers: if speakers.is_empty() {
            vec![NO_DEVICE.into()]
        } else {
            speakers.iter().map(|s| s.to_string()).collect()
        },
        default_mic: default_mic.map(str::to_string),
        default_speaker: default_speaker.map(str::to_string),
    }
}
struct Rig {
    settings: Arc<Settings>,
    devices: Arc<FakeDevices>,
    monitor: Arc<DeviceMonitor>,
    sink: Arc<Sink>,
    _path: PathBuf,
}
impl Rig {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../../target/device-monitor-tests/{}/{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let devices = Arc::new(FakeDevices(
            Arc::new(Mutex::new(snapshot(
                &["Mic A", "Mic B"],
                &["Speaker A [Loopback]", "Speaker B [Loopback]"],
                Some("Mic A"),
                Some("Speaker A [Loopback]"),
            ))),
            Arc::new(AtomicUsize::new(0)),
        ));
        let mut env = production_env("1.0.0-beta", &path);
        env.devices = devices.clone();
        let (settings, _) = Settings::open_with(env, Duration::from_secs(60));
        let settings = Arc::new(settings);
        let sink = Arc::new(Sink::default());
        let monitor = DeviceMonitor::with_source(settings.clone(), devices.clone(), sink.clone());
        Self {
            settings,
            devices,
            monitor,
            sink,
            _path: path,
        }
    }
}
impl Drop for Rig {
    fn drop(&mut self) {
        self.monitor.shutdown().unwrap();
        self.settings.flush().unwrap();
    }
}
#[test]
fn manual_selection_is_preserved_then_device_loss_falls_back_to_default() {
    let r = Rig::new();
    r.settings.set("AUTO_MIC_SELECT", json!(false)).unwrap();
    r.settings
        .set("SELECTED_MIC_DEVICE", json!("Mic B"))
        .unwrap();
    r.monitor.refresh().unwrap();
    assert_eq!(
        r.settings.get_str("SELECTED_MIC_DEVICE").as_deref(),
        Some("Mic B")
    );
    *r.devices.0.lock().unwrap() = snapshot(
        &["Mic A"],
        &["Speaker A [Loopback]"],
        Some("Mic A"),
        Some("Speaker A [Loopback]"),
    );
    r.monitor.refresh().unwrap();
    assert_eq!(
        r.settings.get_str("SELECTED_MIC_DEVICE").as_deref(),
        Some("Mic A")
    );
    assert!(r
        .sink
        .0
        .lock()
        .unwrap()
        .iter()
        .any(|e| e.endpoint == "/run/selected_mic_device" && e.result == "Mic A"));
}
#[test]
fn automatic_selection_follows_defaults_without_new_capture() {
    let r = Rig::new();
    r.settings.set("AUTO_MIC_SELECT", json!(true)).unwrap();
    r.settings.set("AUTO_SPEAKER_SELECT", json!(true)).unwrap();
    r.monitor.refresh().unwrap();
    {
        let mut s = r.devices.0.lock().unwrap();
        s.default_mic = Some("Mic B".into());
        s.default_speaker = Some("Speaker B [Loopback]".into());
    }
    r.monitor.refresh().unwrap();
    assert_eq!(
        r.settings.get_str("SELECTED_MIC_DEVICE").as_deref(),
        Some("Mic B")
    );
    assert_eq!(
        r.settings.get_str("SELECTED_SPEAKER_DEVICE").as_deref(),
        Some("Speaker B [Loopback]")
    );
    assert_eq!(
        r.settings.get_bool("ENABLE_TRANSCRIPTION_SEND"),
        Some(false)
    );
}
#[test]
fn missing_os_default_uses_first_real_and_zero_devices_use_placeholders() {
    let r = Rig::new();
    *r.devices.0.lock().unwrap() = snapshot(&["Mic B"], &["Speaker B [Loopback]"], None, None);
    r.monitor.refresh().unwrap();
    assert_eq!(
        r.settings.get_str("SELECTED_MIC_DEVICE").as_deref(),
        Some("Mic B")
    );
    *r.devices.0.lock().unwrap() = snapshot(&[], &[], None, None);
    r.monitor.refresh().unwrap();
    assert_eq!(
        r.settings.get_str("SELECTED_MIC_HOST").as_deref(),
        Some(NO_HOST)
    );
    assert_eq!(
        r.settings.get_str("SELECTED_MIC_DEVICE").as_deref(),
        Some(NO_DEVICE)
    );
    assert_eq!(
        r.settings.get_str("SELECTED_SPEAKER_DEVICE").as_deref(),
        Some(NO_DEVICE)
    );
}
#[test]
fn unchanged_lists_emit_no_duplicate_events_and_mic_list_is_array() {
    let r = Rig::new();
    r.monitor.refresh().unwrap();
    let count = r.sink.0.lock().unwrap().len();
    r.monitor.refresh().unwrap();
    assert_eq!(r.sink.0.lock().unwrap().len(), count);
    let events = r.sink.0.lock().unwrap();
    let event = events
        .iter()
        .find(|e| e.endpoint == "/run/selectable_mic_device_list")
        .unwrap();
    assert_eq!(event.result, json!(["Mic A", "Mic B"]));
}
#[test]
fn unique_legacy_prefix_maps_exactly_but_ambiguous_prefix_uses_default() {
    let names = vec![
        "Microphone (Long name A)".into(),
        "Microphone (Long name B)".into(),
    ];
    assert_eq!(
        select(&names, Some(&names[1]), "Microphone (Long name A", false),
        names[0]
    );
    assert_eq!(
        select(&names, Some(&names[1]), "Microphone (Long", false),
        names[1]
    );
    assert_eq!(select(&names, None, &names[0], false), names[0]);
    assert_eq!(select(&[], None, "stale", false), NO_DEVICE);
    assert_eq!(select(&names, None, &names[1], true), names[0]);
}
#[test]
fn worker_start_idempotent_and_stop_interrupts_wait_without_leaking() {
    let r = Rig::new();
    r.monitor.start().unwrap();
    r.monitor.start().unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while r.devices.1.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let start = Instant::now();
    r.monitor.shutdown().unwrap();
    assert!(start.elapsed() < Duration::from_millis(300));
    let count = r.devices.1.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(r.devices.1.load(Ordering::SeqCst), count);
    r.monitor.start().unwrap();
    r.monitor.shutdown().unwrap();
}

#[test]
fn dropping_last_owner_stops_worker_without_self_join_or_retained_arc() {
    let r = Rig::new();
    let monitor = DeviceMonitor::with_source(r.settings.clone(), r.devices.clone(), r.sink.clone());
    let weak = Arc::downgrade(&monitor);
    monitor.start().unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while r.devices.1.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(monitor);
    assert!(weak.upgrade().is_none());
    let count = r.devices.1.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(r.devices.1.load(Ordering::SeqCst), count);
}

#[test]
fn enumeration_error_keeps_previous_selection_and_emits_no_list() {
    struct Failed;
    impl Source for Failed {
        fn snapshot(&self) -> Result<Snapshot, String> {
            Err("temporary COM failure".into())
        }
    }
    let r = Rig::new();
    let monitor = DeviceMonitor::with_source(r.settings.clone(), Arc::new(Failed), r.sink.clone());
    let before = r.settings.get_str("SELECTED_MIC_DEVICE");
    assert!(monitor.refresh().is_err());
    assert_eq!(r.settings.get_str("SELECTED_MIC_DEVICE"), before);
    assert!(r.sink.0.lock().unwrap().is_empty());
}

#[test]
fn manual_asio_host_survives_hotplug_refresh_for_both_sessions() {
    let r = Rig::new();
    r.devices.0.lock().unwrap().hosts.push("ASIO".into());
    for kind in ["MIC", "SPEAKER"] {
        r.settings.set(&format!("AUTO_{kind}_SELECT"), json!(false)).unwrap();
        r.settings.set(&format!("SELECTED_{kind}_HOST"), json!("ASIO")).unwrap();
    }
    r.monitor.refresh().unwrap();
    assert_eq!(r.settings.get_str("SELECTED_MIC_HOST").as_deref(), Some("ASIO"));
    assert_eq!(r.settings.get_str("SELECTED_SPEAKER_HOST").as_deref(), Some("ASIO"));
    r.settings.set("AUTO_MIC_SELECT", json!(true)).unwrap();
    r.settings.set("AUTO_SPEAKER_SELECT", json!(true)).unwrap();
    r.monitor.refresh().unwrap();
    assert_eq!(r.settings.get_str("SELECTED_MIC_HOST").as_deref(), Some(WASAPI_HOST));
    assert_eq!(r.settings.get_str("SELECTED_SPEAKER_HOST").as_deref(), Some(WASAPI_HOST));
}
