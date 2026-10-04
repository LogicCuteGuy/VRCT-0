//! Native chat-bubble OCR: capture, YOLOX detection, Paddle ONNX recognition,
//! and bounded deduplication. Models and script choices match RapidOCR.

pub mod engine;
#[cfg(windows)]
pub mod hwnd;
mod models;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::settings::Settings;
use serde_json::{json, Value};

pub type Callback = Arc<dyn Fn(Value) + Send + Sync>;
pub type ErrorCallback = Arc<dyn Fn(String) + Send + Sync>;
pub type Factory =
    Arc<dyn Fn(&Config, Arc<AtomicBool>) -> Result<Box<dyn Scanner>, String> + Send + Sync>;

#[derive(Clone, Debug)]
pub struct Config {
    pub local: PathBuf,
    pub language: String,
    pub window_title: String,
    pub interval: Duration,
    pub confidence: f32,
    pub min_length: usize,
}
impl Config {
    fn read(settings: &Settings) -> Self {
        Self {
            local: PathBuf::from(settings.get_str("PATH_LOCAL").unwrap_or_default()),
            language: settings
                .get_str("OCR_SOURCE_LANGUAGE")
                .unwrap_or_else(|| "auto".into()),
            window_title: settings
                .get_str("OCR_WINDOW_TITLE")
                .unwrap_or_else(|| "VRChat".into()),
            interval: Duration::from_millis(
                settings
                    .get("OCR_POLL_INTERVAL_MS")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(750)
                    .clamp(100, 60000),
            ),
            confidence: settings
                .get("OCR_MIN_CONFIDENCE")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.85)
                .clamp(0.0, 1.0) as f32,
            min_length: settings
                .get("OCR_BUBBLE_MIN_TEXT_LENGTH")
                .and_then(|v| v.as_u64())
                .unwrap_or(2)
                .clamp(1, 4096) as usize,
        }
    }
}

/// Factory-created on the OCR thread; native capture and ONNX objects never
/// cross threads. A scanner returns one merged string per bubble.
pub trait Scanner {
    fn configure(&mut self, config: &Config) -> Result<(), String>;
    fn scan(&mut self, config: &Config, cancelled: &AtomicBool) -> Result<Vec<String>, String>;
}

struct Control {
    pending: Mutex<Option<Config>>,
    wake: Condvar,
    cancelled: Arc<AtomicBool>,
}
pub struct OcrService {
    settings: Arc<Settings>,
    callback: Callback,
    error: ErrorCallback,
    factory: Factory,
    control: Arc<Control>,
    worker: Mutex<Option<JoinHandle<()>>>,
    stopped: AtomicBool,
}
impl OcrService {
    pub fn new(settings: Arc<Settings>, callback: Callback, error: ErrorCallback) -> Self {
        Self::with_factory(
            settings,
            callback,
            error,
            Arc::new(|config, cancelled| {
                Ok(Box::new(engine::NativeScanner::new(config, cancelled)?))
            }),
        )
    }
    pub fn with_factory(
        settings: Arc<Settings>,
        callback: Callback,
        error: ErrorCallback,
        factory: Factory,
    ) -> Self {
        Self {
            settings,
            callback,
            error,
            factory,
            control: Arc::new(Control {
                pending: Mutex::new(None),
                wake: Condvar::new(),
                cancelled: Arc::new(AtomicBool::new(true)),
            }),
            worker: Mutex::new(None),
            stopped: AtomicBool::new(false),
        }
    }
    pub fn configure(&self) {
        *self.control.pending.lock().unwrap() = Some(Config::read(&self.settings));
        self.control.wake.notify_all();
    }
    pub fn start(&self) -> Result<(), String> {
        let mut worker = self.worker.lock().unwrap();
        if self.stopped.load(Ordering::Acquire) {
            return Err("OCR service has shut down".into());
        }
        if worker.as_ref().is_some_and(|worker| !worker.is_finished()) {
            return Ok(());
        }
        if let Some(previous) = worker.take() {
            let _ = previous.join();
        }
        let config = Config::read(&self.settings);
        if model_spec(&config.language).is_none() {
            return Err("OCR_DISABLED_UNSUPPORTED_LANGUAGE".into());
        }
        self.control.cancelled.store(false, Ordering::Release);
        *self.control.pending.lock().unwrap() = None;
        let control = self.control.clone();
        let callback = self.callback.clone();
        let error = self.error.clone();
        let factory = self.factory.clone();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = thread::Builder::new().name("vrct-ocr".into()).spawn(move || {
            let mut scanner = match factory(&config,control.cancelled.clone()) {
                Ok(scanner) => scanner,
                Err(message) => { let _ = ready_tx.send(Err(message)); return; }
            };
            if control.cancelled.load(Ordering::Acquire) { let _ = ready_tx.send(Err("OCR start cancelled".into())); return; }
            let _ = ready_tx.send(Ok(()));
            let mut config = config;
            let mut cache = Dedup::default();
            let started = Instant::now();
            let mut last_tick = Duration::ZERO;
            while !control.cancelled.load(Ordering::Acquire) {
                let pending = control.pending.lock().unwrap().take();
                if let Some(next) = pending {
                    match scanner.configure(&next) {
                        Ok(()) => {
                            if next.language != config.language { cache = Dedup::default(); }
                            config = next;
                        }
                        Err(message) => { error(message); break; }
                    }
                }
                let tick = Instant::now();
                let now = started.elapsed();
                cache.evict(now, Duration::from_secs(30).max((now-last_tick).saturating_mul(2)));
                last_tick = now;
                match scanner.scan(&config,&control.cancelled) {
                    Ok(texts) => for text in texts {
                        let text = text.trim();
                        if control.cancelled.load(Ordering::Acquire) { break; }
                        if text.chars().count() < config.min_length || cache.seen(text,now) { continue; }
                        static ID: AtomicU64 = AtomicU64::new(0);
                        let language = (config.language != "auto").then_some(config.language.clone());
                        callback(json!({"text":text,"language":language,"is_final":true,"segment_id":format!("ocr-{}-{}",std::process::id(),ID.fetch_add(1,Ordering::Relaxed)),"recognition_error":false,"source":"ocr"}));
                    },
                    Err(message) => { error(message); break; }
                }
                let sleep = config.interval.saturating_sub(tick.elapsed());
                let pending = control.pending.lock().unwrap();
                if !control.cancelled.load(Ordering::Acquire) && pending.is_none() && !sleep.is_zero() {
                    let _ = control.wake.wait_timeout(pending,sleep);
                }
            }
            // Drop native resources on their creating thread.
            drop(scanner);
        }).map_err(|e| e.to_string())?;
        match ready_rx.recv() {
            Ok(Ok(())) => {
                *worker = Some(thread);
                Ok(())
            }
            result => {
                self.control.cancelled.store(true, Ordering::Release);
                let _ = thread.join();
                Err(match result {
                    Ok(Err(error)) => error,
                    _ => "OCR worker failed to start".into(),
                })
            }
        }
    }
    pub fn stop(&self) {
        self.control.cancelled.store(true, Ordering::Release);
        self.control.wake.notify_all();
        if let Some(worker) = self.worker.lock().unwrap().take() {
            let _ = worker.join();
        }
    }
    pub fn shutdown(&self) {
        self.stopped.store(true, Ordering::Release);
        self.stop();
    }
}
impl Drop for OcrService {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// (detector, recognizer). Legacy saved language names still resolve to auto.
pub fn model_spec(language: &str) -> Option<(&'static str, &'static str)> {
    let rec = match language {
        "Korean" => "korean_PP-OCRv5_rec_mobile",
        "Russian" | "Ukrainian" => "cyrillic_PP-OCRv5_rec_mobile",
        "Thai" => "th_PP-OCRv5_rec_mobile",
        "Arabic" => "arabic_PP-OCRv5_rec_mobile",
        "Hindi" => "devanagari_PP-OCRv5_rec_mobile",
        "auto"
        | "Japanese"
        | "English"
        | "Chinese Simplified"
        | "Chinese Traditional"
        | "French"
        | "German"
        | "Spanish"
        | "Italian"
        | "Portuguese"
        | "Dutch"
        | "Polish"
        | "Turkish"
        | "Vietnamese"
        | "Indonesian" => return Some(("multi_PP-OCRv6_det_small", "multi_PP-OCRv6_rec_small")),
        _ => return None,
    };
    Some(("ch_PP-OCRv5_det_mobile", rec))
}

#[derive(Default)]
pub struct Dedup {
    items: VecDeque<(String, Duration)>,
}
impl Dedup {
    pub fn evict(&mut self, now: Duration, retention: Duration) {
        self.items
            .retain(|(_, seen)| now.saturating_sub(*seen) <= retention);
    }
    pub fn seen(&mut self, text: &str, now: Duration) -> bool {
        // Rust lowercase preserves Unicode scripts. Casefold's special expansions
        // are handled for the common ß ligature rather than ASCII-only matching.
        let folded = text.to_lowercase().replace('ß', "ss");
        if let Some(index) = self
            .items
            .iter()
            .position(|(old, _)| similar(&folded, old, 2))
        {
            let (old, _) = self.items.remove(index).unwrap();
            self.items.push_back((old, now));
            true
        } else {
            self.items.push_back((folded, now));
            while self.items.len() > 128 {
                self.items.pop_front();
            }
            false
        }
    }
}
pub fn similar(left: &str, right: &str, budget: usize) -> bool {
    if left == right {
        return true;
    }
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    if left.len().abs_diff(right.len()) > budget {
        return false;
    }
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    for (i, a) in left.iter().enumerate() {
        let mut current = vec![i + 1];
        for (j, b) in right.iter().enumerate() {
            current.push(
                (previous[j + 1] + 1)
                    .min(current[j] + 1)
                    .min(previous[j] + usize::from(a != b)),
            );
        }
        if current.iter().copied().min().unwrap_or(0) > budget {
            return false;
        }
        previous = current;
    }
    previous[right.len()] <= budget
}
#[derive(Clone, Debug)]
pub struct Line {
    pub text: String,
    pub confidence: f32,
    pub top: f32,
}
pub fn merge_lines(mut lines: Vec<Line>) -> String {
    lines.retain(|line| !line.text.trim().is_empty());
    lines.sort_by(|a, b| a.top.total_cmp(&b.top));
    let mut result = String::new();
    for line in lines {
        let text = line.text.trim();
        if let Some(last) = result.chars().last() {
            if "。．.！!？?…」』)）".contains(last) {
                result.push('\n');
            } else if is_latin(last) || text.chars().next().is_some_and(is_latin) {
                result.push(' ');
            }
        }
        result.push_str(text);
    }
    result
}
fn is_latin(char: char) -> bool {
    char.is_ascii_alphanumeric() || char == '\'' || char == '"'
}
