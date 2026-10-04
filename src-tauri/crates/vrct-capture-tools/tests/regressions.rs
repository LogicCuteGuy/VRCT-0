//! Tests never open a native capture device or use VRChat UDP port 9000.
use clap::Parser;
use image::{Rgb, RgbImage};
use serde_json::json;
use std::{
    collections::HashSet,
    fs,
    net::UdpSocket,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use vrct_capture_tools::{
    collector::{
        self, Collector, Command as CaptureCommand, ImageStore, Label, Options, OscValue, Wanderer,
    },
    player::{self, Playback, Sample},
    source::{self, CaptureUnavailable, Frame, Source, VrIdentity},
};
fn folder() -> PathBuf {
    static ID: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "vrct-capture-tests-{}-{}-{}",
        std::process::id(),
        ID.fetch_add(1, Ordering::Relaxed),
        vrct_capture_tools::run_id().unwrap()
    ));
    fs::create_dir_all(&path).unwrap();
    path
}
fn catalog() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tools/chatbox_samples")
}
fn frame(age: Duration) -> Frame {
    Frame {
        rgb: RgbImage::from_raw(2, 1, vec![255, 0, 7, 0, 5, 255]).unwrap(),
        captured_monotonic: Instant::now() - age,
        metadata: json!({"captured_at":"2026-10-04T00:00:00Z","backend":"fake","eye":"left"}),
    }
}
fn options() -> Options {
    Options {
        interval: Duration::from_millis(20),
        duration: Duration::from_millis(180),
        manual: false,
        max_frames: 0,
        max_age: Duration::from_secs(2),
    }
}
fn silent() -> collector::Emit {
    Arc::new(|_| {})
}
#[test]
fn catalog_language_lengths_utf16_and_lines_match_original() {
    let samples = player::load_samples(&catalog()).unwrap();
    let languages: HashSet<_> = samples
        .iter()
        .map(|s| &s.language)
        .filter(|s| s.as_str() != "mixed")
        .collect();
    assert_eq!(languages.len(), 20);
    assert!(samples.len() >= 240);
    assert_eq!(
        samples.iter().map(|s| &s.id).collect::<HashSet<_>>().len(),
        samples.len()
    );
    assert_eq!(samples.iter().map(|s| s.units()).min(), Some(1));
    assert_eq!(samples.iter().map(|s| s.units()).max(), Some(144));
    assert_eq!(
        samples
            .iter()
            .map(|s| s.text.matches('\n').count() + 1)
            .max(),
        Some(9)
    );
    for language in languages {
        assert_eq!(
            samples
                .iter()
                .filter(|s| &s.language == language)
                .map(|s| format!("{:?}", s.length()))
                .collect::<HashSet<_>>()
                .len(),
            4
        );
    }
}
#[test]
fn editable_samples_fail_before_any_send_without_truncation() {
    for text in [
        "".into(),
        " \n ".into(),
        "a".repeat(145),
        "🙂".repeat(73),
        "1\n".repeat(9),
        "bad\0text".into(),
        "bad\rtext".into(),
        "bad\u{1b}text".into(),
        "bad\u{202e}text".into(),
    ] {
        let directory = folder();
        fs::write(
            directory.join("samples.json"),
            json!({"en":[{"text":text}]}).to_string(),
        )
        .unwrap();
        assert!(player::load_samples(&directory).is_err(), "{text:?}");
    }
    let directory = folder();
    let text = "🙂".repeat(72);
    fs::write(
        directory.join("samples.json"),
        format!(
            "\u{feff}{}",
            json!({"en":[{"text":text},{"text":"a\n\nb"}]})
        ),
    )
    .unwrap();
    let samples = player::load_samples(&directory).unwrap();
    assert_eq!(samples[0].units(), 144);
    assert_eq!(samples[1].text, "a\n\nb");
    fs::write(
        directory.join("other.json"),
        json!({"en":[{"text":"Hi"}]}).to_string(),
    )
    .unwrap();
    assert!(player::load_samples(&directory).is_err());
    #[cfg(windows)]
    {
        let directory = folder();
        fs::write(
            directory.join("Z.json"),
            json!({"en":[{"text":"z"}]}).to_string(),
        )
        .unwrap();
        fs::write(
            directory.join("a.JSON"),
            json!({"ja":[{"text":"a"}]}).to_string(),
        )
        .unwrap();
        let samples = player::load_samples(&directory).unwrap();
        assert_eq!(
            samples
                .iter()
                .map(|s| s.language.as_str())
                .collect::<Vec<_>>(),
            ["ja", "en"]
        );
    }
}
#[test]
fn osc_text_and_actual_bool_arguments_are_byte_exact() {
    let text = "日本語 / مرحبا / café / 🙂\n第二行";
    let packet = player::message_bytes(text);
    let mut expected = Vec::new();
    player::pad_string(&mut expected, "/chatbox/input");
    player::pad_string(&mut expected, ",sTF");
    player::pad_string(&mut expected, text);
    assert_eq!(packet, expected);
    assert_eq!(
        collector::osc_message("/input/Jump", OscValue::Int(1)),
        b"/input/Jump\0,i\0\0\0\0\0\x01"
    );
    assert_eq!(
        collector::osc_message("/input/Vertical", OscValue::Float(1.0)),
        b"/input/Vertical\0,f\0\0?\x80\0\0"
    );
}
fn samples() -> Vec<Sample> {
    (0..10)
        .map(|i| Sample {
            id: format!("en_{i:03}"),
            language: "en".into(),
            text: i.to_string(),
            tags: vec![],
        })
        .collect()
}
#[test]
fn playback_pause_quit_and_completion_deadlines_never_burst() {
    let mut playback =
        Playback::new(samples(), Duration::from_secs(6), false, 0, true, None).unwrap();
    assert!(playback.due(Duration::ZERO).is_some());
    playback.completed(Duration::from_secs(20));
    assert_eq!(playback.deadline, Duration::from_secs(26));
    assert!(playback.due(Duration::from_secs(20)).is_none());
    assert!(playback.due(Duration::from_secs(100)).is_some());
    playback.completed(Duration::from_secs(100));
    assert!(playback.due(Duration::from_secs(100)).is_none());
    playback.command('p', Duration::from_secs(100));
    assert!(playback.due(Duration::from_secs(200)).is_none());
    playback.command('p', Duration::from_secs(200));
    assert!(playback.due(Duration::from_secs(200)).is_none());
    assert!(playback.due(Duration::from_secs(206)).is_some());
    playback.command('q', Duration::from_secs(206));
    assert!(playback.due(Duration::from_secs(1000)).is_none());
    let a = Playback::new(
        samples(),
        Duration::from_secs(6),
        false,
        0,
        false,
        Some("42"),
    )
    .unwrap();
    let b = Playback::new(
        samples(),
        Duration::from_secs(6),
        false,
        0,
        false,
        Some("42"),
    )
    .unwrap();
    assert_eq!(a.order, b.order);
    assert_eq!(a.order.len(), 10);
    let huge = "18446744073709551616000000000000000000000000000000000000000000000000000";
    let a = Playback::new(
        samples(),
        Duration::from_secs(6),
        false,
        0,
        false,
        Some(huge),
    )
    .unwrap();
    let b = Playback::new(
        samples(),
        Duration::from_secs(6),
        false,
        0,
        false,
        Some(huge),
    )
    .unwrap();
    assert_eq!(a.order, b.order);
    assert_eq!(vrct_capture_tools::parse_seed("-0000").unwrap(), "0");
    assert_eq!(vrct_capture_tools::parse_seed("+00042").unwrap(), "42");
    assert!(vrct_capture_tools::parse_seed("1e9").is_err());
    let mut once = Playback::new(
        samples()[..2].to_vec(),
        Duration::from_secs(6),
        true,
        0,
        true,
        None,
    )
    .unwrap();
    once.completed(Duration::ZERO);
    once.completed(Duration::from_secs(6));
    assert!(once.stopped);
    let mut limited =
        Playback::new(samples(), Duration::from_secs(6), false, 1, true, None).unwrap();
    limited.completed(Duration::ZERO);
    assert!(limited.stopped);
    assert_eq!(limited.count, 1);
}
#[test]
fn player_cli_dry_list_noninteractive_and_real_ephemeral_udp() {
    let root = folder();
    let logs = root.join("logs");
    let samples = root.join("samples");
    fs::create_dir(&samples).unwrap();
    let text = "こんにちは🙂\nمرحبا";
    fs::write(
        samples.join("sample.json"),
        json!({"mixed":[{"text":text}]}).to_string(),
    )
    .unwrap();
    for args in [vec!["--list"], vec!["--dry-run", "--max-messages", "1"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_vrct-chatbox-player"))
            .args(args)
            .arg("--samples")
            .arg(&samples)
            .arg("--log-dir")
            .arg(&logs)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!logs.exists());
    }
    let cancelled = Command::new(env!("CARGO_BIN_EXE_vrct-chatbox-player"))
        .arg("--samples")
        .arg(&samples)
        .arg("--log-dir")
        .arg(&logs)
        .output()
        .unwrap();
    assert!(!cancelled.status.success());
    assert!(String::from_utf8_lossy(&cancelled.stderr).contains("--start"));
    assert!(!logs.exists());
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let port = socket.local_addr().unwrap().port();
    assert_ne!(port, 9000);
    let output = Command::new(env!("CARGO_BIN_EXE_vrct-chatbox-player"))
        .args([
            "--start",
            "--max-messages",
            "1",
            "--port",
            &port.to_string(),
        ])
        .arg("--samples")
        .arg(&samples)
        .arg("--log-dir")
        .arg(&logs)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut buffer = [0; 4096];
    let (count, _) = socket.recv_from(&mut buffer).unwrap();
    assert_eq!(&buffer[..count], player::message_bytes(text));
    let paths: Vec<_> = fs::read_dir(&logs)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(paths.len(), 1);
    let record: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&paths[0]).unwrap()).unwrap();
    assert_eq!(record["event"], "udp_submitted");
    assert_eq!(record["text"], text);
    assert_eq!(record["target"], format!("127.0.0.1:{port}"));
}
#[test]
fn catalogue_export_cli_preserves_builder_fields_order_bom_and_replaces_generated_files() {
    let root = folder();
    let destination = root.join("catalogue_日本語");
    let logs = root.join("must-not-create-logs");
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let port = socket.local_addr().unwrap().port();
    assert_ne!(port, 9000);
    let expected = player::load_samples(&catalog()).unwrap();
    for attempt in 0..2 {
        let output = Command::new(env!("CARGO_BIN_EXE_vrct-chatbox-player"))
            .arg("--samples")
            .arg(catalog())
            .arg("--export-catalogue")
            .arg(&destination)
            .arg("--log-dir")
            .arg(&logs)
            .args(["--start", "--port", &port.to_string()])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!logs.exists());
        let mut packet = [0u8; 1024];
        assert!(socket.recv_from(&mut packet).is_err());
        let jsonl = fs::read(destination.join("samples.jsonl")).unwrap();
        assert!(!jsonl.starts_with(&[0xef, 0xbb, 0xbf]));
        let records: Vec<serde_json::Value> = std::str::from_utf8(&jsonl)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(records.len(), expected.len());
        for (record, sample) in records.iter().zip(&expected) {
            assert_eq!(record["id"], sample.id);
            assert_eq!(record["language"], sample.language);
            assert_eq!(record["text"], sample.text);
            assert_eq!(record["tags"], json!(sample.tags));
            assert_eq!(record["utf16_units"], sample.units());
            assert_eq!(
                record["length"],
                format!("{:?}", sample.length()).to_lowercase()
            );
            assert_eq!(
                record["explicit_lines"],
                sample.text.matches('\n').count() + 1
            );
            assert_eq!(
                record
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                [
                    "id",
                    "language",
                    "text",
                    "tags",
                    "utf16_units",
                    "length",
                    "explicit_lines"
                ]
            );
        }
        let text = fs::read(destination.join("samples.txt")).unwrap();
        assert!(text.starts_with(&[0xef, 0xbb, 0xbf]));
        let expected_text = expected
            .iter()
            .map(|sample| {
                let length = format!("{:?}", sample.length()).to_lowercase();
                format!(
                    "[{} / {length} / {} UTF-16 units]\n{}",
                    sample.id,
                    sample.units(),
                    sample.text
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")
            + "\n";
        assert_eq!(
            std::str::from_utf8(&text[3..])
                .unwrap()
                .replace("\r\n", "\n"),
            expected_text
        );
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 2);
        // Repeating export must regenerate named outputs rather than append.
        if attempt == 0 {
            fs::write(destination.join("samples.jsonl"), "obsolete").unwrap();
            fs::write(destination.join("samples.txt"), "obsolete").unwrap();
        }
    }
}
#[test]
fn argument_validation_and_unknown_languages_retain_options() {
    for values in [
        vec!["player", "--interval", "nan"],
        vec!["player", "--interval", "2"],
        vec!["player", "--port", "0"],
        vec!["player", "--timeout", "inf"],
    ] {
        let args = player::Args::try_parse_from(values).unwrap();
        assert!(args.validate().is_err());
    }
    assert!(player::Args::try_parse_from(["player", "--max-messages", "-1"]).is_err());
    let args = player::Args::parse_from([
        "player",
        "--samples",
        catalog().to_str().unwrap(),
        "--languages",
        "invalid",
        "--list",
    ]);
    assert!(player::select(&args)
        .unwrap_err()
        .contains("Unknown languages"));
    let args = player::Args::parse_from([
        "player",
        "--samples",
        catalog().to_str().unwrap(),
        "--languages",
        "ja,en",
        "--length",
        "long",
        "--seed",
        "-7",
    ]);
    let selected = player::select(&args).unwrap();
    assert!(selected
        .iter()
        .all(|s| ["ja", "en"].contains(&s.language.as_str()) && s.units() > 80));
    for value in [
        "../bad",
        "CON",
        "con.anything",
        "LPT1",
        "trailing.",
        "a/b",
        "a\\b",
        "",
        "NUL",
        "COM¹",
    ] {
        assert!(collector::session_name(value).is_err(), "{value}");
    }
    assert_eq!(
        collector::session_name("日本語のワールド").unwrap(),
        "日本語のワールド"
    );
    for args in [
        vec!["collector", "--interval", "nan"],
        vec!["collector", "--duration", "inf"],
        vec!["collector", "--max-age", "0"],
        vec!["collector", "--osc-port", "0"],
    ] {
        assert!(collector::Args::parse_from(args).options().is_err());
    }
    assert!(collector::Args::try_parse_from(["collector", "--max-frames", "-1"]).is_err());
    assert!(
        vrct_capture_tools::probe::Args::parse_from(["probe", "--frames", "0"])
            .validate()
            .is_err()
    );
}
#[test]
fn image_pairs_are_lossless_fresh_and_never_overwrite() {
    let root = folder();
    let mut store = ImageStore::new(&root, "test").unwrap();
    let sample = frame(Duration::ZERO);
    let png = store
        .save(&sample, Label::Unlabeled, Duration::from_secs(2))
        .unwrap();
    assert_eq!(image::open(&png).unwrap().to_rgb8(), sample.rgb);
    let record: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(png.with_extension("json")).unwrap()).unwrap();
    assert_eq!(record["captured_at"], sample.metadata["captured_at"]);
    assert_eq!(record["width"], 2);
    assert_eq!(record["height"], 1);
    assert_eq!(record["png_compress_level"], 1);
    assert_eq!(store.count, 1);
    let mut invalid = frame(Duration::ZERO);
    invalid.captured_monotonic = Instant::now() + Duration::from_secs(60);
    assert!(store
        .save(&invalid, Label::Unlabeled, Duration::from_secs(2))
        .is_err());
    invalid.captured_monotonic = Instant::now();
    invalid.rgb = RgbImage::new(0, 0);
    assert!(store
        .save(&invalid, Label::Unlabeled, Duration::from_secs(2))
        .is_err());
    assert_eq!(store.count, 1);
    assert!(store
        .save(
            &frame(Duration::from_secs(10)),
            Label::Unlabeled,
            Duration::from_secs(2)
        )
        .is_err());
    assert_eq!(store.count, 1);
    let mut second = ImageStore::new(&root, "test").unwrap();
    let b = second
        .save(
            &frame(Duration::ZERO),
            Label::Unlabeled,
            Duration::from_secs(2),
        )
        .unwrap();
    assert_ne!(png, b);
    assert!(png.exists() && b.exists());
}
#[test]
fn failed_pair_publication_rolls_back_without_deleting_collisions() {
    for failure in 0..2 {
        let root = folder();
        let mut store = ImageStore::new(&root, "test").unwrap();
        let mut calls = 0;
        let result = store.save_with_publisher(
            &frame(Duration::ZERO),
            Label::Unlabeled,
            Duration::from_secs(2),
            |source, target| {
                if calls == failure {
                    return Err(std::io::Error::other("publish failed"));
                }
                calls += 1;
                fs::rename(source, target)
            },
        );
        assert!(result.is_err());
        assert_eq!(store.count, 0);
        assert_eq!(
            fs::read_dir(store.directory.join("unlabeled"))
                .unwrap()
                .count(),
            0
        );
    }
    let root = folder();
    let mut store = ImageStore::new(&root, "test").unwrap();
    let output = store.directory.join("unlabeled");
    fs::create_dir_all(&output).unwrap();
    let collision = output.join(format!("{}_000000.json", store.run_id));
    fs::write(&collision, "keep").unwrap();
    assert!(store
        .save(
            &frame(Duration::ZERO),
            Label::Unlabeled,
            Duration::from_secs(2)
        )
        .is_err());
    assert_eq!(fs::read_to_string(collision).unwrap(), "keep");
    assert_eq!(fs::read_dir(output).unwrap().count(), 1);
}
#[cfg(windows)]
#[test]
fn output_junctions_cannot_escape_the_dataset_root() {
    use std::os::windows::process::CommandExt;
    for label_junction in [false, true] {
        let root = folder();
        let outside = folder();
        let mut store = ImageStore::new(&root, "guard").unwrap();
        let junction = if label_junction {
            fs::create_dir(&store.directory).unwrap();
            root.join("guard").join("unlabeled")
        } else {
            root.join("guard")
        };
        // Values are separate environment arguments, never interpolated shell
        // code; targets are newly created test directories, not user folders.
        let output = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", "New-Item -ItemType Junction -Path $env:VRCT_TEST_JUNCTION -Target $env:VRCT_TEST_TARGET -ErrorAction Stop | Out-Null"])
            .env("VRCT_TEST_JUNCTION", &junction)
            .env("VRCT_TEST_TARGET", &outside)
            .creation_flags(0x0800_0000)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(store
            .save(
                &frame(Duration::ZERO),
                Label::Unlabeled,
                Duration::from_secs(2)
            )
            .is_err());
        assert_eq!(store.count, 0);
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    }
}
struct FakeSource {
    reads: Arc<AtomicUsize>,
    closed: Arc<AtomicUsize>,
    successes: usize,
}
impl Source for FakeSource {
    fn capture(&mut self) -> Result<Frame, CaptureUnavailable> {
        let index = self.reads.fetch_add(1, Ordering::SeqCst);
        if index < self.successes {
            Ok(frame(Duration::ZERO))
        } else {
            Err(CaptureUnavailable("lost".into()))
        }
    }
    fn close(&mut self) -> Result<(), String> {
        self.closed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
#[test]
fn failed_capture_never_reuses_success_and_resource_closes_once() {
    let reads = Arc::new(AtomicUsize::new(0));
    let closed = Arc::new(AtomicUsize::new(0));
    let (r, c) = (reads.clone(), closed.clone());
    let store = ImageStore::new(&folder(), "test").unwrap();
    let mut worker = Collector::start(
        Box::new(move || {
            Ok(Box::new(FakeSource {
                reads: r,
                closed: c,
                successes: 1,
            }))
        }),
        store,
        options(),
        silent(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !worker.done.load(Ordering::Acquire) {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    let result = worker.stop().unwrap();
    assert_eq!(result.count, 1);
    assert!(reads.load(Ordering::SeqCst) > 1);
    assert_eq!(closed.load(Ordering::SeqCst), 1);
}
#[test]
fn worker_initialization_panic_finishes_and_join_reports_failure() {
    let mut worker = Collector::start(
        Box::new(|| panic!("injected initialization panic")),
        ImageStore::new(&folder(), "panic").unwrap(),
        options(),
        silent(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !worker.done.load(Ordering::Acquire) {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        worker.stop().err().as_deref(),
        Some("Capture worker panicked")
    );
}
#[test]
fn fatal_persistence_failure_stops_and_closes_capture_owner() {
    let reads = Arc::new(AtomicUsize::new(0));
    let closed = Arc::new(AtomicUsize::new(0));
    let (r, c) = (reads.clone(), closed.clone());
    let store = ImageStore::new(&folder(), "blocked").unwrap();
    // A file at the session directory simulates a non-recoverable filesystem
    // error: waiting for another frame must not hide it or leave the source open.
    fs::write(&store.directory, "existing file").unwrap();
    let mut worker = Collector::start(
        Box::new(move || {
            Ok(Box::new(FakeSource {
                reads: r,
                closed: c,
                successes: 10,
            }))
        }),
        store,
        options(),
        silent(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !worker.done.load(Ordering::Acquire) {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    assert!(worker.stop().is_err());
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert_eq!(closed.load(Ordering::SeqCst), 1);
}
#[test]
fn manual_pause_status_duration_and_frame_limit() {
    let reads = Arc::new(AtomicUsize::new(0));
    let closed = Arc::new(AtomicUsize::new(0));
    let (r, c) = (reads.clone(), closed.clone());
    let mut opts = options();
    opts.manual = true;
    opts.duration = Duration::from_secs(2);
    opts.max_frames = 2;
    let (sender, messages) = mpsc::channel();
    let mut worker = Collector::start(
        Box::new(move || {
            Ok(Box::new(FakeSource {
                reads: r,
                closed: c,
                successes: 10,
            }))
        }),
        ImageStore::new(&folder(), "test").unwrap(),
        opts,
        Arc::new(move |value| {
            sender.send(value).unwrap();
        }),
    )
    .unwrap();
    worker.command(CaptureCommand::Pause);
    assert!(messages
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .starts_with("[paused]"));
    worker.command(CaptureCommand::Save(Label::Positive));
    thread::sleep(Duration::from_millis(80));
    assert_eq!(reads.load(Ordering::SeqCst), 0);
    worker.command(CaptureCommand::Status);
    assert!(messages
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .starts_with("[status]"));
    worker.command(CaptureCommand::Pause);
    worker.command(CaptureCommand::Save(Label::Positive));
    worker.command(CaptureCommand::Save(Label::Negative));
    let deadline = Instant::now() + Duration::from_secs(3);
    while !worker.done.load(Ordering::Acquire) {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    let result = worker.stop().unwrap();
    assert_eq!(result.count, 2);
    assert_eq!(
        fs::read_dir(result.directory.join("positive"))
            .unwrap()
            .count(),
        2
    );
    assert_eq!(
        fs::read_dir(result.directory.join("negative"))
            .unwrap()
            .count(),
        2
    );
    let (r, c) = (reads.clone(), closed.clone());
    let mut opts = options();
    opts.manual = true;
    opts.duration = Duration::from_millis(50);
    let mut worker = Collector::start(
        Box::new(move || {
            Ok(Box::new(FakeSource {
                reads: r,
                closed: c,
                successes: 10,
            }))
        }),
        ImageStore::new(&folder(), "timeout").unwrap(),
        opts,
        silent(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !worker.done.load(Ordering::Acquire) {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(worker.stop().unwrap().count, 0);
    assert_eq!(reads.load(Ordering::SeqCst), 2);
}
struct SlowSource {
    entered: mpsc::Sender<()>,
    finish: mpsc::Receiver<()>,
    calls: Arc<Mutex<Vec<(&'static str, thread::ThreadId)>>>,
}
impl Source for SlowSource {
    fn capture(&mut self) -> Result<Frame, CaptureUnavailable> {
        self.calls
            .lock()
            .unwrap()
            .push(("read", thread::current().id()));
        self.entered.send(()).unwrap();
        self.finish.recv_timeout(Duration::from_secs(3)).unwrap();
        self.calls
            .lock()
            .unwrap()
            .push(("read_done", thread::current().id()));
        Ok(frame(Duration::ZERO))
    }
    fn close(&mut self) -> Result<(), String> {
        self.calls
            .lock()
            .unwrap()
            .push(("close", thread::current().id()));
        Ok(())
    }
}
#[test]
fn stop_waits_for_capture_and_releases_on_same_owner_without_saving() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let owned = calls.clone();
    let (entered, read) = mpsc::channel();
    let (finish, release) = mpsc::channel();
    let worker = Collector::start(
        Box::new(move || {
            owned.lock().unwrap().push(("init", thread::current().id()));
            Ok(Box::new(SlowSource {
                entered,
                finish: release,
                calls: owned,
            }))
        }),
        ImageStore::new(&folder(), "test").unwrap(),
        options(),
        silent(),
    )
    .unwrap();
    read.recv_timeout(Duration::from_secs(1)).unwrap();
    let done = worker.done.clone();
    let stopped = thread::spawn(move || {
        let mut worker = worker;
        worker.stop().unwrap()
    });
    thread::sleep(Duration::from_millis(80));
    assert!(!done.load(Ordering::Acquire));
    assert!(!calls
        .lock()
        .unwrap()
        .iter()
        .any(|(call, _)| *call == "close"));
    finish.send(()).unwrap();
    assert_eq!(stopped.join().unwrap().count, 0);
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls.iter().map(|(call, _)| *call).collect::<Vec<_>>(),
        ["init", "read", "read_done", "close"]
    );
    assert!(calls.iter().all(|(_, owner)| *owner == calls[0].1));
    assert_ne!(calls[0].1, thread::current().id());
}
#[test]
fn fresh_scene_identity_rejects_stale_cross_process_and_focus_switches() {
    let before = VrIdentity {
        pid: 12,
        started: 10,
        frame: 100,
        focus: 12,
    };
    assert!(source::validate_vr_before(before, None, true).is_ok());
    assert!(source::validate_vr_before(before, Some(before), true).is_err());
    assert!(source::validate_vr_before(before, None, false).is_err());
    let mut after = before;
    after.frame += 1;
    assert!(source::validate_vr_after(before, after, true).is_ok());
    after.pid = 99;
    assert!(source::validate_vr_after(before, after, true).is_err());
    after = before;
    after.focus = 99;
    assert!(source::validate_vr_after(before, after, true).is_err());
    after = before;
    after.started = 11;
    assert!(source::validate_vr_after(before, after, true).is_err());
    assert_eq!(
        collector::next_deadline(
            Duration::from_secs(10),
            Duration::from_secs_f64(10.6),
            Duration::from_secs(2)
        ),
        Duration::from_secs(12)
    );
    assert_eq!(
        collector::next_deadline(
            Duration::from_secs(10),
            Duration::from_secs_f64(15.7),
            Duration::from_secs(2)
        ),
        Duration::from_secs(16)
    );
}
#[test]
fn wanderer_stops_neutralizes_and_pause_interrupts_active_axis() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let messages = sent.clone();
    let mut wanderer = Wanderer::with_sender(Arc::new(move |address, value| {
        messages.lock().unwrap().push((
            address.to_owned(),
            match value {
                OscValue::Int(v) => v as f32,
                OscValue::Float(v) => v,
            },
        ));
        Ok(())
    }))
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    while sent.lock().unwrap().is_empty() {
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    wanderer.toggle_pause();
    let deadline = Instant::now() + Duration::from_secs(1);
    while !sent
        .lock()
        .unwrap()
        .iter()
        .any(|(a, v)| a == "/input/Vertical" && *v == 0.0)
    {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    wanderer.stop().unwrap();
    let sent = sent.lock().unwrap();
    assert_eq!(
        &sent[sent.len() - 3..],
        [
            ("/input/Vertical".into(), 0.0),
            ("/input/LookHorizontal".into(), 0.0),
            ("/input/Jump".into(), 0.0)
        ]
    );
}
#[test]
fn diagnostic_statistics_preserve_rgb_and_changed_pixel_fraction() {
    let old = RgbImage::from_pixel(2, 1, Rgb([255, 0, 7]));
    let mut frame = old.clone();
    frame.put_pixel(1, 0, Rgb([0, 5, 255]));
    let stats = vrct_capture_tools::probe::image_stats(&frame, Some(&old));
    assert_eq!(stats["changed_pixel_fraction"], 0.5);
    assert!(stats["mean"].as_f64().unwrap() > 0.0);
    assert_eq!(stats["sha256"].as_str().unwrap().len(), 64);
    for binary in [
        env!("CARGO_BIN_EXE_vrct-dataset-collector"),
        env!("CARGO_BIN_EXE_vrct-capture-probe"),
    ] {
        assert!(Command::new(binary)
            .arg("--help")
            .output()
            .unwrap()
            .status
            .success());
    }
    // No capture command is supplied: constructing a manual collector must not
    // initialize OpenVR, read any window surface or send movement input.
    #[cfg(all(windows, target_arch = "x86_64"))]
    {
        let output = Command::new(env!("CARGO_BIN_EXE_vrct-dataset-collector"))
            .args(["manual-cli", "--manual", "--duration", "0.1", "--out"])
            .arg(folder())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("done: saved=0"));
    }
}
