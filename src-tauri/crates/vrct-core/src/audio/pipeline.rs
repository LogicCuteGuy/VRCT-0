//! Audio in, speech segments out: a worker thread that owns a `VadSegmenter`, fed 16 kHz mono
//! PCM from any thread, and, on Windows, the glue that starts a capture in front of it.
//!
//! The capture callback runs on the OS audio thread and must not wait, so `feed` only queues
//! (a full queue drops the chunk and counts it) and the model runs here, off that thread.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::JoinHandle;

use super::vad::{FrameProbability, SpeechSegment, VadSegmenter};

/// About 8 seconds of 10 ms device buffers: more than the model ever falls behind by.
const QUEUE_CHUNKS: usize = 800;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Segment(SpeechSegment),
    /// The probability engine failed on a frame; the frame is dropped and listening goes on.
    EngineError(String),
    /// The capture stream failed (device unplugged, format changed). Nothing more will arrive
    /// from it; the owner should stop this pipeline and start another.
    CaptureFailed(String),
}

enum Message {
    Pcm(Vec<u8>),
    CaptureFailed(String),
    Stop,
}

#[derive(Clone)]
pub struct Feeder {
    queue: SyncSender<Message>,
    dropped: Arc<AtomicUsize>,
}

impl Feeder {
    /// Queues 16 kHz mono PCM16 without blocking. False if it was dropped (queue full or worker gone).
    pub fn feed(&self, pcm: &[u8]) -> bool {
        match self.queue.try_send(Message::Pcm(pcm.to_vec())) {
            Ok(()) => true,
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    pub fn capture_failed(&self, error: String) {
        let _ = self.queue.try_send(Message::CaptureFailed(error));
    }

    /// Chunks dropped because the worker could not keep up or had finished.
    pub fn dropped_chunks(&self) -> usize {
        self.dropped.load(Ordering::Relaxed)
    }
}

pub struct SegmentWorker {
    feeder: Feeder,
    thread: Option<JoinHandle<()>>,
}

impl SegmentWorker {
    pub fn spawn<P>(mut segmenter: VadSegmenter<P>, mut handler: impl FnMut(Event) + Send + 'static) -> Result<Self, String>
    where
        P: FrameProbability + Send + 'static,
    {
        let (queue, inbox) = mpsc::sync_channel::<Message>(QUEUE_CHUNKS);
        let thread = std::thread::Builder::new()
            .name("vrct-vad".into())
            .spawn(move || {
                // Stop (finish / drop) is queued behind the audio, so everything fed before it is processed.
                for message in inbox {
                    match message {
                        Message::Stop => break,
                        Message::Pcm(pcm) => match segmenter.process(&pcm) {
                            Ok(segments) => segments.into_iter().for_each(|s| handler(Event::Segment(s))),
                            Err(error) => handler(Event::EngineError(error)),
                        },
                        Message::CaptureFailed(error) => handler(Event::CaptureFailed(error)),
                    }
                }
                match segmenter.flush() {
                    Ok(Some(segment)) => handler(Event::Segment(segment)),
                    Ok(None) => {}
                    Err(error) => handler(Event::EngineError(error)),
                }
            })
            .map_err(|e| format!("cannot start the VAD thread: {e}"))?;
        Ok(SegmentWorker { feeder: Feeder { queue, dropped: Arc::new(AtomicUsize::new(0)) }, thread: Some(thread) })
    }

    pub fn feeder(&self) -> Feeder {
        self.feeder.clone()
    }

    /// Stops listening: what is queued is processed, the segment still open is ended with
    /// `SegmentEnd::Flush` and delivered, then the thread is joined. Chunks fed afterwards by a
    /// feeder still held elsewhere are dropped (and counted).
    pub fn finish(mut self) {
        self.join();
    }

    fn join(&mut self) {
        if let Some(thread) = self.thread.take() {
            // A blocking send: this is not the audio thread, and a full queue empties as the worker runs.
            let _ = self.feeder.queue.send(Message::Stop);
            let _ = thread.join();
        }
    }
}

impl Drop for SegmentWorker {
    fn drop(&mut self) {
        self.join();
    }
}

#[cfg(windows)]
pub use capture_pipeline::CapturePipeline;

#[cfg(windows)]
mod capture_pipeline {
    use super::{Event, SegmentWorker, VadSegmenter};
    use crate::audio::capture::{Capture, Source};
    use crate::audio::vad::FrameProbability;

    /// A device, the normaliser, the VAD and the handler, started and stopped together.
    pub struct CapturePipeline {
        capture: Capture,
        worker: SegmentWorker,
    }

    impl CapturePipeline {
        pub fn start<P>(
            source: Source,
            device_name: &str,
            segmenter: VadSegmenter<P>,
            handler: impl FnMut(Event) + Send + 'static,
        ) -> Result<Self, String>
        where
            P: FrameProbability + Send + 'static,
        {
            let worker = SegmentWorker::spawn(segmenter, handler)?;
            let (audio, failures) = (worker.feeder(), worker.feeder());
            let capture = Capture::start(
                source,
                device_name,
                move |pcm| {
                    audio.feed(pcm);
                },
                move |error| failures.capture_failed(error),
            )?;
            Ok(CapturePipeline { capture, worker })
        }

        /// Stops the device first, so nothing new is queued, then lets the worker drain and flush.
        pub fn stop(mut self) {
            self.capture.stop();
            self.worker.finish();
        }
    }
}
