use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};
use vrct_core::{
    telemetry::{Clock, Telemetry, Transport, BETA_APP_KEY, ENDPOINT, STABLE_APP_KEY},
    transcription::native::Config,
};
#[derive(Default)]
struct Cfg(Mutex<HashMap<String, Value>>);
impl Config for Cfg {
    fn get(&self, name: &str) -> Option<Value> {
        self.0.lock().unwrap().get(name).cloned()
    }
}
struct Time(Mutex<String>);
impl Clock for Time {
    fn date(&self) -> String {
        self.0.lock().unwrap().clone()
    }
    fn timestamp(&self) -> String {
        format!("{}T12:34:56.000000Z", self.date())
    }
    fn epoch_seconds(&self) -> u64 {
        1_791_022_896
    }
}
#[derive(Default)]
struct Http {
    requests: Mutex<Vec<(String, Value)>>,
    fail: bool,
}
impl Transport for Http {
    fn send(&self, key: &str, body: Value) -> BoxFuture<'static, Result<(), String>> {
        self.requests.lock().unwrap().push((key.to_owned(), body));
        let fail = self.fail;
        Box::pin(async move {
            if fail {
                Err("offline".into())
            } else {
                Ok(())
            }
        })
    }
}
fn path() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../target/telemetry-tests/{}/{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ))
}
fn cfg(enabled: bool) -> Arc<Cfg> {
    let cfg = Arc::new(Cfg::default());
    cfg.0
        .lock()
        .unwrap()
        .insert("ENABLE_TELEMETRY".into(), json!(enabled));
    cfg
}
fn telemetry(
    cfg: Arc<Cfg>,
    path: &std::path::Path,
    http: Arc<Http>,
    clock: Arc<Time>,
    channel: &str,
) -> Arc<Telemetry> {
    Telemetry::with_transport(
        cfg,
        path,
        "1.0.0-beta",
        channel,
        tokio::runtime::Handle::current(),
        clock,
        http,
    )
}
async fn settle() {
    for _ in 0..12 {
        tokio::task::yield_now().await;
    }
}
#[tokio::test]
async fn disabled_start_error_and_shutdown_make_no_requests_or_statefile() {
    let cfg = cfg(false);
    let path = path();
    let http = Arc::new(Http::default());
    let t = telemetry(
        cfg,
        &path,
        http.clone(),
        Arc::new(Time(Mutex::new("2026-10-03".into()))),
        "beta",
    );
    t.start();
    t.track_error("MIC_ERROR");
    t.shutdown();
    settle().await;
    assert!(http.requests.lock().unwrap().is_empty());
    assert!(!path.join("telemetry_state.json").exists());
}
#[tokio::test]
async fn enabling_after_disabled_start_sends_daily_start_once() {
    let cfg = cfg(false);
    let path = path();
    let http = Arc::new(Http::default());
    let t = telemetry(
        cfg.clone(),
        &path,
        http.clone(),
        Arc::new(Time(Mutex::new("2026-10-03".into()))),
        "beta",
    );
    t.start();
    settle().await;
    assert!(http.requests.lock().unwrap().is_empty());
    cfg.0
        .lock()
        .unwrap()
        .insert("ENABLE_TELEMETRY".into(), json!(true));
    t.start();
    settle().await;
    assert_eq!(http.requests.lock().unwrap().len(), 1);
    assert_eq!(
        http.requests.lock().unwrap()[0].1[0]["eventName"],
        "app_started"
    );
    cfg.0
        .lock()
        .unwrap()
        .insert("ENABLE_TELEMETRY".into(), json!(false));
    t.start();
    cfg.0
        .lock()
        .unwrap()
        .insert("ENABLE_TELEMETRY".into(), json!(true));
    t.start();
    t.track_error("MIC_ERROR");
    settle().await;
    assert_eq!(http.requests.lock().unwrap().len(), 2);
    t.shutdown();
}
#[tokio::test]
async fn daily_start_error_dedup_survives_reopen_and_next_day_resets() {
    let cfg = cfg(true);
    let path = path();
    let http = Arc::new(Http::default());
    let clock = Arc::new(Time(Mutex::new("2026-10-03".into())));
    let t = telemetry(cfg.clone(), &path, http.clone(), clock.clone(), "beta");
    t.start();
    t.start();
    t.track_error("MIC_ERROR");
    t.track_error("MIC_ERROR");
    settle().await;
    assert_eq!(http.requests.lock().unwrap().len(), 2);
    t.shutdown();
    let next = telemetry(cfg, &path, http.clone(), clock.clone(), "beta");
    next.start();
    next.track_error("MIC_ERROR");
    settle().await;
    assert_eq!(http.requests.lock().unwrap().len(), 2);
    next.shutdown();
    *clock.0.lock().unwrap() = "2026-10-04".into();
    next.start();
    next.track_error("MIC_ERROR");
    settle().await;
    assert_eq!(http.requests.lock().unwrap().len(), 4);
    assert_eq!(
        next.state()["errors_sent"],
        json!({"2026-10-04":["MIC_ERROR"]})
    );
    next.shutdown();
}
#[tokio::test]
async fn exact_protocol_keys_and_fixed_properties_without_identity_or_text() {
    let cfg = cfg(true);
    let http = Arc::new(Http::default());
    let t = telemetry(
        cfg,
        &path(),
        http.clone(),
        Arc::new(Time(Mutex::new("2026-10-03".into()))),
        "stable",
    );
    t.start();
    t.track_error("MIC_ERROR");
    settle().await;
    let requests = http.requests.lock().unwrap();
    assert_eq!(ENDPOINT, "https://us.aptabase.com/api/v0/events");
    for (key, body) in requests.iter() {
        assert_eq!(key, STABLE_APP_KEY);
        let event = &body[0];
        assert_eq!(event.as_object().unwrap().len(), 5);
        assert!(event["sessionId"]
            .as_str()
            .unwrap()
            .chars()
            .all(|c| c.is_ascii_digit()));
        assert_eq!(event["systemProps"]["appVersion"], "1.0.0-beta");
        assert_eq!(event["systemProps"]["locale"], "en-US");
        assert_eq!(event["systemProps"]["isDebug"], false);
        assert!(event.get("userId").is_none());
        assert!(event.get("message").is_none());
    }
    assert_eq!(requests[0].1[0]["props"], json!({}));
    assert_eq!(requests[1].1[0]["props"], json!({"error_code":"MIC_ERROR"}));
    drop(requests);
    t.shutdown();
}
#[tokio::test]
async fn queued_send_checks_disable_and_shutdown_never_adds_close_event() {
    let cfg = cfg(true);
    let http = Arc::new(Http::default());
    let t = telemetry(
        cfg.clone(),
        &path(),
        http.clone(),
        Arc::new(Time(Mutex::new("2026-10-03".into()))),
        "beta",
    );
    t.start();
    cfg.0
        .lock()
        .unwrap()
        .insert("ENABLE_TELEMETRY".into(), json!(false));
    settle().await;
    assert!(http.requests.lock().unwrap().is_empty());
    t.track_error("MIC_ERROR");
    t.shutdown();
    settle().await;
    assert!(http.requests.lock().unwrap().is_empty());
}
#[tokio::test]
async fn corrupted_state_recovers_and_failed_http_is_not_retried() {
    let cfg = cfg(true);
    let path = path();
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("telemetry_state.json"), b"broken{").unwrap();
    let http = Arc::new(Http {
        fail: true,
        ..Default::default()
    });
    let t = telemetry(
        cfg,
        &path,
        http.clone(),
        Arc::new(Time(Mutex::new("2026-10-03".into()))),
        "beta",
    );
    t.start();
    t.track_error("MIC_ERROR");
    settle().await;
    assert_eq!(http.requests.lock().unwrap().len(), 2);
    assert_eq!(http.requests.lock().unwrap()[0].0, BETA_APP_KEY);
    t.track_error("MIC_ERROR");
    settle().await;
    assert_eq!(http.requests.lock().unwrap().len(), 2);
    t.shutdown();
}

#[tokio::test]
async fn shutdown_cancels_transport_future_already_in_flight() {
    struct Pending(Arc<AtomicUsize>);
    impl Transport for Pending {
        fn send(&self, _: &str, _: Value) -> BoxFuture<'static, Result<(), String>> {
            let alive = self.0.clone();
            Box::pin(async move {
                struct Guard(Arc<AtomicUsize>);
                impl Drop for Guard {
                    fn drop(&mut self) {
                        self.0.fetch_sub(1, Ordering::SeqCst);
                    }
                }
                alive.fetch_add(1, Ordering::SeqCst);
                let _guard = Guard(alive);
                std::future::pending().await
            })
        }
    }
    let active = Arc::new(AtomicUsize::new(0));
    let t = Telemetry::with_transport(
        cfg(true),
        path(),
        "1.0.0-beta",
        "beta",
        tokio::runtime::Handle::current(),
        Arc::new(Time(Mutex::new("2026-10-03".into()))),
        Arc::new(Pending(active.clone())),
    );
    t.start();
    settle().await;
    assert_eq!(active.load(Ordering::SeqCst), 1);
    t.shutdown();
    settle().await;
    assert_eq!(active.load(Ordering::SeqCst), 0);
}
