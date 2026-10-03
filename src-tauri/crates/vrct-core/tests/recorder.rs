//! The recorders and queues of a session: what each pushes onto its queues and how it reports a device that
//! fails. (The energy-threshold logic itself is checked against Python in `energy.rs`, the segmenter in
//! `audio_vad.rs`, and the session around them in `session.rs`.)

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use vrct_core::audio::vad::{FrameProbability, SegmentEnd, SegmentIds, VadConfig, VadSegmenter};
use vrct_core::audio::FRAME_SAMPLES;
use vrct_core::transcription::energy::{rms, Source};
use vrct_core::transcription::failure::{ErrorCode, Failure};
use vrct_core::transcription::phrases::{Chunk, ChunkSource, Format, Stamp};
use vrct_core::transcription::queue::Queue;
use vrct_core::transcription::recorder::{
    CaptureFactory, CaptureFailure, Capturing, EnergyParams, EnergyRecorder, PcmSink, Recorder, VadRecorder,
};

fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(5));
    }
}

fn chunk(text: &str) -> Chunk {
    Chunk { data: text.as_bytes().to_vec(), at: Stamp::now(), end: None }
}

// ---- queues ---------------------------------------------------------------------------------------------

#[test]
fn a_full_queue_gives_up_its_oldest_item() {
    let queue = Queue::bounded(3);
    assert!(!queue.put_dropping_oldest(1));
    assert!(!queue.put_dropping_oldest(2));
    assert!(!queue.put_dropping_oldest(3));
    assert!(queue.put_dropping_oldest(4), "the fourth item pushes the first out");
    assert_eq!(queue.len(), 3);
    assert_eq!((queue.try_pop(), queue.try_pop(), queue.try_pop(), queue.try_pop()), (Some(2), Some(3), Some(4), None));
}

#[test]
fn a_queue_of_one_keeps_only_the_latest() {
    let queue = Queue::bounded(1);
    queue.put_dropping_oldest(5);
    queue.put_dropping_oldest(9);
    assert_eq!(queue.try_pop(), Some(9));
    assert!(queue.is_empty());
}

#[test]
fn a_discarding_queue_keeps_nothing() {
    let queue = Queue::discarding();
    assert!(!queue.put_dropping_oldest(1));
    assert!(queue.is_empty());
    assert_eq!(queue.capacity(), None);
    assert_eq!(Queue::<u8>::bounded(20).capacity(), Some(20));
}

#[test]
fn clones_share_the_items_and_clear_empties_them() {
    let queue = Queue::bounded(5);
    let other = queue.clone();
    other.put_dropping_oldest(chunk("a"));
    other.put_dropping_oldest(chunk("b"));
    let mut source = queue.clone();
    assert!(!ChunkSource::is_empty(&source));
    assert_eq!(source.pop().unwrap().data, b"a");
    queue.clear();
    assert!(other.is_empty());
    assert!(source.pop().is_none());
}

// ---- the energy-threshold recorder ----------------------------------------------------------------------

const CHUNK_FRAMES: usize = 1024;

fn loud() -> Vec<u8> {
    let pattern = [3000i16, -2250, 1500, -1000];
    (0..CHUNK_FRAMES).flat_map(|i| pattern[i % 4].to_le_bytes()).collect()
}

fn quiet() -> Vec<u8> {
    vec![0; CHUNK_FRAMES * 2]
}

enum Step {
    Chunk(Vec<u8>),
    /// Waits until the flag is set.
    Gate(Arc<AtomicBool>),
    Fail,
}

/// Plays a script, then waits (as a device with nothing to say does) until it is closed.
struct ScriptedSource {
    steps: VecDeque<Step>,
    closed: Arc<AtomicBool>,
}

impl Source for ScriptedSource {
    fn chunk(&self) -> usize {
        CHUNK_FRAMES
    }
    fn sample_rate(&self) -> u32 {
        16_000
    }
    fn sample_width(&self) -> u32 {
        2
    }
    fn available(&mut self) -> bool {
        true
    }
    fn read(&mut self) -> io::Result<Vec<u8>> {
        loop {
            if self.closed.load(Ordering::SeqCst) {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"));
            }
            match self.steps.front() {
                Some(Step::Chunk(_)) => {
                    let Some(Step::Chunk(data)) = self.steps.pop_front() else { unreachable!() };
                    return Ok(data);
                }
                Some(Step::Gate(flag)) if flag.load(Ordering::SeqCst) => {
                    self.steps.pop_front();
                }
                Some(Step::Fail) => {
                    self.steps.pop_front();
                    return Err(io::Error::other("device lost"));
                }
                _ => thread::sleep(Duration::from_millis(2)),
            }
        }
    }
}

/// `count` chunks of speech followed by enough quiet to end the phrase.
fn phrase() -> Vec<Step> {
    let mut steps: Vec<Step> = (0..3).map(|_| Step::Chunk(quiet())).collect();
    steps.extend((0..10).map(|_| Step::Chunk(loud())));
    steps.extend((0..16).map(|_| Step::Chunk(quiet())));
    steps
}

fn params() -> EnergyParams {
    EnergyParams { energy_threshold: 300.0, dynamic_energy_threshold: false, phrase_time_limit: 0.0, record_timeout: 5.0 }
}

fn energy_recorder(steps: Vec<Step>, label: &'static str) -> (EnergyRecorder, Arc<AtomicBool>) {
    let closed = Arc::new(AtomicBool::new(false));
    let source = ScriptedSource { steps: steps.into(), closed: Arc::clone(&closed) };
    let unblock = Arc::clone(&closed);
    let recorder = EnergyRecorder::new(label, Box::new(source), 1, params(), Box::new(move || unblock.store(true, Ordering::SeqCst)));
    (recorder, closed)
}

#[test]
fn the_energy_recorder_queues_a_phrase_and_the_levels() {
    let (recorder, _) = energy_recorder(phrase(), "mic");
    assert_eq!(recorder.format(), Format { sample_rate: 16_000, sample_width: 2, channels: 1 });
    assert!(!recorder.is_listening());

    let (audio, energy) = (Queue::bounded(5), Queue::bounded(1));
    recorder.record_into(audio.clone(), Some(energy.clone())).unwrap();
    assert!(recorder.is_listening());

    wait_until("a phrase", || !audio.is_empty());
    let phrase = audio.try_pop().unwrap();
    assert_eq!(phrase.end, None, "the energy recorder says nothing about why a phrase ended");
    assert!(phrase.data.len() >= 10 * CHUNK_FRAMES * 2, "all of the speech is in it");
    assert_eq!(phrase.data.len() % (CHUNK_FRAMES * 2), 0);
    assert!(phrase.data.windows(2).any(|pair| pair == 3000i16.to_le_bytes()), "it holds the loud samples");
    assert!(energy.try_pop().is_some(), "every chunk read reports its loudness");

    recorder.stop();
    assert!(!recorder.device_error().is_set(), "a stop is not a failure");
    assert!(recorder.is_listening(), "like Python, a stopped recorder still has its stopper");
    recorder.stop();
}

#[test]
fn levels_are_the_rms_of_what_was_read() {
    let (recorder, _) = energy_recorder(vec![Step::Chunk(loud())], "mic");
    let (audio, energy) = (Queue::bounded(5), Queue::bounded(1));
    recorder.record_into(audio, Some(energy.clone())).unwrap();
    wait_until("a level", || !energy.is_empty());
    assert_eq!(energy.try_pop(), Some(rms(&loud(), 2)));
    recorder.stop();
}

#[test]
fn a_recorder_that_is_not_watched_for_energy_pushes_none() {
    let (recorder, _) = energy_recorder(phrase(), "mic");
    let audio = Queue::bounded(5);
    recorder.record_into(audio.clone(), None).unwrap();
    wait_until("a phrase", || !audio.is_empty());
    recorder.stop();
}

#[test]
fn it_listens_only_once() {
    let (recorder, _) = energy_recorder(vec![], "mic");
    recorder.record_into(Queue::bounded(5), None).unwrap();
    assert!(recorder.record_into(Queue::bounded(5), None).is_err());
    recorder.stop();
}

#[test]
fn a_lost_device_is_reported_once() {
    let (recorder, _) = energy_recorder(vec![Step::Chunk(quiet()), Step::Chunk(quiet()), Step::Fail], "speaker");
    recorder.record_into(Queue::bounded(5), None).unwrap();
    wait_until("the error", || recorder.device_error().is_set());
    assert_eq!(recorder.device_error().info(), Some(Failure::new(ErrorCode::AudioReadError, "recording", "speaker", Some("OSError"))));
    recorder.device_error().clear();
    assert!(!recorder.device_error().is_set());
    assert!(recorder.device_error().info().is_some(), "the information stays after the flag is cleared");
    recorder.stop();
}

#[test]
fn the_first_unhandled_failure_is_the_one_kept() {
    let (recorder, _) = energy_recorder(vec![], "mic");
    let errors = recorder.device_error();
    errors.set(ErrorCode::AudioReadError, "recording", "mic", "OSError");
    errors.set(ErrorCode::VadInferenceError, "vad", "mic", "RuntimeError");
    assert_eq!(errors.info(), Some(Failure::new(ErrorCode::AudioReadError, "recording", "mic", Some("OSError"))));
    // Once it has been handled, the next one counts.
    errors.clear();
    errors.set(ErrorCode::VadInferenceError, "vad", "mic", "RuntimeError");
    assert_eq!(errors.info(), Some(Failure::new(ErrorCode::VadInferenceError, "vad", "mic", Some("RuntimeError"))));
}

#[test]
fn phrases_that_do_not_fit_are_counted_as_dropped() {
    let mut steps = phrase();
    steps.extend(phrase());
    let (recorder, _) = energy_recorder(steps, "mic");
    let audio = Queue::bounded(1);
    recorder.record_into(audio.clone(), None).unwrap();
    wait_until("the second phrase to push out the first", || recorder.dropped_phrases() == 1);
    assert_eq!(audio.len(), 1);
    recorder.stop();
}

/// A device that keeps delivering until it is closed, and is not told when it is.
struct Endless;

impl Source for Endless {
    fn chunk(&self) -> usize {
        CHUNK_FRAMES
    }
    fn sample_rate(&self) -> u32 {
        16_000
    }
    fn sample_width(&self) -> u32 {
        2
    }
    fn available(&mut self) -> bool {
        true
    }
    fn read(&mut self) -> io::Result<Vec<u8>> {
        thread::sleep(Duration::from_millis(2));
        Ok(quiet())
    }
}

#[test]
fn stopping_ends_a_listener_whose_device_keeps_delivering() {
    let recorder = EnergyRecorder::new("mic", Box::new(Endless), 1, params(), Box::new(|| {}));
    recorder.record_into(Queue::bounded(5), None).unwrap();
    thread::sleep(Duration::from_millis(30));
    recorder.stop();
    assert!(!recorder.device_error().is_set());
}

#[test]
fn a_failure_while_stopping_is_not_reported() {
    // Closing the stream makes the waiting read fail, as it does on a real device.
    let (recorder, _) = energy_recorder(vec![], "mic");
    recorder.record_into(Queue::bounded(5), None).unwrap();
    thread::sleep(Duration::from_millis(50));
    recorder.stop();
    assert!(!recorder.device_error().is_set());
    assert_eq!(recorder.device_error().info(), None);
}

#[test]
fn pausing_stops_the_phrases_and_resuming_brings_them_back() {
    let (second, third) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
    let mut steps = phrase();
    steps.push(Step::Gate(Arc::clone(&second)));
    steps.extend(phrase());
    steps.push(Step::Gate(Arc::clone(&third)));
    steps.extend(phrase());
    let (recorder, _) = energy_recorder(steps, "mic");
    let audio = Queue::bounded(10);
    recorder.record_into(audio.clone(), None).unwrap();

    wait_until("the first phrase", || audio.len() == 1);
    recorder.pause();
    second.store(true, Ordering::SeqCst);
    // The phrase being listened to when the pause came is still delivered; then the listener waits.
    wait_until("the second phrase", || audio.len() == 2);
    third.store(true, Ordering::SeqCst);
    thread::sleep(Duration::from_millis(500));
    assert_eq!(audio.len(), 2, "nothing is read while paused");

    recorder.resume();
    wait_until("the third phrase", || audio.len() == 3);
    recorder.stop();
}

// ---- the VAD recorder -----------------------------------------------------------------------------------

/// Speech wherever the frame is not silent.
struct ByLevel;

impl FrameProbability for ByLevel {
    fn probability(&mut self, frame: &[f32]) -> Result<f32, String> {
        Ok(if frame.iter().any(|sample| sample.abs() > 0.05) { 0.9 } else { 0.0 })
    }
    fn reset(&mut self) {}
}

struct Failing;

impl FrameProbability for Failing {
    fn probability(&mut self, _: &[f32]) -> Result<f32, String> {
        Err("model failed".to_string())
    }
    fn reset(&mut self) {}
}

fn segmenter<P: FrameProbability>(engine: P) -> VadSegmenter<P> {
    let config = VadConfig { hangover_frames: 2, min_speech_frames: 2, pre_speech_pad_frames: 1, max_speech_frames: None, ..VadConfig::default() };
    VadSegmenter::with_ids(engine, config, SegmentIds::starting_at(1))
}

fn speech(frames: usize) -> Vec<u8> {
    (0..frames * FRAME_SAMPLES).flat_map(|_| 8000i16.to_le_bytes()).collect()
}

fn silence(frames: usize) -> Vec<u8> {
    vec![0; frames * FRAME_SAMPLES * 2]
}

#[derive(Default)]
struct FakeCapture {
    sink: Mutex<Option<PcmSink>>,
    failure: Mutex<Option<CaptureFailure>>,
    stopped: Arc<AtomicBool>,
    refuse: bool,
}

struct Running(Arc<AtomicBool>);

impl Capturing for Running {
    fn stop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// What the recorder is given; the test keeps the other handle to push audio through.
struct Factory(Arc<FakeCapture>);

impl CaptureFactory for Factory {
    fn start(&self, sink: PcmSink, on_failure: CaptureFailure) -> Result<Box<dyn Capturing>, String> {
        if self.0.refuse {
            return Err("no such device".to_string());
        }
        *self.0.sink.lock().unwrap() = Some(sink);
        *self.0.failure.lock().unwrap() = Some(on_failure);
        Ok(Box::new(Running(Arc::clone(&self.0.stopped))))
    }
}

impl FakeCapture {
    fn feed(&self, pcm: &[u8]) {
        let mut sink = self.sink.lock().unwrap();
        (sink.as_mut().expect("capture started"))(pcm);
    }
}

fn vad_recorder<P: FrameProbability + Send + 'static>(engine: P, capture: &Arc<FakeCapture>) -> VadRecorder {
    VadRecorder::new("mic", Arc::new(Factory(Arc::clone(capture))), segmenter(engine))
}

#[test]
fn the_vad_recorder_queues_segments_with_why_they_ended() {
    let capture = Arc::new(FakeCapture::default());
    let recorder = vad_recorder(ByLevel, &capture);
    assert_eq!(recorder.format(), Format { sample_rate: 16_000, sample_width: 2, channels: 1 });
    let (audio, energy) = (Queue::bounded(5), Queue::bounded(1));
    recorder.record_into(audio.clone(), Some(energy.clone())).unwrap();
    assert!(recorder.is_listening());

    capture.feed(&silence(2));
    capture.feed(&speech(6));
    capture.feed(&silence(6));
    wait_until("a segment", || !audio.is_empty());
    let segment = audio.try_pop().unwrap();
    assert_eq!(segment.end, Some(SegmentEnd::Silence));
    assert!(segment.data.len() >= 6 * FRAME_SAMPLES * 2);
    assert!(energy.try_pop().is_some());
    recorder.stop();
    assert!(!recorder.device_error().is_set());
}

#[test]
fn levels_are_measured_on_the_pcm_the_capture_delivers() {
    let capture = Arc::new(FakeCapture::default());
    let recorder = vad_recorder(ByLevel, &capture);
    let (audio, energy) = (Queue::bounded(5), Queue::bounded(1));
    recorder.record_into(audio, Some(energy.clone())).unwrap();
    let pcm = speech(1);
    capture.feed(&pcm);
    assert_eq!(energy.try_pop(), Some(rms(&pcm, 2)));
    recorder.stop();
}

#[test]
fn stopping_ends_the_open_segment_and_closes_the_device_first() {
    let capture = Arc::new(FakeCapture::default());
    let recorder = vad_recorder(ByLevel, &capture);
    let audio = Queue::bounded(5);
    recorder.record_into(audio.clone(), None).unwrap();
    capture.feed(&speech(8));
    assert!(audio.is_empty(), "the speech has not ended");
    recorder.stop();
    assert!(capture.stopped.load(Ordering::SeqCst), "the capture was stopped");
    let segment = audio.try_pop().expect("the open segment is delivered when listening stops");
    assert_eq!(segment.end, Some(SegmentEnd::Flush));
    recorder.stop();
}

#[test]
fn a_paused_vad_recorder_lets_the_audio_go() {
    let capture = Arc::new(FakeCapture::default());
    let recorder = vad_recorder(ByLevel, &capture);
    let (audio, energy) = (Queue::bounded(5), Queue::bounded(1));
    recorder.record_into(audio.clone(), Some(energy.clone())).unwrap();

    recorder.pause();
    capture.feed(&speech(6));
    capture.feed(&silence(6));
    thread::sleep(Duration::from_millis(100));
    assert!(audio.is_empty() && energy.is_empty(), "nothing is heard while paused");

    recorder.resume();
    capture.feed(&speech(6));
    capture.feed(&silence(6));
    wait_until("a segment", || !audio.is_empty());
    recorder.stop();
}

#[test]
fn a_lost_capture_is_a_read_error() {
    let capture = Arc::new(FakeCapture::default());
    let recorder = vad_recorder(ByLevel, &capture);
    recorder.record_into(Queue::bounded(5), None).unwrap();
    (capture.failure.lock().unwrap().as_ref().unwrap())("device unplugged".to_string());
    wait_until("the error", || recorder.device_error().is_set());
    assert_eq!(recorder.device_error().info(), Some(Failure::new(ErrorCode::AudioReadError, "recording", "mic", Some("OSError"))));
    recorder.stop();
}

#[test]
fn a_failing_model_is_a_vad_error() {
    let capture = Arc::new(FakeCapture::default());
    let recorder = vad_recorder(Failing, &capture);
    recorder.record_into(Queue::bounded(5), None).unwrap();
    capture.feed(&speech(2));
    wait_until("the error", || recorder.device_error().is_set());
    assert_eq!(recorder.device_error().info(), Some(Failure::new(ErrorCode::VadInferenceError, "vad", "mic", Some("RuntimeError"))));
    recorder.stop();
}

#[test]
fn a_device_that_cannot_be_opened_is_an_open_error() {
    let capture = Arc::new(FakeCapture { refuse: true, ..FakeCapture::default() });
    let recorder = vad_recorder(ByLevel, &capture);
    assert!(recorder.record_into(Queue::bounded(5), None).is_err());
    assert!(!recorder.is_listening());
    assert_eq!(recorder.device_error().info(), Some(Failure::new(ErrorCode::AudioOpenError, "recording", "mic", Some("OSError"))));
    recorder.stop();
}

#[test]
fn a_failure_while_stopping_is_ignored_by_the_vad_recorder() {
    let capture = Arc::new(FakeCapture::default());
    let recorder = vad_recorder(ByLevel, &capture);
    recorder.record_into(Queue::bounded(5), None).unwrap();
    recorder.stop();
    (capture.failure.lock().unwrap().as_ref().unwrap())("closed".to_string());
    thread::sleep(Duration::from_millis(50));
    assert!(!recorder.device_error().is_set());
}
