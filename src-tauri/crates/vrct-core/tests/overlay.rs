use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use vrct_core::{
    overlay::{
        render::{Frame, Renderer},
        transform, Driver, DriverFactory, Overlay, Settings, Size,
    },
    pipeline::host::{LargeLog, SmallLog},
    transcription::native::Config,
};

fn fonts() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../resources/fonts")
}
#[derive(Default)]
struct FakeConfig(Mutex<HashMap<String, Value>>);
impl Config for FakeConfig {
    fn get(&self, name: &str) -> Option<Value> {
        self.0.lock().unwrap().get(name).cloned()
    }
}
fn config() -> Arc<FakeConfig> {
    let cfg = Arc::new(FakeConfig::default());
    cfg.0
        .lock()
        .unwrap()
        .insert("OVERLAY_SMALL_LOG".into(), json!(true));
    cfg.0
        .lock()
        .unwrap()
        .insert("OVERLAY_LARGE_LOG".into(), json!(true));
    cfg
}
fn log<'a>(message: &'a str, ruby: &'a [Value]) -> SmallLog<'a> {
    SmallLog {
        message: Some(message),
        language: Some("Japanese"),
        translation: &[],
        your_languages: &Value::Null,
        transliteration_message: ruby,
        transliteration_translation: &[],
    }
}

#[test]
fn plain_small_layout_dimensions_and_readable_rgba() {
    let renderer = Renderer::load(fonts()).unwrap();
    let frame = renderer.small(&log("Hello, 世界 ไทย 한국어", &[])).unwrap();
    assert_eq!((frame.width, frame.height), (3940, 304));
    assert_eq!(frame.pixels.len(), 3940 * 304 * 4);
    assert_eq!(&frame.pixels[..4], &[0, 0, 0, 0]);
    assert!(frame
        .pixels
        .chunks_exact(4)
        .any(|p| p[0] > 100 && p[1] > 100 && p[2] > 100 && p[3] > 0));
    assert!(frame.pixels.chunks_exact(4).any(|p| p == [41, 42, 45, 255]));
    let empty = renderer.small(&log("", &[])).unwrap();
    assert_eq!(empty.height, 304);
}
#[test]
fn ruby_increases_height_and_preserves_long_token_wrapping() {
    let renderer = Renderer::load(fonts()).unwrap();
    let ruby = vec![
        json!({"orig":"東京","hira":"とうきょう","hepburn":"toukyou"}),
        json!({"orig":"に","hira":"に","hepburn":"ni"}),
        json!({"orig":"行く","hira":"いく","hepburn":"iku"}),
    ];
    let frame = renderer.small(&log("東京に行く", &ruby)).unwrap();
    assert_eq!(frame.width, 3940);
    assert_eq!(frame.height, 310);
    let long_ruby: Vec<_> = (0..100).map(|_| ruby[0].clone()).collect();
    let long = renderer
        .small(&log(&"東京".repeat(100), &long_ruby))
        .unwrap();
    assert!(long.height > frame.height);
    assert!(long.pixels.chunks_exact(4).filter(|p| p[0] > 100).count() > 500);
}
#[test]
fn large_conversation_retains_last_five_and_clear_removes_history() {
    let mut renderer = Renderer::load(fonts()).unwrap();
    let mut last_height = 0;
    for index in 0..7 {
        let frame = renderer
            .large(
                &LargeLog {
                    direction: if index % 2 == 0 { "send" } else { "receive" },
                    message: Some("会話 hello"),
                    language: Some("Japanese"),
                    translation: &[],
                    languages: &Value::Null,
                    transliteration_message: &[],
                    transliteration_translation: &[],
                },
                "12:34",
            )
            .unwrap();
        assert_eq!(frame.width, 1010);
        if index < 5 {
            assert!(frame.height > last_height);
        } else {
            assert_eq!(frame.height, last_height);
        }
        last_height = frame.height;
    }
    assert_eq!(renderer.history_len(), 5);
    renderer.clear_history();
    assert_eq!(renderer.history_len(), 0);
}
#[test]
fn transforms_match_python_base_matrices_and_composed_local_rotation() {
    let cfg = config();
    let mut settings = Settings::read(cfg.as_ref(), Size::Small);
    let hmd = transform(&settings);
    assert_eq!(
        hmd,
        [[1., 0., 0., 0.], [0., 1., 0., -0.4], [0., 0., 1., -1.]]
    );
    settings.position = [0.2, 0.3, 0.4];
    settings.rotation = [0., 0., 90.];
    let matrix = transform(&settings);
    assert!((matrix[0][1] + 1.).abs() < 1e-6);
    assert!((matrix[1][0] - 1.).abs() < 1e-6);
    assert!((matrix[2][3] + 1.4).abs() < 1e-6);
    settings.position = [0.; 3];
    settings.rotation = [0.; 3];
    settings.tracker = "LeftHand".into();
    let left = transform(&settings);
    assert!((left[0][3] + 0.093).abs() < 1e-6);
    assert!((left[1][3] + 0.031).abs() < 1e-6);
    assert!((left[2][3] - 0.31).abs() < 1e-6);
    assert!((left[0][0] - 0.4082179).abs() < 1e-5);
    assert!((left[1][0] + 0.8754261).abs() < 1e-5);
    assert!((left[2][0] + 0.25881904).abs() < 1e-5);
}
#[test]
fn fade_zero_disables_fade_and_large_width_uses_quarter_scale() {
    let cfg = config();
    let mut settings = Settings::read(cfg.as_ref(), Size::Large);
    assert_eq!(settings.width, 0.25);
    assert_eq!(settings.alpha(4.), 1.);
    assert_eq!(settings.alpha(6.), 0.5);
    assert_eq!(settings.alpha(8.), 0.);
    settings.fadeout_duration = 0.;
    assert_eq!(settings.alpha(1_000.), 1.);
    settings.fadeout_duration = 2.;
    settings.opacity = 0.5;
    assert_eq!(settings.alpha(6.), 0.25);
}

#[derive(Default)]
struct Calls {
    connect: AtomicUsize,
    drops: AtomicUsize,
    images: Mutex<Vec<(Size, u32, u32)>>,
    settings: Mutex<Vec<(Size, Settings, f32)>>,
    fail_images: AtomicUsize,
}
struct Factory(Arc<Calls>);
struct FakeDriver(Arc<Calls>);
impl DriverFactory for Factory {
    fn connect(&self) -> Result<Box<dyn Driver>, String> {
        self.0.connect.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(FakeDriver(self.0.clone())))
    }
}
impl Driver for FakeDriver {
    fn image(&mut self, size: Size, frame: &Frame) -> Result<(), String> {
        if self
            .0
            .fail_images
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err("fake disconnected image".into());
        }
        self.0
            .images
            .lock()
            .unwrap()
            .push((size, frame.width, frame.height));
        Ok(())
    }
    fn settings(&mut self, size: Size, settings: &Settings, alpha: f32) -> Result<(), String> {
        self.0
            .settings
            .lock()
            .unwrap()
            .push((size, settings.clone(), alpha));
        Ok(())
    }
    fn active(&mut self) -> Result<bool, String> {
        Ok(true)
    }
}
impl Drop for FakeDriver {
    fn drop(&mut self) {
        self.0.drops.fetch_add(1, Ordering::SeqCst);
    }
}
fn wait(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(4);
    while !predicate() {
        assert!(Instant::now() < deadline, "overlay condition timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn worker_start_is_idempotent_live_settings_apply_and_shutdown_drops_driver() {
    let cfg = config();
    let calls = Arc::new(Calls::default());
    let errors = Arc::new(Mutex::new(Vec::new()));
    let error_copy = errors.clone();
    let overlay = Overlay::with_factory(
        Renderer::load(fonts()).unwrap(),
        cfg.clone(),
        Arc::new(Factory(calls.clone())),
        Arc::new(move |error| error_copy.lock().unwrap().push(error)),
    )
    .unwrap();
    assert_eq!(calls.connect.load(Ordering::SeqCst), 0);
    overlay.start().unwrap();
    overlay.start().unwrap();
    wait(|| overlay.available());
    assert_eq!(calls.connect.load(Ordering::SeqCst), 1);
    overlay.small_log(&log("日本語 hello", &[])).unwrap();
    wait(|| calls.images.lock().unwrap().iter().any(|v| v.1 == 3940));
    cfg.0.lock().unwrap().insert(
        "OVERLAY_SMALL_LOG_SETTINGS".into(),
        json!({"opacity":0.3,"ui_scaling":2.,"tracker":"RightHand"}),
    );
    wait(|| {
        calls.settings.lock().unwrap().iter().any(|(size, s, _)| {
            *size == Size::Small && s.opacity == 0.3 && s.width == 2. && s.tracker == "RightHand"
        })
    });
    overlay.shutdown().unwrap();
    assert!(!overlay.available());
    assert_eq!(calls.drops.load(Ordering::SeqCst), 1);
    assert!(errors.lock().unwrap().is_empty());
    assert!(overlay.clear_small().is_err());
    overlay.start().unwrap();
    wait(|| overlay.available());
    overlay.shutdown().unwrap();
    assert_eq!(calls.drops.load(Ordering::SeqCst), 2);
}
#[test]
fn image_failure_releases_driver_and_reconnects_without_unbounded_wait() {
    let calls = Arc::new(Calls::default());
    calls.fail_images.store(1, Ordering::SeqCst);
    let errors = Arc::new(Mutex::new(Vec::new()));
    let error_copy = errors.clone();
    let overlay = Overlay::with_factory(
        Renderer::load(fonts()).unwrap(),
        config(),
        Arc::new(Factory(calls.clone())),
        Arc::new(move |error| error_copy.lock().unwrap().push(error)),
    )
    .unwrap();
    overlay.start().unwrap();
    wait(|| overlay.available());
    assert_eq!(calls.connect.load(Ordering::SeqCst), 2);
    assert_eq!(calls.drops.load(Ordering::SeqCst), 1);
    overlay.shutdown().unwrap();
    assert_eq!(calls.drops.load(Ordering::SeqCst), 2);
    assert_eq!(errors.lock().unwrap().len(), 1);
}
#[test]
fn renderer_rejects_huge_input_instead_of_allocating_unbounded_image() {
    let renderer = Renderer::load(fonts()).unwrap();
    assert!(renderer.small(&log(&"日".repeat(33_000), &[])).is_err());
}
