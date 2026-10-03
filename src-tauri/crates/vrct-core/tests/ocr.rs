use serde_json::json;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use vrct_core::ocr::{
    self,
    engine::{decode_ctc, non_max_suppression, Bubble},
    Config, Dedup, Line, OcrService, Scanner,
};
use vrct_core::settings::{system::production_env, Devices, Settings};

struct NoAudio;
impl Devices for NoAudio {
    fn mic_hosts(&self) -> Vec<String> {
        vec![]
    }
    fn mic_device_names(&self, _: &str) -> Vec<String> {
        vec![]
    }
    fn speaker_device_names(&self) -> Vec<String> {
        vec![]
    }
    fn default_mic(&self) -> Option<(String, String)> {
        None
    }
    fn default_speaker(&self) -> Option<String> {
        None
    }
}
fn settings() -> Arc<Settings> {
    static TEST_ID: AtomicUsize = AtomicUsize::new(0);
    let local = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../target/ocr-tests/{}-{}",
        std::process::id(),
        TEST_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let mut env = production_env("3.5.0", local);
    env.devices = Arc::new(NoAudio);
    Arc::new(Settings::open(env).0)
}

#[test]
fn language_scripts_line_merge_dedup_and_ctc_validate_inputs() {
    for language in [
        "auto",
        "Japanese",
        "Korean",
        "Russian",
        "Ukrainian",
        "Thai",
        "Arabic",
        "Hindi",
    ] {
        assert!(ocr::model_spec(language).is_some());
    }
    assert!(ocr::model_spec("Swahili").is_none());
    let line = |text: &str, top| Line {
        text: text.into(),
        top,
        confidence: 0.9,
    };
    assert_eq!(
        ocr::merge_lines(vec![line("世界", 10.0), line("こんにちは", 0.0)]),
        "こんにちは世界"
    );
    assert_eq!(
        ocr::merge_lines(vec![
            line("Hello", 0.0),
            line("world.", 10.0),
            line("Again", 20.0)
        ]),
        "Hello world.\nAgain"
    );
    let mut cache = Dedup::default();
    assert!(!cache.seen("Hello world", Duration::ZERO));
    assert!(cache.seen("HELLO w0rld", Duration::from_secs(29)));
    cache.evict(Duration::from_secs(40), Duration::from_secs(30));
    assert!(cache.seen("Hello world", Duration::from_secs(40)));
    cache.evict(Duration::from_secs(71), Duration::from_secs(30));
    assert!(!cache.seen("Hello world", Duration::from_secs(71)));
    let labels = vec!["".into(), "ก".into(), " ".into()];
    assert_eq!(
        decode_ctc(
            &[0.1, 0.9, 0.0, 0.2, 0.8, 0.0, 0.9, 0.1, 0.0, 0.1, 0.9, 0.0],
            3,
            &labels
        )
        .unwrap()
        .0,
        "กก"
    );
    assert!(decode_ctc(&[0.1], 3, &labels).is_err());
    assert!(decode_ctc(&[], 0, &[]).is_err());
    let boxes = vec![
        Bubble {
            x: 0,
            y: 0,
            width: 100,
            height: 50,
            score: 0.9,
        },
        Bubble {
            x: 0,
            y: 0,
            width: 100,
            height: 50,
            score: 0.8,
        },
        Bubble {
            x: 200,
            y: 0,
            width: 100,
            height: 50,
            score: 0.7,
        },
    ];
    assert_eq!(non_max_suppression(boxes, 0.65).len(), 2);
}
struct FakeScanner {
    drops: Arc<AtomicUsize>,
}
impl Scanner for FakeScanner {
    fn configure(&mut self, _: &Config) -> Result<(), String> {
        Ok(())
    }
    fn scan(&mut self, config: &Config, _: &AtomicBool) -> Result<Vec<String>, String> {
        Ok(vec![format!("{} test message", config.language)])
    }
}
impl Drop for FakeScanner {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}
#[test]
fn service_has_no_startup_capture_reconfigures_on_worker_and_stops_synchronously() {
    let settings = settings();
    settings.set("OCR_POLL_INTERVAL_MS", json!(100)).unwrap();
    let starts = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    let (sender, events) = mpsc::channel();
    let start_counter = starts.clone();
    let drop_counter = drops.clone();
    let service = OcrService::with_factory(
        settings.clone(),
        Arc::new(move |value| {
            let _ = sender.send(value);
        }),
        Arc::new(|error| panic!("{error}")),
        Arc::new(move |_, _| {
            start_counter.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(FakeScanner {
                drops: drop_counter.clone(),
            }))
        }),
    );
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    service.start().unwrap();
    service.start().unwrap();
    let first = events.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(first["source"], "ocr");
    assert_eq!(first["language"], serde_json::Value::Null);
    assert!(events.recv_timeout(Duration::from_millis(250)).is_err());
    settings.set("OCR_SOURCE_LANGUAGE", json!("Thai")).unwrap();
    service.configure();
    assert_eq!(
        events.recv_timeout(Duration::from_secs(2)).unwrap()["language"],
        "Thai"
    );
    let stopping = Instant::now();
    service.stop();
    assert!(stopping.elapsed() < Duration::from_secs(1));
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    service.start().unwrap();
    service.shutdown();
    assert_eq!(drops.load(Ordering::SeqCst), 2);
    assert!(service.start().is_err());
}
#[test]
fn failed_start_can_retry_and_cancellation_interrupts_initialisation() {
    let settings = settings();
    let attempts = Arc::new(AtomicUsize::new(0));
    let count = attempts.clone();
    let service = Arc::new(OcrService::with_factory(
        settings,
        Arc::new(|_| {}),
        Arc::new(|_| {}),
        Arc::new(move |_, cancelled| {
            if count.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err("model load failed".into());
            }
            while !cancelled.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err("cancelled".into())
        }),
    ));
    assert!(service.start().is_err());
    let starting = service.clone();
    let thread = std::thread::spawn(move || starting.start());
    let deadline = Instant::now() + Duration::from_secs(2);
    while attempts.load(Ordering::SeqCst) < 2 {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    service.stop();
    assert!(thread.join().unwrap().is_err());
}

struct FailedScanner(Arc<AtomicUsize>);
impl Scanner for FailedScanner {
    fn configure(&mut self, _: &Config) -> Result<(), String> {
        Ok(())
    }
    fn scan(&mut self, _: &Config, _: &AtomicBool) -> Result<Vec<String>, String> {
        Err("capture failed".into())
    }
}
impl Drop for FailedScanner {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
#[test]
fn terminal_scan_error_reports_once_and_releases_capture() {
    let drops = Arc::new(AtomicUsize::new(0));
    let capture_drops = drops.clone();
    let (sender, errors) = mpsc::channel();
    let service = OcrService::with_factory(
        settings(),
        Arc::new(|_| panic!("failed capture emitted text")),
        Arc::new(move |error| {
            sender.send(error).unwrap();
        }),
        Arc::new(move |_, _| Ok(Box::new(FailedScanner(capture_drops.clone())))),
    );
    service.start().unwrap();
    assert_eq!(
        errors.recv_timeout(Duration::from_secs(2)).unwrap(),
        "capture failed"
    );
    service.stop();
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert!(errors.try_recv().is_err());
}

/// Uses the pinned actual ONNX networks and generated multilingual images;
/// opt-in because the prepared weights/runtime are deployment resources.
#[test]
#[ignore = "prepare resources/ocr models and set ORT_DYLIB_PATH first"]
fn real_onnx_all_language_models_and_bubble_detector_smoke() {
    use vrct_core::audio::silero::OnnxRuntime;
    use vrct_core::ocr::engine::PaddleReader;
    OnnxRuntime::load(&OnnxRuntime::locate().expect("ORT_DYLIB_PATH")).unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cancel = Arc::new(AtomicBool::new(false));
    for language in [
        "English", "Japanese", "Korean", "Russian", "Thai", "Arabic", "Hindi",
    ] {
        let config = Config {
            local: root.clone(),
            language: language.into(),
            window_title: "VRChat".into(),
            interval: Duration::from_secs(60),
            confidence: 0.0,
            min_length: 1,
        };
        let mut reader = PaddleReader::new(&config, cancel.clone()).unwrap();
        let image = image::open(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("tests/fixtures/ocr/{language}.png")),
        )
        .unwrap()
        .to_rgb8();
        let lines = reader.recognize(&image, 0.5).unwrap();
        assert!(lines
            .iter()
            .all(|line| line.confidence.is_finite() && line.confidence >= 0.5));
        let text = ocr::merge_lines(lines);
        eprintln!("{language}: {text}");
        // Exact checks exercise actual DB/crop/classifier/CTC inference. Korean
        // and Devanagari synthetic fonts are poorly recognized by upstream too;
        // executing their networks verifies availability, not language accuracy.
        match language {
            "English" => assert_eq!(text, "Hello VRChat"),
            "Japanese" => assert_eq!(text, "こんにちは"),
            "Russian" => assert_eq!(text.replace(' ', ""), "Приветмир"),
            "Thai" => assert_eq!(text, "สวัสดีครับ"),
            _ => {}
        }
        if language == "Thai" {
            assert!(text.chars().any(|c| ('\u{0e00}'..='\u{0e7f}').contains(&c)));
        }
        if language == "Arabic" {
            assert!(text.chars().any(|c| ('\u{0600}'..='\u{06ff}').contains(&c)));
        }
    }
}

#[test]
#[ignore = "requires a locally authorized external bubble detector and ORT_DYLIB_PATH"]
fn real_onnx_authorized_external_bubble_detector_smoke() {
    use vrct_core::audio::silero::OnnxRuntime;
    use vrct_core::ocr::engine::BubbleDetector;
    OnnxRuntime::load(&OnnxRuntime::locate().expect("ORT_DYLIB_PATH")).unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bubble_config = Config {
        local: root,
        language: "auto".into(),
        window_title: "VRChat".into(),
        interval: Duration::from_secs(60),
        confidence: 0.0,
        min_length: 1,
    };
    let mut bubbles = BubbleDetector::new(&bubble_config).unwrap();
    assert!(bubbles
        .detect(&image::RgbImage::from_pixel(
            640,
            360,
            image::Rgb([255, 255, 255])
        ))
        .unwrap()
        .is_empty());
}
