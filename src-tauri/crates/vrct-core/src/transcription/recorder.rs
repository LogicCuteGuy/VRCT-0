//! Recorders: one per open microphone or speaker, pushing what it hears onto queues.
//! A port of `BaseEnergyAndAudioRecorder` and `BaseVadAndAudioRecorder` in
//! `models/transcription/transcription_recorder.py`.
//!
//! * [`EnergyRecorder`] reads the device in its own format and lets `energy` decide where a phrase starts
//!   and ends; each phrase is queued as one chunk.
//! * [`VadRecorder`] runs the Silero segmenter over 16 kHz mono audio; each speech segment is queued
//!   with the reason it ended.
//!
//! Both also report the loudness of every chunk they read (for the volume meter), and both latch the
//! first thing that goes wrong with the device in a [`DeviceError`] that the session polls: a
//! recorder's own threads have nobody to return an error to.

use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use super::energy::{self, Control, Source, SystemClock, Timing};
use super::failure::{ErrorCode, Failure};
use super::phrases::{Chunk, Format, Stamp};
use super::queue::Queue;
use crate::audio::pipeline::{Event, SegmentWorker};
use crate::audio::vad::{FrameProbability, VadSegmenter};

/// Phrases waiting for recognition.
pub type AudioQueue = Queue<Chunk>;
/// Loudness values (`audioop.rms`) waiting for the meter; the session keeps only the latest.
pub type EnergyQueue = Queue<u32>;

/// The first thing that went wrong with a device while recording.
///
/// `info` stays once set; `event` is the flag the session clears after it has handled the error. A
/// recorder that was asked to stop reports nothing: reading from a stream being closed fails by design.
#[derive(Clone)]
pub struct DeviceError {
    shared: Arc<DeviceErrorShared>,
}

struct DeviceErrorShared {
    stop_requested: AtomicBool,
    state: Mutex<DeviceErrorState>,
}

#[derive(Default)]
struct DeviceErrorState {
    event: bool,
    info: Option<Failure>,
}

impl DeviceError {
    pub fn new() -> Self {
        DeviceError { shared: Arc::new(DeviceErrorShared { stop_requested: AtomicBool::new(false), state: Mutex::new(DeviceErrorState::default()) }) }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, DeviceErrorState> {
        self.shared.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Notes a failure, unless the recorder is being stopped or an earlier one has not been handled yet.
    pub fn set(&self, code: ErrorCode, stage: &str, source: &str, exception_type: &str) {
        if self.shared.stop_requested.load(Ordering::SeqCst) {
            return;
        }
        let mut state = self.state();
        if state.event {
            return;
        }
        state.info = Some(Failure::new(code, stage, source, Some(exception_type)));
        state.event = true;
    }

    /// An unhandled failure is waiting.
    pub fn is_set(&self) -> bool {
        self.state().event
    }

    /// Marks the failure handled; `info` stays.
    pub fn clear(&self) {
        self.state().event = false;
    }

    pub fn info(&self) -> Option<Failure> {
        self.state().info.clone()
    }

    /// From now on nothing is reported (the recorder is stopping).
    pub fn stop_requested(&self) {
        self.shared.stop_requested.store(true, Ordering::SeqCst);
    }
}

impl Default for DeviceError {
    fn default() -> Self {
        Self::new()
    }
}

/// The recorder could not start listening. The reason is in its [`DeviceError`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordError(pub String);

/// What a session needs of a recorder.
pub trait Recorder: Send + Sync {
    /// The format of the audio it queues.
    fn format(&self) -> Format;
    /// Starts listening in the background (`recordIntoQueue`). `energy` is given only when somebody watches the meter.
    fn record_into(&self, audio: AudioQueue, energy: Option<EnergyQueue>) -> Result<(), RecordError>;
    /// Stops reading the device; `resume` goes on.
    fn pause(&self);
    fn resume(&self);
    /// Stops for good and waits for the listener to finish. May be called more than once.
    fn stop(&self);
    /// `record_into` has succeeded: there is a listener to stop.
    fn is_listening(&self) -> bool;
    fn device_error(&self) -> &DeviceError;
}

// ---- the energy-threshold recorder --------------------------------------------------------------------

/// What the settings give the energy-threshold recorder.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnergyParams {
    pub energy_threshold: f64,
    pub dynamic_energy_threshold: bool,
    /// A phrase is cut after this many seconds (`phrase_time_limit`). Zero means no limit.
    pub phrase_time_limit: f64,
    /// Seconds of reading with nothing to show before the wait is restarted. Zero or less means no limit.
    pub record_timeout: f64,
}

pub struct EnergyRecorder {
    label: &'static str,
    format: Format,
    params: EnergyParams,
    /// Taken by `record_into`: the listener thread owns the device from then on, and closes it when it ends.
    source: Mutex<Option<Box<dyn Source + Send>>>,
    /// Makes a read that is waiting for data return (the device went quiet, as a loopback does).
    unblock: Box<dyn Fn() + Send + Sync>,
    control: Control,
    errors: DeviceError,
    listener: Mutex<Option<JoinHandle<()>>>,
    listening: AtomicBool,
    dropped: Arc<AtomicUsize>,
}

impl EnergyRecorder {
    /// `label` is `mic` or `speaker`. `channels` is what the device delivers per frame; the audio is queued as it is.
    pub fn new(
        label: &'static str,
        source: Box<dyn Source + Send>,
        channels: u32,
        params: EnergyParams,
        unblock: Box<dyn Fn() + Send + Sync>,
    ) -> Self {
        let format = Format { sample_rate: source.sample_rate(), sample_width: source.sample_width(), channels };
        EnergyRecorder {
            label,
            format,
            params,
            source: Mutex::new(Some(source)),
            unblock,
            control: Control::new(),
            errors: DeviceError::new(),
            listener: Mutex::new(None),
            listening: AtomicBool::new(false),
            dropped: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Phrases that were dropped because recognition was too far behind.
    pub fn dropped_phrases(&self) -> usize {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl Recorder for EnergyRecorder {
    fn format(&self) -> Format {
        self.format
    }

    fn record_into(&self, audio: AudioQueue, energy_queue: Option<EnergyQueue>) -> Result<(), RecordError> {
        let Some(mut source) = self.source.lock().unwrap_or_else(|p| p.into_inner()).take() else {
            return Err(RecordError("the recorder is already listening".to_string()));
        };
        let params = self.params;
        let mut settings = energy::Settings {
            energy_threshold: params.energy_threshold,
            dynamic_energy_threshold: params.dynamic_energy_threshold,
            ..energy::Settings::default()
        };
        // `phrase_timeout=1`: the wait for a phrase to start is retried every second so a stop is noticed.
        let timing = Timing { phrase_timeout: Some(1.0), phrase_time_limit: Some(params.phrase_time_limit), record_timeout: energy::record_timeout(params.record_timeout) };
        let control = self.control.clone();
        let errors = self.errors.clone();
        let label = self.label;
        let dropped = Arc::clone(&self.dropped);

        let thread = std::thread::Builder::new()
            .name(format!("vrct-{label}-energy"))
            .spawn(move || {
                let clock = SystemClock::new();
                let mut on_energy = |value: u32| {
                    if let Some(queue) = &energy_queue {
                        queue.put_dropping_oldest(value);
                    }
                };
                let mut on_phrase = |_: &energy::Settings, phrase: energy::Phrase| {
                    let chunk = Chunk { data: phrase.raw_data(), at: Stamp::now(), end: None };
                    if audio.put_dropping_oldest(chunk) {
                        dropped.fetch_add(1, Ordering::Relaxed);
                    }
                };
                let outcome = energy::run_listener(&mut settings, &mut *source, &clock, &control, timing, &mut on_energy, &mut on_phrase);
                if let Err(error) = outcome {
                    errors.set(ErrorCode::AudioReadError, "recording", label, exception_type(&error));
                }
                // `source` goes out of scope here: the device is closed.
            })
            .map_err(|error| {
                self.errors.set(ErrorCode::AudioReadError, "recording", self.label, "RuntimeError");
                RecordError(format!("cannot start the listener thread: {error}"))
            })?;
        *self.listener.lock().unwrap_or_else(|p| p.into_inner()) = Some(thread);
        self.listening.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn pause(&self) {
        self.control.pause();
    }

    fn resume(&self) {
        self.control.resume();
    }

    fn stop(&self) {
        self.errors.stop_requested();
        self.control.stop();
        (self.unblock)();
        let thread = self.listener.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(thread) = thread {
            let _ = thread.join();
        }
    }

    fn is_listening(&self) -> bool {
        self.listening.load(Ordering::SeqCst)
    }

    fn device_error(&self) -> &DeviceError {
        &self.errors
    }
}

/// Python's name for the error a failed read raised.
fn exception_type(_error: &io::Error) -> &'static str {
    "OSError"
}

// ---- the VAD recorder ---------------------------------------------------------------------------------

/// A running capture: dropping or stopping it closes the device.
pub trait Capturing: Send {
    fn stop(&mut self);
}

pub type PcmSink = Box<dyn FnMut(&[u8]) + Send>;
pub type CaptureFailure = Box<dyn Fn(String) + Send + Sync>;

/// Opens a device and delivers 16 kHz mono PCM16 from it (on Windows, `audio::capture`).
pub trait CaptureFactory: Send + Sync {
    fn start(&self, sink: PcmSink, on_failure: CaptureFailure) -> Result<Box<dyn Capturing>, String>;
}

type Starter = Box<dyn FnOnce(Box<dyn FnMut(Event) + Send>) -> Result<SegmentWorker, String> + Send>;

pub struct VadRecorder {
    label: &'static str,
    capture: Arc<dyn CaptureFactory>,
    /// Holds the segmenter until `record_into` hands it to a worker thread.
    starter: Mutex<Option<Starter>>,
    running: Mutex<Option<(Box<dyn Capturing>, SegmentWorker)>>,
    paused: Arc<AtomicBool>,
    errors: DeviceError,
    listening: AtomicBool,
    dropped: Arc<AtomicUsize>,
}

impl VadRecorder {
    pub fn new<P>(label: &'static str, capture: Arc<dyn CaptureFactory>, segmenter: VadSegmenter<P>) -> Self
    where
        P: FrameProbability + Send + 'static,
    {
        let starter: Starter = Box::new(move |handler| SegmentWorker::spawn(segmenter, handler));
        VadRecorder {
            label,
            capture,
            starter: Mutex::new(Some(starter)),
            running: Mutex::new(None),
            paused: Arc::new(AtomicBool::new(false)),
            errors: DeviceError::new(),
            listening: AtomicBool::new(false),
            dropped: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Segments that were dropped because recognition was too far behind.
    pub fn dropped_segments(&self) -> usize {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl Recorder for VadRecorder {
    fn format(&self) -> Format {
        // What the segmenter returns, whatever the device delivers.
        Format { sample_rate: crate::audio::TARGET_SAMPLE_RATE, sample_width: 2, channels: 1 }
    }

    fn record_into(&self, audio: AudioQueue, energy_queue: Option<EnergyQueue>) -> Result<(), RecordError> {
        let Some(starter) = self.starter.lock().unwrap_or_else(|p| p.into_inner()).take() else {
            return Err(RecordError("the recorder is already listening".to_string()));
        };
        let label = self.label;
        let errors = self.errors.clone();
        let dropped = Arc::clone(&self.dropped);
        let handler = move |event: Event| match event {
            Event::Segment(segment) => {
                let chunk = Chunk { data: segment.audio, at: Stamp::now(), end: Some(segment.reason) };
                if audio.put_dropping_oldest(chunk) {
                    dropped.fetch_add(1, Ordering::Relaxed);
                }
            }
            Event::EngineError(_) => errors.set(ErrorCode::VadInferenceError, "vad", label, "RuntimeError"),
            Event::CaptureFailed(_) => errors.set(ErrorCode::AudioReadError, "recording", label, "OSError"),
        };
        let worker = starter(Box::new(handler)).map_err(|error| {
            self.errors.set(ErrorCode::AudioReadError, "recording", self.label, "RuntimeError");
            RecordError(error)
        })?;

        let (audio_feed, failure_feed) = (worker.feeder(), worker.feeder());
        let paused = Arc::clone(&self.paused);
        let sink: PcmSink = Box::new(move |pcm| {
            // A paused recorder does not read its device; whatever arrives meanwhile is let go.
            if paused.load(Ordering::SeqCst) {
                return;
            }
            if let Some(queue) = &energy_queue {
                queue.put_dropping_oldest(energy::rms(pcm, 2));
            }
            audio_feed.feed(pcm);
        });
        let on_failure: CaptureFailure = Box::new(move |error| failure_feed.capture_failed(error));
        match self.capture.start(sink, on_failure) {
            Ok(capture) => {
                *self.running.lock().unwrap_or_else(|p| p.into_inner()) = Some((capture, worker));
                self.listening.store(true, Ordering::SeqCst);
                Ok(())
            }
            Err(error) => {
                worker.finish();
                self.errors.set(ErrorCode::AudioOpenError, "recording", self.label, "OSError");
                Err(RecordError(error))
            }
        }
    }

    fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
    }

    fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
    }

    fn stop(&self) {
        self.errors.stop_requested();
        let running = self.running.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some((mut capture, worker)) = running {
            // The device first, so nothing new is queued; then the worker drains and flushes the open segment.
            capture.stop();
            worker.finish();
        }
    }

    fn is_listening(&self) -> bool {
        self.listening.load(Ordering::SeqCst)
    }

    fn device_error(&self) -> &DeviceError {
        &self.errors
    }
}
