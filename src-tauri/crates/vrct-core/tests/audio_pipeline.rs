//! `audio::pipeline`: the worker thread around `VadSegmenter`, with a scripted engine, and (Windows)
//! the capture glue on the default speaker's loopback.

use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vrct_core::audio::pipeline::{Event, SegmentWorker};
use vrct_core::audio::vad::{FrameProbability, SegmentEnd, SegmentIds, VadConfig, VadSegmenter};
use vrct_core::audio::FRAME_BYTES;

/// Answers from a script by frame number; frames past the script are silence. `fail_on` makes one frame an error.
struct Scripted {
    probabilities: Vec<f32>,
    fail_on: Option<usize>,
    next: usize,
}

impl Scripted {
    fn new(probabilities: &[f32]) -> Self {
        Scripted { probabilities: probabilities.to_vec(), fail_on: None, next: 0 }
    }
}

impl FrameProbability for Scripted {
    fn probability(&mut self, _frame: &[f32]) -> Result<f32, String> {
        let index = self.next;
        self.next += 1;
        if self.fail_on == Some(index) {
            return Err("model failed".to_string());
        }
        Ok(self.probabilities.get(index).copied().unwrap_or(0.0))
    }
    fn reset(&mut self) {}
}

fn config() -> VadConfig {
    VadConfig { hangover_frames: 2, min_speech_frames: 2, pre_speech_pad_frames: 1, max_speech_frames: None, ..VadConfig::default() }
}

fn segmenter(engine: Scripted) -> VadSegmenter<Scripted> {
    VadSegmenter::with_ids(engine, config(), SegmentIds::starting_at(1))
}

fn frames(count: usize, value: u8) -> Vec<u8> {
    vec![value; count * FRAME_BYTES]
}

fn worker(engine: Scripted) -> (SegmentWorker, Receiver<Event>) {
    let (tx, rx) = channel();
    let worker = SegmentWorker::spawn(segmenter(engine), move |event| {
        let _ = tx.send(event);
    })
    .unwrap();
    (worker, rx)
}

fn drain(rx: &Receiver<Event>) -> Vec<Event> {
    rx.try_iter().collect()
}

/// What the segmenter alone makes of the same audio, fed in one piece.
fn direct(engine: Scripted, audio: &[u8]) -> Vec<Event> {
    let mut segmenter = segmenter(engine);
    let mut events: Vec<Event> = segmenter.process(audio).unwrap().into_iter().map(Event::Segment).collect();
    events.extend(segmenter.flush().unwrap().map(Event::Segment));
    events
}

const SCRIPT: [f32; 12] = [0.0, 0.9, 0.9, 0.9, 0.0, 0.0, 0.0, 0.0, 0.9, 0.9, 0.0, 0.0];

#[test]
fn the_worker_gives_the_segments_the_segmenter_gives_however_the_audio_is_chunked() {
    let mut audio = Vec::new();
    for frame in 0..12 {
        audio.extend(frames(1, frame as u8 + 1));
    }
    let expected = direct(Scripted::new(&SCRIPT), &audio);
    assert!(expected.len() >= 2, "the script should make two segments: {expected:?}");

    let (worker, rx) = worker(Scripted::new(&SCRIPT));
    let feeder = worker.feeder();
    // Odd chunk sizes: frames are split across feeds.
    for chunk in audio.chunks(777) {
        assert!(feeder.feed(chunk));
    }
    worker.finish();
    assert_eq!(drain(&rx), expected);
}

#[test]
fn finish_flushes_the_segment_still_open() {
    let (worker, rx) = worker(Scripted::new(&[0.9, 0.9, 0.9, 0.9]));
    worker.feeder().feed(&frames(4, 7));
    worker.finish();
    let events = drain(&rx);
    assert_eq!(events.len(), 1, "{events:?}");
    match &events[0] {
        Event::Segment(segment) => assert_eq!(segment.reason, SegmentEnd::Flush),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_partial_frame_at_the_end_is_padded_and_scored_on_finish() {
    let (worker, rx) = worker(Scripted::new(&[0.9, 0.9, 0.9]));
    worker.feeder().feed(&frames(2, 3));
    worker.feeder().feed(&vec![5u8; FRAME_BYTES / 2]);
    worker.finish();
    match &drain(&rx)[..] {
        // Speech starts on the second positive frame (one frame of pre-roll), so the padded third
        // frame is what makes the segment two frames long.
        [Event::Segment(segment)] => {
            assert_eq!(segment.audio.len(), 2 * FRAME_BYTES);
            let last = &segment.audio[FRAME_BYTES..];
            assert!(last[..FRAME_BYTES / 2].iter().all(|b| *b == 5));
            assert!(last[FRAME_BYTES / 2..].iter().all(|b| *b == 0), "padded with zeros");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_engine_error_is_reported_and_listening_goes_on() {
    let mut engine = Scripted::new(&[0.0, 0.0, 0.9, 0.9, 0.9, 0.0, 0.0, 0.0]);
    engine.fail_on = Some(0);
    let (worker, rx) = worker(engine);
    let feeder = worker.feeder();
    feeder.feed(&frames(1, 1)); // the frame that fails
    feeder.feed(&frames(7, 2));
    worker.finish();
    let events = drain(&rx);
    assert_eq!(events[0], Event::EngineError("model failed".to_string()));
    assert!(events.iter().any(|e| matches!(e, Event::Segment(_))), "{events:?}");
}

#[test]
fn a_capture_failure_is_passed_on_in_order() {
    let (worker, rx) = worker(Scripted::new(&[0.9, 0.9, 0.9, 0.0, 0.0, 0.0]));
    let feeder = worker.feeder();
    feeder.feed(&frames(6, 1));
    feeder.capture_failed("device unplugged".to_string());
    worker.finish();
    let events = drain(&rx);
    let last = events.last().unwrap();
    assert_eq!(*last, Event::CaptureFailed("device unplugged".to_string()));
    assert!(matches!(events[0], Event::Segment(_)), "audio fed before the failure is still processed: {events:?}");
}

#[test]
fn feeding_after_finish_is_dropped_and_counted_not_blocked() {
    let (worker, _rx) = worker(Scripted::new(&[]));
    let feeder = worker.feeder();
    worker.finish();
    assert!(!feeder.feed(&frames(1, 1)));
    assert_eq!(feeder.dropped_chunks(), 1);
}

#[test]
fn a_slow_handler_does_not_block_the_feeder_and_nothing_is_lost_below_the_queue_limit() {
    let seen = Arc::new(Mutex::new(0usize));
    let counter = seen.clone();
    let worker = SegmentWorker::spawn(segmenter(Scripted::new(&[0.9; 50])), move |_| {
        std::thread::sleep(Duration::from_millis(20));
        *counter.lock().unwrap() += 1;
    })
    .unwrap();
    let feeder = worker.feeder();
    let started = std::time::Instant::now();
    for _ in 0..50 {
        assert!(feeder.feed(&frames(1, 1)));
    }
    assert!(started.elapsed() < Duration::from_millis(500), "feed must not wait for the worker");
    worker.finish();
    assert_eq!(feeder.dropped_chunks(), 0);
    assert_eq!(*seen.lock().unwrap(), 1, "one flushed segment");
}

#[cfg(windows)]
#[test]
fn the_loopback_pipeline_starts_runs_and_stops() {
    use vrct_core::audio::capture::Source;
    use vrct_core::audio::pipeline::CapturePipeline;
    use vrct_core::audio::wasapi::list_devices;

    let Some(name) = list_devices().expect("listing").default_speaker else {
        eprintln!("no playback device, skipping");
        return;
    };
    let (tx, rx) = channel();
    // Whatever is playing counts as speech, so a segment is made if anything is audible; silence makes none.
    let pipeline = CapturePipeline::start(Source::Speaker, &name, segmenter(Scripted::new(&[0.9; 100_000])), move |event| {
        let _ = tx.send(event);
    })
    .expect("starting");
    std::thread::sleep(Duration::from_millis(600));
    pipeline.stop();
    let events: Vec<Event> = rx.try_iter().collect();
    assert!(events.iter().all(|e| !matches!(e, Event::EngineError(_) | Event::CaptureFailed(_))), "{events:?}");
    eprintln!("loopback pipeline events: {}", events.len());
    // Everything after stop is quiet.
    std::thread::sleep(Duration::from_millis(200));
    assert!(rx.try_recv().is_err());
}

#[test]
fn a_full_queue_drops_chunks_instead_of_blocking_the_audio_thread() {
    use std::sync::mpsc::sync_channel;
    // The handler stalls on its first event until released, so the queue behind it fills up.
    let (release, gate) = sync_channel::<()>(0);
    let gate = Mutex::new(Some(gate));
    let worker = SegmentWorker::spawn(segmenter(Scripted::new(&[0.9; 4])), move |_| {
        if let Some(gate) = gate.lock().unwrap().take() {
            let _ = gate.recv();
        }
    })
    .unwrap();
    // Declared after the worker so a failing assertion drops (and so releases) it first.
    let release = release;
    let feeder = worker.feeder();
    // A capture failure reaches the handler at once, which is the quickest way to stall it.
    feeder.capture_failed("stall".to_string());
    std::thread::sleep(Duration::from_millis(100)); // the worker is now inside the handler
    let started = std::time::Instant::now();
    let accepted = (0..2000).filter(|_| feeder.feed(&frames(1, 1))).count();
    assert!(started.elapsed() < Duration::from_millis(500), "feed must never wait");
    assert!((700..2000).contains(&accepted), "accepted {accepted}");
    assert_eq!(feeder.dropped_chunks(), 2000 - accepted);
    release.send(()).unwrap();
    worker.finish();
}
