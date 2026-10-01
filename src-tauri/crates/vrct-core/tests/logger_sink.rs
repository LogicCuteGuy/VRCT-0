//! The log sink must write the file Python's `setupLogger` did.
//! `fixtures/logger_golden.json` is captured from the real Python logger by
//! `fixtures/regenerate_logger_golden.py` (timestamps replaced by `<TS>`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};
use vrct_core::config::ConfigReplica;
use vrct_core::protocol::{parse_sidecar_line, Response};
use vrct_core::sinks::Sinks;

const GOLDEN: &str = include_str!("fixtures/logger_golden.json");
const EOL: &str = if cfg!(windows) { "\r\n" } else { "\n" };
const MARKER: &str = " - log - INFO - ";
const STAMP_LEN: usize = "2026-10-02 03:04:05,007".len();

fn line(endpoint: &str, result: Value) -> Response {
    let text = json!({"status": 200, "endpoint": endpoint, "result": result}).to_string();
    parse_sidecar_line(&text).expect("sidecar line parses")
}

fn sinks() -> Sinks {
    Sinks::new(Arc::new(ConfigReplica::default()))
}

fn start(sinks: &Sinks, path: &Path) {
    assert!(sinks.ingest(&line("/internal/logger/start", json!({"path": path.to_str().unwrap()}))));
}

fn log(sinks: &Sinks, text: &str) {
    assert!(sinks.ingest(&line("/internal/logger/line", json!({"text": text}))));
}

/// A scratch folder that is removed afterwards.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("vrct-logger-{name}-{}-{nanos}", std::process::id()));
        Self(dir)
    }
    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Replace each record's timestamp with `<TS>` and the platform line end
/// with `\n`, so the text can be compared with the fixture.
fn normalized(raw: &[u8]) -> String {
    let text = String::from_utf8(raw.to_vec()).expect("log is UTF-8").replace(EOL, "\n");
    let mut out = String::new();
    let mut rest = text.as_str();
    while let Some(at) = rest.find(MARKER) {
        let (before, after) = rest.split_at(at);
        let stamp_at = before.len().checked_sub(STAMP_LEN).expect("a timestamp precedes the marker");
        let stamp = &before[stamp_at..];
        assert!(
            stamp.bytes().enumerate().all(|(i, b)| match i {
                4 | 7 => b == b'-',
                10 => b == b' ',
                13 | 16 => b == b':',
                19 => b == b',',
                _ => b.is_ascii_digit(),
            }),
            "not a timestamp: {stamp:?}"
        );
        out.push_str(&before[..stamp_at]);
        out.push_str("<TS>");
        out.push_str(MARKER);
        rest = &after[MARKER.len()..];
    }
    out.push_str(rest);
    out
}

#[test]
fn the_file_is_what_the_python_logger_wrote() {
    let golden: Value = serde_json::from_str(GOLDEN).unwrap();
    let scratch = Scratch::new("golden");
    let path = scratch.file("run.log");
    let sinks = sinks();
    start(&sinks, &path);
    for message in golden["messages"].as_array().unwrap() {
        log(&sinks, message.as_str().unwrap());
    }

    let raw = std::fs::read(&path).unwrap();
    assert_eq!(normalized(&raw), golden["file"].as_str().unwrap());
    // Python wrote text-mode line ends; so must we, byte for byte.
    assert_eq!(raw.windows(2).filter(|w| w == b"\r\n").count() > 0, cfg!(windows));
}

#[test]
fn nothing_is_written_before_start_or_after_stop_and_the_file_waits_for_a_line() {
    let scratch = Scratch::new("lifecycle");
    let path = scratch.file("logs").join("run.log");
    let sinks = sinks();

    log(&sinks, "before start");
    assert!(!path.exists());

    start(&sinks, &path);
    assert!(!path.exists(), "the file appears with the first line, like delay=True");
    log(&sinks, "one");
    assert!(sinks.ingest(&line("/internal/logger/stop", Value::Null)));
    log(&sinks, "after stop");

    let text = normalized(&std::fs::read(&path).unwrap());
    assert_eq!(text, "<TS> - log - INFO - one\n");
}

#[test]
fn a_new_file_gets_the_lines_and_the_old_one_stops() {
    // Python's logger kept every earlier handler and wrote to all of them.
    let scratch = Scratch::new("restart");
    let (first, second) = (scratch.file("a.log"), scratch.file("b.log"));
    let sinks = sinks();

    start(&sinks, &first);
    log(&sinks, "to a");
    sinks.ingest(&line("/internal/logger/stop", Value::Null));
    start(&sinks, &second);
    log(&sinks, "to b");

    assert_eq!(normalized(&std::fs::read(&first).unwrap()), "<TS> - log - INFO - to a\n");
    assert_eq!(normalized(&std::fs::read(&second).unwrap()), "<TS> - log - INFO - to b\n");
}

#[test]
fn an_existing_log_is_appended_to() {
    let scratch = Scratch::new("append");
    let path = scratch.file("run.log");
    let first = sinks();
    start(&first, &path);
    log(&first, "first run");
    let second = sinks();
    start(&second, &path);
    log(&second, "second run");

    let text = normalized(&std::fs::read(&path).unwrap());
    assert_eq!(text, "<TS> - log - INFO - first run\n<TS> - log - INFO - second run\n");
}

#[test]
fn the_file_is_emptied_in_place_when_it_would_pass_ten_megabytes() {
    let scratch = Scratch::new("truncate");
    let path = scratch.file("run.log");
    let sinks = sinks();
    start(&sinks, &path);

    let size = 6 * 1024 * 1024;
    log(&sinks, &"x".repeat(size));
    log(&sinks, "small"); // 6 MB + a short line still fits
    let raw = std::fs::read(&path).unwrap();
    assert!(raw.len() > size && raw.ends_with(format!("small{EOL}").as_bytes()));

    // A second 6 MB record would pass the limit, so the file starts over with it.
    log(&sinks, &"y".repeat(size));
    let raw = std::fs::read(&path).unwrap();
    let record_len = STAMP_LEN + MARKER.len() + size + EOL.len();
    assert_eq!(raw.len(), record_len);
    assert!(raw[STAMP_LEN + MARKER.len()..].starts_with(b"yyyy"));
    // Emptied in place: no rotated backup appears next to it.
    assert_eq!(std::fs::read_dir(&scratch.0).unwrap().count(), 1);
}

#[test]
fn malformed_lines_are_consumed_without_effect() {
    let scratch = Scratch::new("malformed");
    let sinks = sinks();
    assert!(sinks.ingest(&line("/internal/logger/start", json!({}))));
    assert!(sinks.ingest(&line("/internal/logger/start", json!({"path": ""}))));
    assert!(sinks.ingest(&line("/internal/logger/line", json!({"oops": 1}))));
    assert!(!scratch.0.exists());
}
