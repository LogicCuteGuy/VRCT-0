//! The energy-threshold recorder: when a phrase starts and ends in the audio a device delivers.
//! A port of `Recognizer.listen_energy_and_audio` and `listen_energy_and_audio_in_background` from
//! `misyaguziya/custom_speech_recognition` (3.10.4.5), which VRCT's `BaseEnergyAndAudioRecorder` hands
//! phrase detection to, checked against that code run on scripted audio.
//!
//! Audio is read in chunks. Chunks are kept (the last `non_speaking_duration` of them) until one is louder
//! than the energy threshold; then every chunk is kept until `pause_threshold` seconds of quiet follow, or
//! until `phrase_time_limit` is reached. A phrase shorter than `phrase_threshold` is dropped and the
//! wait begins again. With `dynamic_energy_threshold` the threshold follows the quiet chunks it waits
//! through. Loudness is `audioop.rms`.
//!
//! The behaviour is the original's, quirks included: when the device has nothing to read yet the call
//! gives up (`WaitTimeout`) and whatever it had gathered is lost; the time spent waiting for a phrase
//! carries over between attempts within one call only; and once stopped, a recorder stays stopped.

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// What the recorder reads: fixed-size chunks of PCM.
pub trait Source {
    /// Frames (samples per channel) in a chunk.
    fn chunk(&self) -> usize;
    fn sample_rate(&self) -> u32;
    /// Bytes per sample.
    fn sample_width(&self) -> u32;
    /// Whether a read would return without waiting (`get_read_available() != 0`).
    fn available(&mut self) -> bool;
    /// The next chunk; an empty one is the end of the stream.
    fn read(&mut self) -> io::Result<Vec<u8>>;
}

/// Time as the recorder sees it: `record_timeout` is measured on this, and it sleeps on it.
pub trait Clock {
    /// Seconds on any scale that only moves forward.
    fn now(&self) -> f64;
    fn sleep(&self, seconds: f64);
}

pub struct SystemClock {
    start: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        SystemClock { start: Instant::now() }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    fn sleep(&self, seconds: f64) {
        std::thread::sleep(Duration::from_secs_f64(seconds));
    }
}

/// The `Recognizer` attributes the recorder reads (and `energy_threshold`, which it also changes).
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Minimum chunk energy to count as speech.
    pub energy_threshold: f64,
    pub dynamic_energy_threshold: bool,
    pub dynamic_energy_adjustment_damping: f64,
    pub dynamic_energy_ratio: f64,
    /// Seconds of quiet that end a phrase.
    pub pause_threshold: f64,
    /// Minimum seconds of speech for a phrase to count (filters clicks and pops).
    pub phrase_threshold: f64,
    /// Seconds of quiet kept on both sides of a phrase.
    pub non_speaking_duration: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            energy_threshold: 300.0,
            dynamic_energy_threshold: true,
            dynamic_energy_adjustment_damping: 0.15,
            dynamic_energy_ratio: 1.5,
            pause_threshold: 0.8,
            phrase_threshold: 0.3,
            non_speaking_duration: 0.5,
        }
    }
}

/// Why `listen` returned no phrase.
#[derive(Debug)]
pub enum ListenError {
    /// No phrase started in time, the device had nothing to read, or `record_timeout` passed.
    WaitTimeout,
    /// The recorder was stopped.
    Terminated,
    /// Reading the device failed.
    Io(io::Error),
    /// `pause_threshold >= non_speaking_duration >= 0` does not hold.
    Invalid,
}

/// Stops, pauses and resumes a background listener; clones share the state.
#[derive(Debug, Clone)]
pub struct Control {
    running: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    terminated: Arc<AtomicBool>,
}

impl Control {
    pub fn new() -> Self {
        Control { running: Arc::new(AtomicBool::new(true)), paused: Arc::new(AtomicBool::new(false)), terminated: Arc::new(AtomicBool::new(false)) }
    }

    /// Asks the listener to stop; a blocked read ends it on its next chunk. It cannot be restarted.
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        self.terminated.store(true, Ordering::SeqCst);
    }

    pub fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
    }

    pub fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    fn is_terminated(&self) -> bool {
        self.terminated.load(Ordering::SeqCst)
    }
}

impl Default for Control {
    fn default() -> Self {
        Self::new()
    }
}

/// `audioop.rms`: the square root of the mean square, truncated.
pub fn rms(data: &[u8], width: u32) -> u32 {
    let width = width as usize;
    let samples = data.len() / width;
    if samples == 0 {
        return 0;
    }
    let mut sum_squares = 0.0f64;
    for chunk in data.chunks_exact(width) {
        let value = match width {
            1 => f64::from(chunk[0] as i8),
            2 => f64::from(i16::from_le_bytes([chunk[0], chunk[1]])),
            3 => f64::from((i32::from(chunk[2] as i8) << 16) | (i32::from(chunk[1]) << 8) | i32::from(chunk[0])),
            _ => f64::from(i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])),
        };
        sum_squares += value * value;
    }
    (sum_squares / samples as f64).sqrt() as u32
}

/// `listen_energy_and_audio`: one phrase from `source`.
///
/// `timeout` is the longest wait for a phrase to start and `phrase_time_limit` the longest a phrase may
/// run (`None` or zero: no limit); `record_timeout` bounds how long recording a phrase may take on `clock`.
/// `energy` sees the loudness of every chunk read.
#[allow(clippy::too_many_arguments)]
pub fn listen(
    settings: &mut Settings,
    source: &mut dyn Source,
    clock: &dyn Clock,
    control: &Control,
    timeout: Option<f64>,
    phrase_time_limit: Option<f64>,
    record_timeout: f64,
    energy: &mut dyn FnMut(u32),
) -> Result<Vec<u8>, ListenError> {
    if !(settings.pause_threshold >= settings.non_speaking_duration && settings.non_speaking_duration >= 0.0) {
        return Err(ListenError::Invalid);
    }
    let width = source.sample_width();
    let seconds_per_buffer = source.chunk() as f64 / f64::from(source.sample_rate());
    let pause_buffers = (settings.pause_threshold / seconds_per_buffer).ceil() as i64;
    let phrase_buffers = (settings.phrase_threshold / seconds_per_buffer).ceil() as i64;
    let non_speaking_buffers = (settings.non_speaking_duration / seconds_per_buffer).ceil() as usize;
    let timeout = timeout.filter(|seconds| *seconds != 0.0);
    let phrase_time_limit = phrase_time_limit.filter(|seconds| *seconds != 0.0);

    let mut elapsed = 0.0f64;
    let mut buffer: Vec<u8>;
    let mut frames: VecDeque<Vec<u8>>;
    let mut pause_count: i64;
    loop {
        frames = VecDeque::new();

        // Store audio until the phrase starts.
        loop {
            elapsed += seconds_per_buffer;
            if timeout.is_some_and(|limit| elapsed > limit) {
                return Err(ListenError::WaitTimeout);
            }
            if source.available() {
                buffer = source.read().map_err(ListenError::Io)?;
            } else {
                clock.sleep(0.01);
                return Err(ListenError::WaitTimeout);
            }
            if control.is_terminated() {
                return Err(ListenError::Terminated);
            }
            let loudness = rms(&buffer, width);
            energy(loudness);
            if buffer.is_empty() {
                break;
            }
            frames.push_back(buffer.clone());
            if frames.len() > non_speaking_buffers {
                frames.pop_front();
            }
            if f64::from(loudness) > settings.energy_threshold {
                break;
            }
            if settings.dynamic_energy_threshold {
                let damping = settings.dynamic_energy_adjustment_damping.powf(seconds_per_buffer);
                let target_energy = f64::from(loudness) * settings.dynamic_energy_ratio;
                settings.energy_threshold = settings.energy_threshold * damping + target_energy * (1.0 - damping);
            }
        }

        // Read audio until the phrase ends.
        let record_start = clock.now();
        pause_count = 0;
        let mut phrase_count: i64 = 0;
        let phrase_start = elapsed;
        loop {
            if clock.now() - record_start > record_timeout {
                return Err(ListenError::WaitTimeout);
            }
            if control.is_terminated() {
                return Err(ListenError::Terminated);
            }
            elapsed += seconds_per_buffer;
            if phrase_time_limit.is_some_and(|limit| elapsed - phrase_start > limit) {
                break;
            }
            buffer = source.read().map_err(ListenError::Io)?;
            let loudness = rms(&buffer, width);
            energy(loudness);
            if buffer.is_empty() {
                break;
            }
            frames.push_back(buffer.clone());
            phrase_count += 1;
            if f64::from(loudness) > settings.energy_threshold {
                pause_count = 0;
            } else {
                pause_count += 1;
            }
            if pause_count > pause_buffers {
                break;
            }
        }

        // A phrase this short is a click: wait for the next one. The end of the stream stops the wait.
        phrase_count -= pause_count;
        if phrase_count >= phrase_buffers || buffer.is_empty() {
            break;
        }
    }

    // The quiet at the end is kept only up to `non_speaking_duration`.
    for _ in 0..(pause_count - non_speaking_buffers as i64).max(0) {
        frames.pop_back();
    }
    Ok(frames.into_iter().flatten().collect())
}

/// A phrase `listen` found (`AudioData`).
#[derive(Debug, Clone, PartialEq)]
pub struct Phrase {
    pub data: Vec<u8>,
    pub sample_rate: u32,
    pub sample_width: u32,
}

impl Phrase {
    /// `AudioData.get_raw_data()`, which is what VRCT queues: signed samples. 8-bit PCM is unsigned, so
    /// 128 is taken off every byte; other widths are as read.
    pub fn raw_data(&self) -> Vec<u8> {
        if self.sample_width == 1 {
            self.data.iter().map(|byte| byte.wrapping_sub(128)).collect()
        } else {
            self.data.clone()
        }
    }
}

/// How the background loop calls `listen`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Timing {
    /// The wait for a phrase to start before trying again (`phrase_timeout`, 1 s in VRCT).
    pub phrase_timeout: Option<f64>,
    pub phrase_time_limit: Option<f64>,
    pub record_timeout: f64,
}

/// VRCT's reading of its `record_timeout` setting: zero or less means no limit.
pub fn record_timeout(seconds: f64) -> f64 {
    if seconds > 0.0 {
        seconds
    } else {
        f64::INFINITY
    }
}

/// `listen_energy_and_audio_in_background`'s thread body: listens until `control` stops it, handing every
/// phrase to `on_phrase` (with the settings as they are then). A failed read ends it and is returned, so
/// the caller can report the device as lost; a stop is `Ok`.
#[allow(clippy::too_many_arguments)]
pub fn run_listener(
    settings: &mut Settings,
    source: &mut dyn Source,
    clock: &dyn Clock,
    control: &Control,
    timing: Timing,
    energy: &mut dyn FnMut(u32),
    on_phrase: &mut dyn FnMut(&Settings, Phrase),
) -> Result<(), io::Error> {
    while control.is_running() {
        match listen(settings, source, clock, control, timing.phrase_timeout, timing.phrase_time_limit, timing.record_timeout, energy) {
            Err(ListenError::WaitTimeout) => {}
            Err(ListenError::Terminated | ListenError::Invalid) => break,
            Err(ListenError::Io(error)) => return Err(error),
            Ok(data) => {
                if control.is_running() {
                    on_phrase(settings, Phrase { data, sample_rate: source.sample_rate(), sample_width: source.sample_width() });
                }
            }
        }
        while control.is_paused() {
            clock.sleep(0.1);
        }
    }
    Ok(())
}
