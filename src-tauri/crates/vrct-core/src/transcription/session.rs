//! One microphone or one speaker: a recorder, a transcriber and the threads between them, started and
//! stopped together. A port of `_AudioDeviceSession` (and the `MicSession` / `SpeakerSession` hooks) in
//! `model.py`.
//!
//! A session holds the union of what is wanted from its device: a transcript, a volume meter, or both.
//! It keeps a single recorder for that union, so a device is never opened twice, and starts it over
//! when the wanted set or the device changes ([`AudioSession::reconfigure`]).
//!
//! Three kinds of thread run while it listens: the recorder's own, one that turns queued audio into
//! transcripts and hands them out, and one that hands out the meter's values. When recognition or the
//! device fails, the session stops itself once and tells the UI once ([`Failure`]); the thread that
//! noticed cannot join itself, so the stopping is done on another thread.
//!
//! Everything outside the session (which device the settings pick, how a recorder is built, which
//! engine recognises) is a [`Backend`], so the lifecycle can be tested without a device.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle, ThreadId};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::failure::{ErrorCode, Failure};
use super::phrases::{AsrFailure, Format, PhraseTranscriber, Query, Recognizer, Stamp, Transcript};
use super::queue::{Queue, AUDIO_QUEUE_SIZE};
use super::recorder::{AudioQueue, EnergyQueue, Recorder};
use crate::audio::devices::{Device, NO_DEVICE};

/// How long stopping waits for each thread (`TRANSCRIPT_STOP_JOIN_TIMEOUT`).
pub const STOP_JOIN_TIMEOUT: Duration = Duration::from_secs(15);
/// The pause between two looks at the queues.
const POLL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Mic,
    Speaker,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Mic => "mic",
            Kind::Speaker => "speaker",
        }
    }

    /// For the log (`self._kind.capitalize()`).
    fn label(self) -> &'static str {
        match self {
            Kind::Mic => "Mic",
            Kind::Speaker => "Speaker",
        }
    }
}

/// What the transcript callback is given.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// A recognised phrase; `recognition_error` says the last recognition before it had failed.
    Transcript { transcript: Transcript, recognition_error: bool },
    /// There is no device to listen to.
    NoDevice,
    /// The session has failed and stopped.
    Failure(Failure),
}

impl Message {
    /// The dictionary the UI pipeline receives.
    pub fn to_value(&self) -> Value {
        match self {
            Message::Transcript { transcript, recognition_error } => {
                let mut value = json!({
                    "confidence": transcript.confidence,
                    "text": transcript.text,
                    "language": transcript.language,
                    "recognition_error": recognition_error,
                });
                if let Some(ms) = transcript.asr_ms {
                    value["asr_ms"] = json!(ms);
                }
                value
            }
            Message::NoDevice => json!({"text": false, "language": null}),
            Message::Failure(failure) => failure.notification(),
        }
    }
}

/// What the meter callback is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// The loudness of the latest chunk (`audioop.rms`).
    Value(u32),
    /// There is no device to measure.
    NoDevice,
}

pub type OnMessage = Arc<dyn Fn(Message) + Send + Sync>;
pub type OnLevel = Arc<dyn Fn(Level) + Send + Sync>;
/// Where the session writes its notes.
pub type OnNote = Arc<dyn Fn(&str) + Send + Sync>;

/// What turns queued audio into transcripts (`AudioTranscriber`).
pub trait Transcriber: Send {
    /// Takes what is queued and recognises whatever has become a complete phrase. True if a phrase went to
    /// recognition. Does not wait when the queue is empty.
    fn transcribe(&mut self, queue: &AudioQueue) -> Result<bool, AsrFailure>;
    fn has_transcript(&self) -> bool;
    /// The oldest result not yet handed out.
    fn take_transcript(&mut self) -> Transcript;
    /// The latest recognition failed (the UI marks the message).
    fn recognition_error(&self) -> bool;
}

/// What the session needs from its surroundings.
pub trait Backend: Send + Sync {
    /// The device the settings select, if there is one.
    fn selected_device(&self, kind: Kind) -> Option<Device>;
    /// Opens `device` and returns a recorder configured from the settings.
    fn open_recorder(&self, kind: Kind, device: &Device) -> Result<Arc<dyn Recorder>, String>;
    /// A transcriber for audio in `format`, using the engine the settings select.
    fn create_transcriber(&self, kind: Kind, format: Format) -> Result<Box<dyn Transcriber>, String>;
}

/// The reason a session could not start. The UI has been told as well.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionError {
    pub failure: Failure,
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.failure.message(), self.failure.code.as_str())
    }
}

impl std::error::Error for SessionError {}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Features {
    transcript: bool,
    energy: bool,
}

impl Features {
    fn is_empty(self) -> bool {
        !self.transcript && !self.energy
    }
}

/// A thread that repeats until told to stop (`threadFnc`).
struct Worker {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
    id: ThreadId,
}

#[derive(Default)]
struct State {
    features: Features,
    recorder: Option<Arc<dyn Recorder>>,
    transcript_worker: Option<Worker>,
    energy_worker: Option<Worker>,
    audio_queue: Option<AudioQueue>,
    active_device: Option<Device>,
}

#[derive(Default)]
struct Pipeline {
    /// Stopping because of a failure has begun: later failures are the same one seen again.
    started: bool,
    /// The UI has been told.
    notified: bool,
}

struct Callbacks {
    transcript: Option<OnMessage>,
    level: OnLevel,
}

struct Shared {
    kind: Kind,
    backend: Arc<dyn Backend>,
    stop_timeout: Duration,
    callbacks: Mutex<Callbacks>,
    state: Mutex<State>,
    /// Stopping is one thing at a time.
    stop_lock: Mutex<()>,
    pipeline: Mutex<Pipeline>,
    log: Mutex<OnNote>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub struct AudioSession {
    shared: Arc<Shared>,
}

impl AudioSession {
    pub fn new(kind: Kind, backend: Arc<dyn Backend>) -> Self {
        AudioSession {
            shared: Arc::new(Shared {
                kind,
                backend,
                stop_timeout: STOP_JOIN_TIMEOUT,
                callbacks: Mutex::new(Callbacks { transcript: None, level: Arc::new(|_| {}) }),
                state: Mutex::new(State::default()),
                stop_lock: Mutex::new(()),
                pipeline: Mutex::new(Pipeline::default()),
                log: Mutex::new(Arc::new(|_| {})),
            }),
        }
    }

    /// How long stopping waits for each thread before it gives up and reports `CLEANUP_TIMEOUT`.
    pub fn with_stop_timeout(mut self, timeout: Duration) -> Self {
        let shared = Arc::get_mut(&mut self.shared).expect("the session has not been shared yet");
        shared.stop_timeout = timeout;
        self
    }

    /// Where the session writes its notes (`printLog`).
    pub fn set_log(&self, log: OnNote) {
        *lock(&self.shared.log) = log;
    }

    /// Where transcripts, and the failure that ends a session, go.
    pub fn set_message_callback(&self, callback: Option<OnMessage>) {
        lock(&self.shared.callbacks).transcript = callback;
    }

    /// Where the meter's values go.
    pub fn set_level_callback(&self, callback: OnLevel) {
        lock(&self.shared.callbacks).level = callback;
    }

    pub fn wants_transcript(&self) -> bool {
        lock(&self.shared.state).features.transcript
    }

    pub fn wants_energy(&self) -> bool {
        lock(&self.shared.state).features.energy
    }

    /// The device the recorder has open.
    pub fn active_device(&self) -> Option<Device> {
        lock(&self.shared.state).active_device.clone()
    }

    /// Turns the transcript and the meter on (`Some(true)`), off (`Some(false)`) or leaves them (`None`). A
    /// `device` overrides what the settings select. Does nothing when neither the wanted set nor the device
    /// has changed and the recorder is listening; otherwise stops and starts again.
    pub fn reconfigure(&self, transcript: Option<bool>, energy: Option<bool>, device: Option<&Device>) -> Result<(), SessionError> {
        let shared = &self.shared;
        let (current, active, listening) = {
            let state = lock(&shared.state);
            (state.features, state.active_device.clone(), state.recorder.as_ref().is_some_and(|recorder| recorder.is_listening()))
        };
        let mut wanted = current;
        if let Some(on) = transcript {
            wanted.transcript = on;
        }
        if let Some(on) = energy {
            wanted.energy = on;
        }
        let resolved = shared.resolve_device(device);

        // A recorder that was made but never started listening is not "running": start over.
        let already_running = wanted.is_empty() || listening;
        let same_device = resolved.as_ref().map(|d| &d.name) == active.as_ref().map(|d| &d.name);
        if wanted == current && same_device && already_running {
            return Ok(());
        }

        shared.stop();
        lock(&shared.state).features = wanted;
        if wanted.is_empty() {
            return Ok(());
        }
        shared.start(resolved)
    }

    pub fn pause(&self) {
        let recorder = lock(&self.shared.state).recorder.clone();
        // A recorder that is not listening has nothing to pause.
        if let Some(recorder) = recorder.filter(|recorder| recorder.is_listening()) {
            recorder.pause();
        }
    }

    /// Throws away audio queued while paused (it is old), then goes on listening.
    pub fn resume(&self) {
        let (queue, recorder) = {
            let state = lock(&self.shared.state);
            (state.audio_queue.clone(), state.recorder.clone())
        };
        if let Some(queue) = queue {
            queue.clear();
        }
        if let Some(recorder) = recorder.filter(|recorder| recorder.is_listening()) {
            recorder.resume();
        }
    }

    /// The recorder's latched device error, if a recorder is open.
    pub fn device_error(&self) -> Option<super::recorder::DeviceError> {
        lock(&self.shared.state).recorder.as_ref().map(|recorder| recorder.device_error().clone())
    }

    /// Stops everything. True if a thread did not finish in time.
    pub fn stop(&self) -> bool {
        self.shared.stop()
    }
}

impl Drop for AudioSession {
    fn drop(&mut self) {
        self.shared.stop();
    }
}

impl Shared {
    fn note(&self, text: &str) {
        let log = lock(&self.log).clone();
        log(text);
    }

    fn resolve_device(&self, requested: Option<&Device>) -> Option<Device> {
        match requested {
            Some(device) if device.name == NO_DEVICE => None,
            Some(device) => Some(device.clone()),
            None => self.backend.selected_device(self.kind),
        }
    }

    fn failure(&self, code: ErrorCode, stage: &str, exception_type: Option<&str>) -> Failure {
        Failure::new(code, stage, self.kind.as_str(), exception_type)
    }

    fn send_message(&self, message: Message) {
        let callback = lock(&self.callbacks).transcript.clone();
        if let Some(callback) = callback {
            if catch_unwind(AssertUnwindSafe(|| callback(message))).is_err() {
                self.note("the transcript callback panicked");
            }
        }
    }

    fn send_level(&self, level: Level) {
        let callback = lock(&self.callbacks).level.clone();
        if catch_unwind(AssertUnwindSafe(|| callback(level))).is_err() {
            self.note("the energy callback panicked");
        }
    }

    /// Tells the UI a failure ended the session, once.
    fn notify_pipeline_error(&self, failure: Failure) {
        {
            let mut pipeline = lock(&self.pipeline);
            if pipeline.notified {
                return;
            }
            pipeline.notified = true;
        }
        self.send_message(Message::Failure(failure));
    }

    fn start(self: &Arc<Self>, device: Option<Device>) -> Result<(), SessionError> {
        let Some(device) = device else {
            // Nothing to listen to: tell whoever was waiting, and want nothing.
            let features = lock(&self.state).features;
            if features.transcript {
                self.send_message(Message::NoDevice);
            }
            if features.energy {
                self.send_level(Level::NoDevice);
            }
            let mut state = lock(&self.state);
            state.features = Features::default();
            state.active_device = None;
            return Ok(());
        };

        // Compared by `reconfigure` to see whether the device changed.
        lock(&self.state).active_device = Some(device.clone());
        *lock(&self.pipeline) = Pipeline::default();

        match self.try_start(&device) {
            Ok(()) => Ok(()),
            Err(start_failure) => {
                // Whatever got started is stopped through the usual path before the UI is told.
                let recorder_failure = lock(&self.state).recorder.as_ref().and_then(|recorder| recorder.device_error().info());
                let mut failure = start_failure
                    .or(recorder_failure)
                    .unwrap_or_else(|| self.failure(ErrorCode::AudioReadError, "recording", None));
                if self.stop() {
                    failure = self.failure(ErrorCode::CleanupTimeout, "cleanup", None);
                }
                self.notify_pipeline_error(failure.clone());
                Err(SessionError { failure })
            }
        }
    }

    /// Opens the recorder, starts it and the threads that serve it. A failure that has a UI code of its
    /// own is returned; `None` means the recorder's own error (or a generic one) applies.
    fn try_start(self: &Arc<Self>, device: &Device) -> Result<(), Option<Failure>> {
        let features = lock(&self.state).features;
        let recorder = self
            .backend
            .open_recorder(self.kind, device)
            .map_err(|_| Some(self.failure(ErrorCode::AudioOpenError, "recording", Some("OSError"))))?;
        lock(&self.state).recorder = Some(Arc::clone(&recorder));

        // The meter alone needs no audio: a queue that keeps nothing stops the recorder's phrases piling up.
        let audio_queue: AudioQueue = if features.transcript { Queue::bounded(AUDIO_QUEUE_SIZE) } else { Queue::discarding() };
        // Only the latest value means anything to a meter.
        let energy_queue: Option<EnergyQueue> = features.energy.then(|| Queue::bounded(1));
        lock(&self.state).audio_queue = Some(audio_queue.clone());
        recorder.record_into(audio_queue.clone(), energy_queue.clone()).map_err(|_| None)?;

        let mut transcriber = None;
        if features.transcript {
            let made = self.backend.create_transcriber(self.kind, recorder.format());
            transcriber = Some(made.map_err(|_| Some(self.failure(ErrorCode::TranscriberInitError, "asr", Some("RuntimeError"))))?);
        }

        if let Some(transcriber) = transcriber {
            let worker = self.spawn_transcript_worker(Arc::clone(&recorder), transcriber, audio_queue).map_err(|_| None)?;
            lock(&self.state).transcript_worker = Some(worker);
        }
        if let Some(queue) = energy_queue {
            let worker = self.spawn_energy_worker(queue).map_err(|_| None)?;
            lock(&self.state).energy_worker = Some(worker);
        }
        Ok(())
    }

    fn spawn_transcript_worker(
        self: &Arc<Self>,
        recorder: Arc<dyn Recorder>,
        mut transcriber: Box<dyn Transcriber>,
        queue: AudioQueue,
    ) -> std::io::Result<Worker> {
        let stop = Arc::new(AtomicBool::new(false));
        let (shared, flag) = (Arc::clone(self), Arc::clone(&stop));
        let thread = thread::Builder::new().name(format!("vrct-{}-transcript", self.kind.as_str())).spawn(move || {
            while !flag.load(Ordering::SeqCst) {
                shared.transcribe_round(&recorder, transcriber.as_mut(), &queue);
            }
            // What was still queued will never be recognised.
            while queue.try_pop().is_some() {}
        })?;
        let id = thread.thread().id();
        Ok(Worker { stop, thread, id })
    }

    /// One pass of the transcript thread: report a dead device, or recognise and hand out what is ready.
    fn transcribe_round(self: &Arc<Self>, recorder: &Arc<dyn Recorder>, transcriber: &mut dyn Transcriber, queue: &AudioQueue) {
        let errors = recorder.device_error();
        if errors.is_set() {
            let failure = errors.info().unwrap_or_else(|| self.failure(ErrorCode::AudioReadError, "recording", None));
            errors.clear();
            self.handle_pipeline_error(failure);
            return;
        }

        let outcome = catch_unwind(AssertUnwindSafe(|| -> Result<bool, Failure> {
            let sent = transcriber.transcribe(queue).map_err(|failed| {
                Failure::new(ErrorCode::AsrError, AsrFailure::STAGE, &failed.source, Some(&failed.exception_type))
            })?;
            // A single call can leave several results (the Google engine sends once per chunk taken): all of them
            // go out now, not after the next successful recognition.
            while transcriber.has_transcript() {
                let transcript = transcriber.take_transcript();
                let recognition_error = transcriber.recognition_error();
                self.send_message(Message::Transcript { transcript, recognition_error });
            }
            Ok(sent)
        }));
        match outcome {
            Ok(Ok(true)) => {}
            // Nothing was sent: look again in a moment instead of spinning.
            Ok(Ok(false)) => thread::sleep(POLL),
            Ok(Err(failure)) => self.handle_pipeline_error(failure),
            Err(_) => self.handle_pipeline_error(self.failure(ErrorCode::AsrError, "asr", Some("panic"))),
        }
    }

    fn spawn_energy_worker(self: &Arc<Self>, queue: EnergyQueue) -> std::io::Result<Worker> {
        let stop = Arc::new(AtomicBool::new(false));
        let (shared, flag) = (Arc::clone(self), Arc::clone(&stop));
        let thread = thread::Builder::new().name(format!("vrct-{}-energy-meter", self.kind.as_str())).spawn(move || {
            while !flag.load(Ordering::SeqCst) {
                if let Some(value) = queue.try_pop() {
                    shared.send_level(Level::Value(value));
                }
                thread::sleep(POLL);
            }
        })?;
        let id = thread.thread().id();
        Ok(Worker { stop, thread, id })
    }

    /// Stops the session because of `failure`, once, and tells the UI. Called from the thread that found it.
    fn handle_pipeline_error(self: &Arc<Self>, failure: Failure) {
        {
            let mut pipeline = lock(&self.pipeline);
            if pipeline.started {
                return;
            }
            pipeline.started = true;
        }
        // The transcript thread cannot join itself: it ends its loop, and the rest happens elsewhere.
        if let Some(worker) = &lock(&self.state).transcript_worker {
            if worker.id == thread::current().id() {
                worker.stop.store(true, Ordering::SeqCst);
            }
        }
        let shared = Arc::clone(self);
        let spawned = thread::Builder::new().name(format!("vrct-{}-error-cleanup", self.kind.as_str())).spawn(move || {
            let timed_out = shared.stop();
            let to_tell = if timed_out { shared.failure(ErrorCode::CleanupTimeout, "cleanup", None) } else { failure };
            shared.notify_pipeline_error(to_tell);
        });
        if spawned.is_err() {
            self.note("cannot start the cleanup thread");
        }
    }

    /// Stops the threads and the recorder and forgets the device. True if something did not finish in time.
    fn stop(&self) -> bool {
        let _one_at_a_time = lock(&self.stop_lock);
        let mut timed_out = false;
        let (kind, label) = (self.kind.as_str(), self.kind.label());

        let (transcript_worker, energy_worker) = {
            let mut state = lock(&self.state);
            (state.transcript_worker.take(), state.energy_worker.take())
        };
        for (worker, what) in [(transcript_worker, "transcription"), (energy_worker, "energy")] {
            let Some(worker) = worker else { continue };
            worker.stop.store(true, Ordering::SeqCst);
            // A thread cannot wait for itself.
            if worker.id != thread::current().id() && !join_within(worker.thread, self.stop_timeout) {
                timed_out = true;
                self.note(&format!("{label} {what} thread did not terminate within timeout"));
            }
        }

        let recorder = lock(&self.state).recorder.take();
        // A recorder that never started listening has nothing to resume or stop; it closes its device when dropped.
        if let Some(recorder) = recorder.filter(|recorder| recorder.is_listening()) {
            // A paused listener never notices a stop.
            recorder.resume();
            // The stop can take long if the device hangs; do not let it hold up the UI for ever.
            let stopping = Arc::clone(&recorder);
            match thread::Builder::new().name(format!("vrct-{kind}-recorder-stop")).spawn(move || stopping.stop()) {
                Ok(thread) => {
                    if !join_within(thread, self.stop_timeout) {
                        timed_out = true;
                        self.note(&format!("{label} recorder did not stop within timeout"));
                    }
                }
                Err(_) => recorder.stop(),
            }
        }

        let mut state = lock(&self.state);
        if let Some(queue) = state.audio_queue.take() {
            queue.clear();
        }
        state.active_device = None;
        state.features = Features::default();
        timed_out
    }
}

/// Waits for `thread` to end for at most `limit`. False if it is still running (it is left to finish alone).
fn join_within(thread: JoinHandle<()>, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while !thread.is_finished() {
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(2));
    }
    let _ = thread.join();
    true
}

// ---- the transcriber the app uses ----------------------------------------------------------------------

/// What one call to recognise asks of the engine, read from the settings each time (the user can change
/// the languages while listening).
#[derive(Debug, Clone, PartialEq)]
pub struct Ask {
    pub languages: Vec<String>,
    pub countries: Vec<String>,
    pub avg_logprob: f64,
    pub no_speech_prob: f64,
    pub no_repeat_ngram_size: u32,
}

/// [`PhraseTranscriber`] with an engine behind it.
pub struct EngineTranscriber {
    phrases: PhraseTranscriber,
    /// `None` when the engine could not be set up: every phrase then fails, as it does in Python.
    recognizer: Option<Box<dyn Recognizer + Send>>,
    ask: Box<dyn Fn() -> Ask + Send>,
}

impl EngineTranscriber {
    pub fn new(phrases: PhraseTranscriber, recognizer: Option<Box<dyn Recognizer + Send>>, ask: Box<dyn Fn() -> Ask + Send>) -> Self {
        EngineTranscriber { phrases, recognizer, ask }
    }

    pub fn phrases(&self) -> &PhraseTranscriber {
        &self.phrases
    }
}

impl Transcriber for EngineTranscriber {
    fn transcribe(&mut self, queue: &AudioQueue) -> Result<bool, AsrFailure> {
        let ask = (self.ask)();
        let query = Query {
            languages: &ask.languages,
            countries: &ask.countries,
            avg_logprob: ask.avg_logprob,
            no_speech_prob: ask.no_speech_prob,
            no_repeat_ngram_size: ask.no_repeat_ngram_size,
        };
        let mut queue = queue.clone();
        let recognizer = self.recognizer.as_mut().map(|engine| engine.as_mut() as &mut dyn Recognizer);
        self.phrases.transcribe_queue(&mut queue, recognizer, &query, Stamp::now())
    }

    fn has_transcript(&self) -> bool {
        self.phrases.has_transcript()
    }

    fn take_transcript(&mut self) -> Transcript {
        self.phrases.take_transcript()
    }

    fn recognition_error(&self) -> bool {
        self.phrases.last_recognition_error()
    }
}
