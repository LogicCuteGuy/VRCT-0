//! Speech boundary detection: `VadSegmenter`.
//!
//! The state machine of `models/transcription/audio_vad.py`, around a per-frame
//! speech probability that is supplied from outside (`FrameProbability`; Silero
//! comes in a later slice). Frames of 512 samples (32 ms) go in; finished
//! segments come out, ended by silence, by the length limit or by a flush.
//!
//! Two properties matter and are kept as Python has them:
//! - the silence "anchor" is only set by a frame below the negative threshold
//!   and only cleared by a frame at or above the speech threshold, so frames in
//!   between neither extend nor cancel it;
//! - a segment cut by the length limit does not reset the probability engine,
//!   because the speaker is still talking.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use super::{FRAME_BYTES, FRAME_SAMPLES, TARGET_SAMPLE_RATE};

/// Probability that one frame of 512 samples (floats in -1..1) contains speech.
pub trait FrameProbability {
    /// An error (the model failing to run) stops `process`/`flush`, as an exception does in Python.
    fn probability(&mut self, frame: &[f32]) -> Result<f32, String>;
    fn reset(&mut self);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentEnd {
    Silence,
    MaxDuration,
    Flush,
}

impl SegmentEnd {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Silence => "silence",
            Self::MaxDuration => "max_duration",
            Self::Flush => "flush",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeechSegment {
    /// 16 kHz mono signed 16-bit little endian.
    pub audio: Vec<u8>,
    pub segment_id: u64,
    pub reason: SegmentEnd,
}

impl SpeechSegment {
    pub fn duration_ms(&self) -> f64 {
        self.audio.len() as f64 / 2.0 / f64::from(TARGET_SAMPLE_RATE) * 1000.0
    }
}

/// Hands out segment ids. One counter is shared by the whole process, so that
/// restarting a microphone or speaker session never reuses an id the UI has shown.
#[derive(Clone)]
pub struct SegmentIds(Arc<AtomicU64>);

impl SegmentIds {
    pub fn global() -> Self {
        static GLOBAL: std::sync::OnceLock<Arc<AtomicU64>> = std::sync::OnceLock::new();
        Self(GLOBAL.get_or_init(|| Arc::new(AtomicU64::new(0))).clone())
    }

    pub fn starting_at(first: u64) -> Self {
        Self(Arc::new(AtomicU64::new(first)))
    }

    fn next(&self) -> u64 {
        self.0.fetch_add(1, Ordering::Relaxed)
    }
}

#[derive(Debug, Clone)]
pub struct VadConfig {
    pub speech_threshold: f64,
    /// `None` means `max(0, speech_threshold - 0.15)`, like Silero's own iterator.
    pub negative_threshold: Option<f64>,
    pub hangover_frames: usize,
    pub max_speech_frames: Option<usize>,
    pub min_speech_frames: usize,
    pub pre_speech_pad_frames: usize,
    /// Shown in the diagnostic lines, e.g. `mic` or `speaker`.
    pub label: String,
}

impl Default for VadConfig {
    /// The values kikitan-translator runs with (about 768 ms hangover, 160 ms pre-roll).
    fn default() -> Self {
        Self {
            speech_threshold: 0.25,
            negative_threshold: None,
            hangover_frames: 24,
            max_speech_frames: Some(250),
            min_speech_frames: 2,
            pre_speech_pad_frames: 5,
            label: "vad".to_string(),
        }
    }
}

type Diagnostic = Box<dyn FnMut(&str) + Send>;

pub struct VadSegmenter<P> {
    probability: P,
    speech_threshold: f64,
    negative_threshold: f64,
    hangover_frames: usize,
    max_speech_frames: Option<usize>,
    min_speech_frames: usize,
    pre_speech_pad_frames: usize,
    label: String,
    diagnostic: Option<Diagnostic>,
    ids: SegmentIds,
    pre_speech_frames: VecDeque<Vec<u8>>,
    remainder: Vec<u8>,
    speech_frames: Vec<Vec<u8>>,
    positive_frames: usize,
    speech_frame_count: usize,
    silence_start_frame: Option<usize>,
    speaking: bool,
    segment_id: u64,
}

impl<P: FrameProbability> VadSegmenter<P> {
    pub fn new(probability: P, config: VadConfig) -> Self {
        Self::with_ids(probability, config, SegmentIds::global())
    }

    pub fn with_ids(probability: P, config: VadConfig, ids: SegmentIds) -> Self {
        let negative_threshold =
            config.negative_threshold.unwrap_or_else(|| (config.speech_threshold - 0.15).max(0.0));
        let segment_id = ids.next();
        Self {
            probability,
            speech_threshold: config.speech_threshold,
            negative_threshold,
            hangover_frames: config.hangover_frames,
            max_speech_frames: config.max_speech_frames,
            min_speech_frames: config.min_speech_frames,
            pre_speech_pad_frames: config.pre_speech_pad_frames,
            label: config.label,
            diagnostic: None,
            ids,
            pre_speech_frames: VecDeque::new(),
            remainder: Vec::new(),
            speech_frames: Vec::new(),
            positive_frames: 0,
            speech_frame_count: 0,
            silence_start_frame: None,
            speaking: false,
            segment_id,
        }
    }

    /// Called with a line on speech start and speech end only, never per frame.
    pub fn on_diagnostic(&mut self, callback: impl FnMut(&str) + Send + 'static) {
        self.diagnostic = Some(Box::new(callback));
    }

    pub fn speaking(&self) -> bool {
        self.speaking
    }

    pub fn engine(&self) -> &P {
        &self.probability
    }

    /// Feed 16 kHz mono PCM of any length; whole frames are consumed, the rest waits for the next call.
    /// On an engine error the frame that failed is dropped and the frames after it stay queued.
    pub fn process(&mut self, pcm: &[u8]) -> Result<Vec<SpeechSegment>, String> {
        self.remainder.extend_from_slice(pcm);
        let mut segments = Vec::new();
        let mut offset = 0;
        while self.remainder.len() - offset >= FRAME_BYTES {
            let frame = self.remainder[offset..offset + FRAME_BYTES].to_vec();
            offset += FRAME_BYTES;
            let scored = self.score(&frame);
            let segment = match scored {
                Ok(probability) => self.process_frame(frame, probability),
                Err(e) => {
                    self.remainder.drain(..offset);
                    return Err(e);
                }
            };
            segments.extend(segment);
        }
        self.remainder.drain(..offset);
        Ok(segments)
    }

    /// Close the stream (pause, stop): a trailing partial frame is zero-padded
    /// and scored, and a segment still open is ended with `Flush`.
    pub fn flush(&mut self) -> Result<Option<SpeechSegment>, String> {
        if !self.remainder.is_empty() {
            let mut padded = std::mem::take(&mut self.remainder);
            padded.resize(FRAME_BYTES, 0);
            let probability = self.score(&padded)?;
            if let Some(segment) = self.process_frame(padded, probability) {
                return Ok(Some(segment));
            }
        }
        if !self.speaking {
            return Ok(None);
        }
        Ok(self.finish_segment(SegmentEnd::Flush))
    }

    /// Back to the initial state (mute, device change). The segment id is kept.
    pub fn reset(&mut self) {
        self.remainder.clear();
        self.speech_frames.clear();
        self.positive_frames = 0;
        self.speech_frame_count = 0;
        self.silence_start_frame = None;
        self.speaking = false;
        self.pre_speech_frames.clear();
        self.probability.reset();
    }

    fn score(&mut self, frame: &[u8]) -> Result<f32, String> {
        let samples: Vec<f32> = frame
            .chunks_exact(2)
            .map(|pair| f32::from(i16::from_le_bytes([pair[0], pair[1]])) / 32768.0)
            .collect();
        debug_assert_eq!(samples.len(), FRAME_SAMPLES);
        self.probability.probability(&samples)
    }

    fn process_frame(&mut self, frame: Vec<u8>, probability: f32) -> Option<SpeechSegment> {
        let probability = f64::from(probability);
        if !self.speaking {
            if self.pre_speech_pad_frames > 0 {
                if self.pre_speech_frames.len() == self.pre_speech_pad_frames {
                    self.pre_speech_frames.pop_front();
                }
                self.pre_speech_frames.push_back(frame);
            }
            if probability >= self.speech_threshold {
                self.positive_frames += 1;
            } else {
                self.positive_frames = 0;
            }
            if self.positive_frames >= self.min_speech_frames {
                self.speaking = true;
                self.speech_frames = self.pre_speech_frames.drain(..).collect();
                self.speech_frame_count = self.positive_frames;
                self.silence_start_frame = None;
                self.log(&format!(
                    "speech_start segment_id={} prob={:.3} threshold={:?} pre_roll_frames={}",
                    self.segment_id,
                    probability,
                    self.speech_threshold,
                    self.speech_frames.len()
                ));
                if self.max_speech_reached() {
                    return self.finish_segment(SegmentEnd::MaxDuration);
                }
            }
            return None;
        }

        self.speech_frames.push(frame);
        self.speech_frame_count += 1;

        if probability >= self.speech_threshold {
            self.silence_start_frame = None;
        } else if probability < self.negative_threshold {
            let start = *self.silence_start_frame.get_or_insert(self.speech_frame_count);
            if self.speech_frame_count - start >= self.hangover_frames {
                return self.finish_segment(SegmentEnd::Silence);
            }
        }
        // Between the two thresholds the anchor is left alone.

        if self.max_speech_reached() {
            return self.finish_segment(SegmentEnd::MaxDuration);
        }
        None
    }

    fn max_speech_reached(&self) -> bool {
        self.max_speech_frames.is_some_and(|max| self.speech_frame_count >= max)
    }

    fn finish_segment(&mut self, reason: SegmentEnd) -> Option<SpeechSegment> {
        let frame_count = self.speech_frame_count;
        let mut result = None;
        if self.speaking && frame_count >= self.min_speech_frames {
            result = Some(SpeechSegment { audio: self.speech_frames.concat(), segment_id: self.segment_id, reason });
        }
        if self.speaking || self.positive_frames > 0 {
            self.segment_id = self.ids.next();
        }

        self.speech_frames.clear();
        self.positive_frames = 0;
        self.speech_frame_count = 0;
        self.silence_start_frame = None;
        self.speaking = false;
        self.pre_speech_frames.clear();

        if let Some(segment) = &result {
            self.log(&format!(
                "speech_end segment_id={} reason={} duration_ms={:.1} frames={}",
                segment.segment_id,
                reason.as_str(),
                segment.duration_ms(),
                frame_count
            ));
        }

        if reason != SegmentEnd::MaxDuration {
            self.probability.reset();
        }
        result
    }

    fn log(&mut self, message: &str) {
        if let Some(callback) = self.diagnostic.as_mut() {
            callback(&format!("[VAD][{}] {message}", self.label));
        }
    }
}
