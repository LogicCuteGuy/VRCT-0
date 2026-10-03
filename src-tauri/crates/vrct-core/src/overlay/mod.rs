//! Native overlay images and a stoppable SteamVR worker. Settings, render work
//! and driver state are separate; failed SteamVR operations never block audio.
mod native;
pub mod render;
use crate::{
    pipeline::host::{LargeLog, SmallLog},
    transcription::native::Config,
};
use render::{Frame, Renderer};
use serde_json::Value;
use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    Small,
    Large,
}
impl Size {
    fn index(self) -> usize {
        match self {
            Self::Small => 0,
            Self::Large => 1,
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub position: [f32; 3],
    pub rotation: [f32; 3],
    pub display_duration: f32,
    pub fadeout_duration: f32,
    pub opacity: f32,
    pub width: f32,
    pub tracker: String,
}
impl Settings {
    pub fn read(config: &dyn Config, size: Size) -> Self {
        let value = config
            .get(match size {
                Size::Small => "OVERLAY_SMALL_LOG_SETTINGS",
                Size::Large => "OVERLAY_LARGE_LOG_SETTINGS",
            })
            .unwrap_or(Value::Null);
        let num = |key: &str, default: f32| {
            value
                .get(key)
                .and_then(Value::as_f64)
                .filter(|v| v.is_finite())
                .map_or(default, |v| v as f32)
        };
        Self {
            position: [num("x_pos", 0.), num("y_pos", 0.), num("z_pos", 0.)],
            rotation: [
                num("x_rotation", 0.),
                num("y_rotation", 0.),
                num("z_rotation", 0.),
            ],
            display_duration: num("display_duration", 5.).max(0.),
            fadeout_duration: num("fadeout_duration", 2.).max(0.),
            opacity: num("opacity", 1.).clamp(0., 1.),
            width: num("ui_scaling", 1.).clamp(0.01, 100.)
                * if size == Size::Large { 0.25 } else { 1. },
            tracker: value
                .get("tracker")
                .and_then(Value::as_str)
                .unwrap_or(if size == Size::Small {
                    "HMD"
                } else {
                    "LeftHand"
                })
                .to_owned(),
        }
    }
    pub fn alpha(&self, elapsed: f32) -> f32 {
        if self.fadeout_duration == 0. || elapsed <= self.display_duration {
            self.opacity
        } else {
            self.opacity
                * (1. - (elapsed - self.display_duration) / self.fadeout_duration).clamp(0., 1.)
        }
    }
}

pub trait Driver: Send {
    fn image(&mut self, size: Size, frame: &Frame) -> Result<(), String>;
    fn settings(&mut self, size: Size, settings: &Settings, alpha: f32) -> Result<(), String>;
    /// false indicates the SteamVR Quit event and requires releasing the lease.
    fn active(&mut self) -> Result<bool, String>;
}
pub trait DriverFactory: Send + Sync {
    fn connect(&self) -> Result<Box<dyn Driver>, String>;
}
pub type ErrorCallback = Arc<dyn Fn(String) + Send + Sync>;
struct Worker {
    stop: Arc<AtomicBool>,
    sender: SyncSender<(Size, Frame)>,
    thread: JoinHandle<()>,
}
pub struct Overlay {
    config: Arc<dyn Config>,
    renderer: Mutex<Renderer>,
    factory: Arc<dyn DriverFactory>,
    error: ErrorCallback,
    available: Arc<AtomicBool>,
    worker: Mutex<Option<Worker>>,
}
impl Overlay {
    pub fn new(
        fonts: impl AsRef<Path>,
        config: Arc<dyn Config>,
        error: ErrorCallback,
    ) -> Result<Arc<Self>, String> {
        Self::with_factory(
            Renderer::load(fonts)?,
            config,
            Arc::new(native::Factory),
            error,
        )
    }
    pub fn with_factory(
        renderer: Renderer,
        config: Arc<dyn Config>,
        factory: Arc<dyn DriverFactory>,
        error: ErrorCallback,
    ) -> Result<Arc<Self>, String> {
        Ok(Arc::new(Self {
            config,
            renderer: Mutex::new(renderer),
            factory,
            error,
            available: Arc::new(AtomicBool::new(false)),
            worker: Mutex::new(None),
        }))
    }
    pub fn start(&self) -> Result<(), String> {
        let mut worker = self.worker.lock().unwrap_or_else(|p| p.into_inner());
        if worker.is_some() {
            return Ok(());
        }
        let (sender, receiver) = mpsc::sync_channel(4);
        let stop = Arc::new(AtomicBool::new(false));
        let task_stop = stop.clone();
        let config = self.config.clone();
        let factory = self.factory.clone();
        let available = self.available.clone();
        let error = self.error.clone();
        let thread = thread::Builder::new()
            .name("vrct-overlay".into())
            .spawn(move || run(receiver, task_stop, config, factory, available, error))
            .map_err(|e| e.to_string())?;
        *worker = Some(Worker {
            stop,
            sender,
            thread,
        });
        Ok(())
    }
    pub fn shutdown(&self) -> Result<(), String> {
        let mut guard = self.worker.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(worker) = guard.take() {
            worker.stop.store(true, Ordering::Release);
            drop(worker.sender);
            let deadline = Instant::now() + Duration::from_secs(5);
            while !worker.thread.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(5));
            }
            if !worker.thread.is_finished() {
                self.available.store(false, Ordering::Release);
                return Err("Overlay shutdown timed out; old worker retains its OpenVR lease until it returns".into());
            }
            worker
                .thread
                .join()
                .map_err(|_| "Overlay worker panicked".to_owned())?;
        }
        self.available.store(false, Ordering::Release);
        Ok(())
    }
    pub fn available(&self) -> bool {
        self.available.load(Ordering::Acquire)
    }
    fn submit(&self, size: Size, frame: Frame) -> Result<(), String> {
        let worker = self.worker.lock().unwrap_or_else(|p| p.into_inner());
        let worker = worker.as_ref().ok_or("Overlay worker is not running")?;
        worker
            .sender
            .try_send((size, frame))
            .map_err(|e| format!("Overlay image queue: {e}"))
    }
    pub fn small_log(&self, log: &SmallLog<'_>) -> Result<(), String> {
        let frame = self
            .renderer
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .small(log)?;
        self.submit(Size::Small, frame)
    }
    pub fn large_log(&self, log: &LargeLog<'_>) -> Result<(), String> {
        let time = chrono::Local::now().format("%H:%M").to_string();
        let frame = self
            .renderer
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .large(log, &time)?;
        self.submit(Size::Large, frame)
    }
    pub fn clear_small(&self) -> Result<(), String> {
        self.submit(Size::Small, Frame::transparent())
    }
    pub fn clear_large(&self) -> Result<(), String> {
        self.renderer
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear_history();
        self.submit(Size::Large, Frame::transparent())
    }
    pub fn preview_small(&self, message: &str, language: &str) -> Result<(), String> {
        self.small_log(&SmallLog {
            message: Some(message),
            language: Some(language),
            translation: &[],
            your_languages: &Value::Null,
            transliteration_message: &[],
            transliteration_translation: &[],
        })
    }
    pub fn preview_large(&self, message: &str, language: &str) -> Result<(), String> {
        self.large_log(&LargeLog {
            direction: "send",
            message: Some(message),
            language: Some(language),
            translation: &[],
            languages: &Value::Null,
            transliteration_message: &[],
            transliteration_translation: &[],
        })
    }
}
impl Drop for Overlay {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            (self.error)(error);
        }
    }
}

fn run(
    receiver: Receiver<(Size, Frame)>,
    stop: Arc<AtomicBool>,
    config: Arc<dyn Config>,
    factory: Arc<dyn DriverFactory>,
    available: Arc<AtomicBool>,
    error: ErrorCallback,
) {
    let mut driver: Option<Box<dyn Driver>> = None;
    let mut last_error = String::new();
    let mut retry = Instant::now();
    let mut frames = [Frame::transparent(), Frame::transparent()];
    let mut changed = [true, true];
    let mut last_update = [Instant::now(), Instant::now()];
    while !stop.load(Ordering::Acquire) {
        match receiver.recv_timeout(Duration::from_millis(62)) {
            Ok((size, frame)) => {
                let index = size.index();
                frames[index] = frame;
                changed[index] = true;
                last_update[index] = Instant::now();
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        for (size, frame) in receiver.try_iter() {
            let index = size.index();
            frames[index] = frame;
            changed[index] = true;
            last_update[index] = Instant::now();
        }
        if stop.load(Ordering::Acquire) {
            break;
        }
        if driver.is_none() && Instant::now() >= retry {
            match factory.connect() {
                Ok(connected) => {
                    driver = Some(connected);
                    changed = [true, true];
                    last_error.clear();
                }
                Err(message) => {
                    if message != last_error {
                        error(message.clone());
                        last_error = message;
                    }
                    retry = Instant::now() + Duration::from_secs(1);
                }
            }
        }
        if let Some(current) = driver.as_mut() {
            let result = (|| {
                if !current.active()? {
                    return Err("SteamVR quit; overlay will reconnect".into());
                }
                for size in [Size::Small, Size::Large] {
                    let index = size.index();
                    let settings = Settings::read(config.as_ref(), size);
                    let enabled = config.get(match size {
                        Size::Small => "OVERLAY_SMALL_LOG",
                        Size::Large => "OVERLAY_LARGE_LOG",
                    }) == Some(Value::Bool(true));
                    if changed[index] {
                        current.image(size, &frames[index])?;
                        changed[index] = false;
                    }
                    current.settings(
                        size,
                        &settings,
                        if enabled {
                            settings.alpha(last_update[index].elapsed().as_secs_f32())
                        } else {
                            0.
                        },
                    )?;
                }
                Ok::<_, String>(())
            })();
            match result {
                Ok(()) => {
                    available.store(true, Ordering::Release);
                }
                Err(message) => {
                    available.store(false, Ordering::Release);
                    if message != last_error {
                        error(message.clone());
                        last_error = message;
                    }
                    driver = None;
                    retry = Instant::now() + Duration::from_secs(1);
                }
            }
        }
    }
    available.store(false, Ordering::Release);
    drop(driver);
}

pub fn transform(settings: &Settings) -> [[f32; 4]; 3] {
    let (position, rotation) = match settings.tracker.as_str() {
        "LeftHand" => ([-0.093, -0.031, 0.31], [-65., 165., 115.]),
        "RightHand" => ([0.093, -0.031, 0.31], [-65., -165., -115.]),
        _ => ([0., -0.4, -1.], [0., 0., 0.]),
    };
    let base = rotation_matrix(rotation);
    let local = rotation_matrix(settings.rotation);
    let mut out = [[0.; 4]; 3];
    for i in 0..3 {
        for j in 0..3 {
            out[i][j] = (0..3).map(|k| base[i][k] * local[k][j]).sum();
        }
        out[i][3] =
            position[i] + base[i][0] * settings.position[0] + base[i][1] * settings.position[1]
                - base[i][2] * settings.position[2];
    }
    out
}
fn rotation_matrix(rotation: [f32; 3]) -> [[f32; 3]; 3] {
    let (sx, cx) = rotation[0].to_radians().sin_cos();
    let (sy, cy) = rotation[1].to_radians().sin_cos();
    let (sz, cz) = rotation[2].to_radians().sin_cos();
    [
        [cz * cy, cz * sy * sx - sz * cx, cz * sy * cx + sz * sx],
        [sz * cy, sz * sy * sx + cz * cx, sz * sy * cx - cz * sx],
        [-sy, cy * sx, cy * cx],
    ]
}
