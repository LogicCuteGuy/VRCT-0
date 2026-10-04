use crate::{
    base_directory, console,
    player::pad_string,
    run_id,
    source::{Backend, CaptureSource, CaptureUnavailable, Eye, Frame, Source},
    Random,
};
use clap::Parser;
use image::{
    codecs::png::{CompressionType, FilterType, PngEncoder},
    ImageEncoder,
};
use serde_json::json;
use std::{
    fs::{self, File},
    io::{self, Write},
    net::UdpSocket,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, SyncSender},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub fn session_name(value: &str) -> Result<String, String> {
    let prefix = value.split('.').next().unwrap_or_default().to_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&prefix.as_str())
        || ["COM", "LPT"].iter().any(|p| {
            prefix.strip_prefix(p).is_some_and(|n| {
                ["1", "2", "3", "4", "5", "6", "7", "8", "9", "¹", "²", "³"].contains(&n)
            })
        });
    if value.is_empty()
        || value.chars().count() > 80
        || [".", ".."].contains(&value)
        || value.chars().any(|c| c < ' ' || "<>:\"/\\|?*".contains(c))
        || value.ends_with(['.', ' '])
        || reserved
    {
        return Err("session_name must be a valid single folder name (1..80 characters)".into());
    }
    Ok(value.into())
}
pub fn next_deadline(previous: Duration, now: Duration, interval: Duration) -> Duration {
    let skipped = now.saturating_sub(previous).as_nanos() / interval.as_nanos().max(1) + 1;
    previous.saturating_add(interval.saturating_mul(skipped.min(u32::MAX as u128) as u32))
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Label {
    Positive,
    Negative,
    Unlabeled,
}
impl Label {
    pub fn name(self) -> &'static str {
        match self {
            Self::Positive => "positive",
            Self::Negative => "negative",
            Self::Unlabeled => "unlabeled",
        }
    }
}
#[derive(Debug)]
pub enum SaveError {
    Unavailable(CaptureUnavailable),
    Io(String),
}
impl From<io::Error> for SaveError {
    fn from(error: io::Error) -> Self {
        Self::Io(error.to_string())
    }
}
impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(e) => e.fmt(f),
            Self::Io(e) => e.fmt(f),
        }
    }
}
pub struct ImageStore {
    root: PathBuf,
    pub directory: PathBuf,
    pub session: String,
    pub run_id: String,
    pub count: usize,
}
impl ImageStore {
    pub fn new(root: &Path, session: &str) -> Result<Self, String> {
        let session = session_name(session)?;
        fs::create_dir_all(root).map_err(|e| e.to_string())?;
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        Ok(Self {
            directory: root.join(&session),
            root,
            session,
            run_id: run_id()?,
            count: 0,
        })
    }
    pub fn save(
        &mut self,
        frame: &Frame,
        label: Label,
        max_age: Duration,
    ) -> Result<PathBuf, SaveError> {
        self.save_with_publisher(frame, label, max_age, publish)
    }
    /// Injectable publisher exercises failures between pair publication steps.
    /// The PNG is the commit marker: consumers enumerate PNG only after its JSON
    /// has been fully published. Neither file is visible while being written.
    pub fn save_with_publisher(
        &mut self,
        frame: &Frame,
        label: Label,
        max_age: Duration,
        mut publisher: impl FnMut(&Path, &Path) -> io::Result<()>,
    ) -> Result<PathBuf, SaveError> {
        if Instant::now()
            .checked_duration_since(frame.captured_monotonic)
            .is_none_or(|age| age > max_age)
        {
            return Err(SaveError::Unavailable(CaptureUnavailable(
                "Frame took too long to acquire; not saved".into(),
            )));
        }
        if frame.rgb.width() == 0 || frame.rgb.height() == 0 {
            return Err(SaveError::Unavailable(CaptureUnavailable(
                "Empty frame; not saved".into(),
            )));
        }
        // Check every existing directory before creating children: a symlink or
        // Windows junction must not let a session escape the selected root.
        if self.directory.exists() && !self.directory.canonicalize()?.starts_with(&self.root) {
            return Err(SaveError::Io("Session path escapes output root".into()));
        }
        fs::create_dir_all(&self.directory)?;
        if !self.directory.canonicalize()?.starts_with(&self.root) {
            return Err(SaveError::Io("Session path escapes output root".into()));
        }
        let folder = self.directory.join(label.name());
        if folder.exists() && !folder.canonicalize()?.starts_with(&self.root) {
            return Err(SaveError::Io("Label path escapes output root".into()));
        }
        fs::create_dir_all(&folder)?;
        if !folder.canonicalize()?.starts_with(&self.root) {
            return Err(SaveError::Io("Label path escapes output root".into()));
        }
        let stem = format!("{}_{:06}", self.run_id, self.count);
        let png = folder.join(format!("{stem}.png"));
        let metadata = folder.join(format!("{stem}.json"));
        let png_tmp = folder.join(format!("{stem}.png.part"));
        let json_tmp = folder.join(format!("{stem}.json.part"));
        let mut record = frame
            .metadata
            .as_object()
            .cloned()
            .ok_or_else(|| SaveError::Io("Frame metadata must be an object".into()))?;
        record.extend(json!({"session":self.session,"run_id":self.run_id,"label":label.name(),"image":png.file_name().unwrap().to_string_lossy(),"width":frame.rgb.width(),"height":frame.rgb.height(),"png_compress_level":1}).as_object().unwrap().clone());
        let mut owned = Vec::new();
        let mut published = Vec::new();
        let result = (|| -> Result<(), SaveError> {
            let mut file = File::options()
                .write(true)
                .create_new(true)
                .open(&png_tmp)?;
            owned.push(png_tmp.clone());
            PngEncoder::new_with_quality(&mut file, CompressionType::Fast, FilterType::Adaptive)
                .write_image(
                    frame.rgb.as_raw(),
                    frame.rgb.width(),
                    frame.rgb.height(),
                    image::ExtendedColorType::Rgb8,
                )
                .map_err(|e| SaveError::Io(e.to_string()))?;
            file.flush()?;
            file.sync_all()?;
            drop(file);
            let mut file = File::options()
                .write(true)
                .create_new(true)
                .open(&json_tmp)?;
            owned.push(json_tmp.clone());
            serde_json::to_writer_pretty(&mut file, &record)
                .map_err(|e| SaveError::Io(e.to_string()))?;
            file.flush()?;
            file.sync_all()?;
            drop(file);
            if Instant::now()
                .checked_duration_since(frame.captured_monotonic)
                .is_none_or(|age| age > max_age)
            {
                return Err(SaveError::Unavailable(CaptureUnavailable(
                    "Frame became stale during persistence; not saved".into(),
                )));
            }
            publisher(&json_tmp, &metadata)?;
            published.push(metadata.clone());
            publisher(&png_tmp, &png)?;
            published.push(png.clone());
            Ok(())
        })();
        if result.is_err() {
            for path in owned.iter().chain(published.iter()) {
                let _ = fs::remove_file(path);
            }
        }
        result?;
        self.count += 1;
        Ok(png)
    }
}
fn publish(source: &Path, target: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};
        let source: Vec<u16> = source.as_os_str().encode_wide().chain([0]).collect();
        let target: Vec<u16> = target.as_os_str().encode_wide().chain([0]).collect();
        // No REPLACE_EXISTING flag. Collision cannot destroy another run's pair.
        if unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), MOVEFILE_WRITE_THROUGH) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        fs::hard_link(source, target)?;
        fs::remove_file(source)
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Command {
    Pause,
    Status,
    Save(Label),
}
#[derive(Clone)]
pub struct Options {
    pub interval: Duration,
    pub duration: Duration,
    pub manual: bool,
    pub max_frames: usize,
    pub max_age: Duration,
}
pub type Factory = Box<dyn FnOnce() -> Result<Box<dyn Source>, String> + Send>;
pub type Emit = Arc<dyn Fn(String) + Send + Sync>;
pub struct RunSummary {
    pub count: usize,
    pub directory: PathBuf,
}
pub struct Collector {
    commands: SyncSender<Command>,
    pub stopped: Arc<AtomicBool>,
    pub done: Arc<AtomicBool>,
    count: Arc<AtomicUsize>,
    thread: Option<JoinHandle<Result<RunSummary, String>>>,
    emit: Emit,
}
struct Finished(Arc<AtomicBool>);
impl Drop for Finished {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
impl Collector {
    pub fn start(
        factory: Factory,
        mut store: ImageStore,
        options: Options,
        emit: Emit,
    ) -> Result<Self, String> {
        let (commands, receiver) = mpsc::sync_channel(16);
        let stopped = Arc::new(AtomicBool::new(false));
        let stopping = stopped.clone();
        let done = Arc::new(AtomicBool::new(false));
        let finished = done.clone();
        let count = Arc::new(AtomicUsize::new(0));
        let saved = count.clone();
        let messages = emit.clone();
        let worker = thread::Builder::new()
            .name("dataset-capture".into())
            .spawn(move || {
                let _finished = Finished(finished);
                let result = (|| -> Result<RunSummary, String> {
                    let mut source = factory()?;
                    let run = (|| -> Result<(), String> {
                        let clock = Instant::now();
                        let mut deadline = Duration::ZERO;
                        let mut paused = false;
                        let mut warning = None;
                        while !stopping.load(Ordering::Acquire) {
                            let now = clock.elapsed();
                            if (!options.duration.is_zero() && now >= options.duration)
                                || (options.max_frames > 0 && store.count >= options.max_frames)
                            {
                                break;
                            }
                            let mut timeout = Duration::from_millis(100);
                            if !options.duration.is_zero() {
                                timeout = timeout.min(options.duration.saturating_sub(now));
                            }
                            if !options.manual && !paused {
                                timeout = timeout.min(deadline.saturating_sub(now));
                            }
                            let command = receiver.recv_timeout(timeout).ok();
                            let mut requested = None;
                            match command {
                                Some(Command::Pause) => {
                                    paused = !paused;
                                    deadline = clock.elapsed().saturating_add(options.interval);
                                    messages(
                                        if paused {
                                            "[paused] P to resume"
                                        } else {
                                            "[resumed]"
                                        }
                                        .into(),
                                    );
                                }
                                Some(Command::Status) => messages(format!(
                                    "[status] saved={}, paused={paused}",
                                    store.count
                                )),
                                Some(Command::Save(label)) if options.manual && !paused => {
                                    requested = Some(label)
                                }
                                _ => {}
                            }
                            let auto = !options.manual && !paused && clock.elapsed() >= deadline;
                            if auto {
                                requested = Some(Label::Unlabeled);
                            }
                            if let Some(label) = requested {
                                if stopping.load(Ordering::Acquire)
                                    || (!options.duration.is_zero()
                                        && clock.elapsed() >= options.duration)
                                {
                                    break;
                                }
                                match source.capture() {
                                    Ok(frame) => {
                                        if stopping.load(Ordering::Acquire)
                                            || (!options.duration.is_zero()
                                                && clock.elapsed() >= options.duration)
                                        {
                                            break;
                                        }
                                        match store.save(&frame, label, options.max_age) {
                                            Ok(path) => {
                                                warning = None;
                                                saved.store(store.count, Ordering::Release);
                                                messages(format!(
                                                    "[saved {}] {} (this run: {})",
                                                    label.name(),
                                                    path.display(),
                                                    store.count
                                                ));
                                            }
                                            Err(SaveError::Unavailable(error)) => warn_once(
                                                &messages,
                                                &mut warning,
                                                error.to_string(),
                                            ),
                                            Err(error) => return Err(error.to_string()),
                                        }
                                    }
                                    Err(error) => {
                                        warn_once(&messages, &mut warning, error.to_string())
                                    }
                                }
                                if auto {
                                    deadline =
                                        next_deadline(deadline, clock.elapsed(), options.interval);
                                }
                            }
                        }
                        Ok(())
                    })();
                    let cleanup = source.close();
                    drop(source);
                    run?;
                    cleanup?;
                    Ok(RunSummary {
                        count: store.count,
                        directory: store.directory,
                    })
                })();
                if let Err(error) = &result {
                    messages(format!("[ERROR] {error}"));
                }
                result
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            commands,
            stopped,
            done,
            count,
            thread: Some(worker),
            emit,
        })
    }
    pub fn command(&self, command: Command) {
        if self.commands.try_send(command).is_err() {
            (self.emit)("[WARN] Command queue is full or capture has finished".into());
        }
    }
    pub fn count(&self) -> usize {
        self.count.load(Ordering::Acquire)
    }
    pub fn stop(&mut self) -> Result<RunSummary, String> {
        self.stopped.store(true, Ordering::Release);
        let thread = self.thread.take().ok_or("Collector has already stopped")?;
        thread
            .join()
            .map_err(|_| "Capture worker panicked".to_string())?
    }
}
impl Drop for Collector {
    fn drop(&mut self) {
        if self.thread.is_some() {
            let _ = self.stop();
        }
    }
}
fn warn_once(emit: &Emit, previous: &mut Option<String>, message: String) {
    if previous.as_ref() != Some(&message) {
        emit(format!("[waiting] {message}"));
        *previous = Some(message);
    }
}

pub enum OscValue {
    Int(i32),
    Float(f32),
}
pub fn osc_message(address: &str, value: OscValue) -> Vec<u8> {
    let mut packet = Vec::new();
    pad_string(&mut packet, address);
    match value {
        OscValue::Int(value) => {
            pad_string(&mut packet, ",i");
            packet.extend(value.to_be_bytes());
        }
        OscValue::Float(value) => {
            pad_string(&mut packet, ",f");
            packet.extend(value.to_be_bytes());
        }
    }
    packet
}
pub type SendInput = Arc<dyn Fn(&str, OscValue) -> Result<(), String> + Send + Sync>;
pub struct Wanderer {
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<(), String>>>,
}
fn neutral(send: &SendInput) -> Result<(), String> {
    // Attempt every release even if one send fails.
    let results = [
        send("/input/Vertical", OscValue::Float(0.0)),
        send("/input/LookHorizontal", OscValue::Float(0.0)),
        send("/input/Jump", OscValue::Int(0)),
    ];
    results
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map(|_| ())
}
fn wait_interruptible(stop: &AtomicBool, paused: &AtomicBool, time: Duration) {
    let clock = Instant::now();
    while clock.elapsed() < time && !stop.load(Ordering::Acquire) && !paused.load(Ordering::Acquire)
    {
        thread::sleep(Duration::from_millis(20).min(time.saturating_sub(clock.elapsed())));
    }
}
impl Wanderer {
    pub fn start(port: u16) -> Result<Self, String> {
        if port == 0 {
            return Err("--osc-port must be 1..65535".into());
        }
        let socket = UdpSocket::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
        socket
            .set_write_timeout(Some(Duration::from_secs(1)))
            .map_err(|e| e.to_string())?;
        Self::with_sender(Arc::new(move |address, value| {
            let packet = osc_message(address, value);
            let sent = socket
                .send_to(&packet, (std::net::Ipv4Addr::LOCALHOST, port))
                .map_err(|e| e.to_string())?;
            if sent != packet.len() {
                return Err("Incomplete wander OSC datagram".into());
            }
            Ok(())
        }))
    }
    pub fn with_sender(send: SendInput) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let paused = Arc::new(AtomicBool::new(false));
        let pause = paused.clone();
        let thread = thread::Builder::new()
            .name("wander".into())
            .spawn(move || {
                let run = (|| -> Result<(), String> {
                    let mut rng = Random::new(None)?;
                    while !stopped.load(Ordering::Acquire) {
                        if pause.load(Ordering::Acquire) {
                            wait_interruptible(
                                &stopped,
                                &AtomicBool::new(false),
                                Duration::from_millis(100),
                            );
                            continue;
                        }
                        for (address, value, seconds) in [
                            (
                                "/input/Vertical",
                                OscValue::Float(1.0),
                                2.0 + 4.0 * rng.unit(),
                            ),
                            (
                                "/input/LookHorizontal",
                                OscValue::Float(if rng.unit() < 0.5 { -1.0 } else { 1.0 }),
                                0.2 + rng.unit(),
                            ),
                        ] {
                            if stopped.load(Ordering::Acquire) || pause.load(Ordering::Acquire) {
                                break;
                            }
                            send(address, value)?;
                            wait_interruptible(&stopped, &pause, Duration::from_secs_f64(seconds));
                            send(address, OscValue::Float(0.0))?;
                        }
                        if rng.unit() < 0.2
                            && !stopped.load(Ordering::Acquire)
                            && !pause.load(Ordering::Acquire)
                        {
                            send("/input/Jump", OscValue::Int(1))?;
                            wait_interruptible(&stopped, &pause, Duration::from_millis(100));
                            send("/input/Jump", OscValue::Int(0))?;
                        }
                    }
                    Ok(())
                })();
                let release = neutral(&send);
                run?;
                release
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            stop,
            paused,
            thread: Some(thread),
        })
    }
    pub fn toggle_pause(&self) {
        self.paused.fetch_xor(true, Ordering::AcqRel);
    }
    pub fn stop(&mut self) -> Result<(), String> {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| "Wander worker panicked".to_string())??;
        }
        Ok(())
    }
}
impl Drop for Wanderer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[derive(Parser, Debug)]
#[command(
    about = "Collect fresh VRChat-only PNG/JSON pairs locally. Automatic images are unlabeled."
)]
pub struct Args {
    #[arg(value_parser=session_name)]
    pub session: Option<String>,
    #[arg(long)]
    pub out: Option<PathBuf>,
    #[arg(long,value_enum,default_value_t=Backend::Auto)]
    pub backend: Backend,
    #[arg(long,value_enum,default_value_t=Eye::Left)]
    pub eye: Eye,
    #[arg(long, default_value_t = 2.0)]
    pub interval: f64,
    #[arg(long, default_value_t = 600.0)]
    pub duration: f64,
    #[arg(long, default_value_t = 0)]
    pub max_frames: usize,
    #[arg(long, default_value_t = 2.0)]
    pub max_age: f64,
    #[arg(long)]
    pub manual: bool,
    #[arg(long)]
    pub wander: bool,
    #[arg(long, default_value_t = 9000)]
    pub osc_port: u16,
}
impl Args {
    pub fn options(&self) -> Result<Options, String> {
        if !self.interval.is_finite() || self.interval < 0.1 {
            return Err("--interval must be finite and at least 0.1".into());
        }
        if !self.duration.is_finite() || self.duration < 0.0 {
            return Err("--duration must be finite and nonnegative".into());
        }
        if !self.max_age.is_finite() || self.max_age <= 0.0 {
            return Err("--max-age must be finite and positive".into());
        }
        if self.osc_port == 0 {
            return Err("--osc-port must be 1..65535".into());
        }
        let duration = |value| {
            Duration::try_from_secs_f64(value).map_err(|e| format!("Time out of range: {e}"))
        };
        Ok(Options {
            interval: duration(self.interval)?,
            duration: duration(self.duration)?,
            max_age: duration(self.max_age)?,
            manual: self.manual,
            max_frames: self.max_frames,
        })
    }
}
pub fn run(args: Args) -> Result<(), String> {
    let options = args.options()?;
    if !cfg!(all(windows, target_arch = "x86_64")) {
        return Err("This capture tool requires Windows x64".into());
    }
    let stop = console::interrupt_flag()?;
    let session = args.session.unwrap_or_else(|| {
        chrono::Local::now()
            .format("session_%Y%m%d_%H%M%S")
            .to_string()
    });
    let store = ImageStore::new(
        &args
            .out
            .unwrap_or_else(|| base_directory().join("dataset_collected")),
        &session,
    )?;
    println!("session={session}, backend={:?}, interval={}s, duration={}s\noutput={}\nP=pause/resume  Q=quit  R=status (console focused)",args.backend,args.interval,args.duration,store.directory.display());
    println!(
        "{}",
        if args.manual {
            "Enter=positive  N=negative  U=unlabeled"
        } else {
            "Automatic capture -> unlabeled/"
        }
    );
    let mut wanderer = if args.wander {
        Some(Wanderer::start(args.osc_port)?)
    } else {
        None
    };
    let mut collector = Collector::start(
        Box::new(move || Ok(Box::new(CaptureSource::new(args.backend, args.eye)))),
        store,
        options,
        Arc::new(|message| println!("{message}")),
    )?;
    while !collector.done.load(Ordering::Acquire) && !stop.load(Ordering::Acquire) {
        if let Some(key) = console::read_key() {
            match key {
                'q' => {
                    collector.stopped.store(true, Ordering::Release);
                    break;
                }
                'p' => {
                    collector.command(Command::Pause);
                    if let Some(wanderer) = &wanderer {
                        wanderer.toggle_pause();
                    }
                }
                'r' => collector.command(Command::Status),
                '\r' | '\n' => collector.command(Command::Save(Label::Positive)),
                'n' => collector.command(Command::Save(Label::Negative)),
                'u' => collector.command(Command::Save(Label::Unlabeled)),
                _ => {}
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    collector.stopped.store(true, Ordering::Release);
    let walking = if let Some(wanderer) = &mut wanderer {
        wanderer.stop()
    } else {
        Ok(())
    };
    let summary = collector.stop()?;
    walking?;
    println!(
        "done: saved={} in this run, output={}",
        summary.count,
        summary.directory.display()
    );
    Ok(())
}
