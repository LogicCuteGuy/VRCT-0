//! Phrase accumulation: which queued audio is joined into one phrase, and when it is sent for
//! recognition. A port of `AudioTranscriber.transcribeAudioQueue`.
//!
//! A recorder queues chunks of audio. A phrase is not sent while it can still grow; it is sent
//! when one of these holds:
//!
//! * the silence between two chunks is longer than `phrase_timeout`;
//! * the buffered audio reaches [`MAX_PHRASE_SECONDS`];
//! * the queue has run dry and `phrase_timeout` has passed in real time since the last chunk;
//! * (segmenter mode) a chunk arrives that ended on a natural boundary rather than on the
//!   segmenter's length cap, which is queued as `SegmentEnd::MaxDuration` and only accumulates.
//!
//! The Google engine is different: it is sent the growing buffer again after every drain (an
//! "interim send"), and a gap or the 15 s cap then only resets what was already sent.
//!
//! One call returns after the first phrase it sent, leaving the rest of the queue for the next
//! call, so the caller can deliver that phrase before the next recognition starts. A call that
//! sent nothing reports `false`; the caller is expected to pause briefly before calling again
//! (Python sleeps 10 ms inside the call, which a library function should not do).

use std::collections::VecDeque;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::audio::vad::SegmentEnd;

/// Audio longer than this is sent without waiting for a pause.
pub const MAX_PHRASE_SECONDS: f64 = 15.0;
/// Digital silence put before and after a clip that a segmenter ended, whatever the engine
/// (the Google endpoint sometimes finds nothing in a clip that has no lead-in or tail).
pub const SEGMENT_PRE_PAD_MS: u32 = 300;
pub const SEGMENT_POST_PAD_MS: u32 = 500;

/// A point in time, in microseconds on any clock the caller uses consistently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Stamp(pub i64);

impl Stamp {
    pub fn from_millis(ms: i64) -> Self {
        Stamp(ms * 1000)
    }

    pub fn now() -> Self {
        let since_epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        Stamp(since_epoch.as_micros() as i64)
    }
}

/// What a chunk of queued audio is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub data: Vec<u8>,
    pub at: Stamp,
    /// Why a segmenter ended the chunk; `None` from the energy-threshold recorder (and counts as
    /// a natural boundary when a segmenter did not say).
    pub end: Option<SegmentEnd>,
}

/// Where chunks come from. Taking one must not wait.
pub trait ChunkSource {
    fn pop(&mut self) -> Option<Chunk>;
    fn is_empty(&self) -> bool;
}

impl ChunkSource for VecDeque<Chunk> {
    fn pop(&mut self) -> Option<Chunk> {
        self.pop_front()
    }

    fn is_empty(&self) -> bool {
        VecDeque::is_empty(self)
    }
}

/// The format of the audio in a phrase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Format {
    pub sample_rate: u32,
    pub sample_width: u32,
    pub channels: u32,
}

/// Which recognition engine is asked; this changes how a phrase is sent and which errors are kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    /// The free web endpoint: unreliable per call, so sent as a growing buffer.
    Google,
    /// Local Whisper: sent once, finished. A failure stays flagged until a later success.
    Whisper,
    /// An API engine (Groq/OpenAI/custom Whisper, Deepgram): sent once; the error flag is cleared before each phrase.
    Cloud,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub speaker: bool,
    pub format: Format,
    /// Seconds of silence between chunks that end a phrase.
    pub phrase_timeout: i64,
    /// Results kept for delivery (the oldest is dropped beyond this).
    pub max_phrases: i64,
    pub engine: Engine,
    /// The chunks come from a segmenter (each carries its end reason) instead of the energy recorder.
    pub segmented: bool,
}

/// One recognition request: a clip, one candidate language, and the engine's thresholds.
#[derive(Debug, Clone, PartialEq)]
pub struct Request<'a> {
    pub pcm: &'a [u8],
    pub format: Format,
    pub language: &'a str,
    pub country: &'a str,
    pub avg_logprob: f64,
    pub no_speech_prob: f64,
    pub no_repeat_ngram_size: u32,
    /// Only one candidate language: use it rather than detect.
    pub force_language: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Recognition {
    pub text: String,
    pub confidence: f64,
    /// The engine detected the language itself and was sure: the remaining candidates are not tried.
    pub definitive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecognizeError {
    /// Nothing recognised (not a failure).
    NoMatch,
    /// An API engine failed with one of the `TRANSCRIPTION_API_*` codes.
    Api { code: String },
    /// Anything else; the text names the kind of failure for the report.
    Other { kind: String },
}

pub trait Recognizer {
    fn recognize(&mut self, request: &Request<'_>) -> Result<Recognition, RecognizeError>;
}

/// What the caller asks of one call.
#[derive(Debug, Clone, PartialEq)]
pub struct Query<'a> {
    pub languages: &'a [String],
    pub countries: &'a [String],
    pub avg_logprob: f64,
    pub no_speech_prob: f64,
    pub no_repeat_ngram_size: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Transcript {
    pub text: String,
    pub confidence: f64,
    pub language: Option<String>,
    /// How long the recognition took.
    pub asr_ms: Option<u64>,
}

impl Transcript {
    fn empty() -> Self {
        Transcript { text: String::new(), confidence: 0.0, language: None, asr_ms: None }
    }
}

/// Recognition failed and nothing was recognised: the session should be stopped and the user told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsrFailure {
    /// `mic` or `speaker`.
    pub source: String,
    /// The kind of the first failure.
    pub exception_type: String,
}

impl AsrFailure {
    pub const CODE: &'static str = "ASR_ERROR";
    pub const STAGE: &'static str = "asr";
}

pub struct PhraseTranscriber {
    settings: Settings,
    buffer: Vec<u8>,
    last_spoken: Option<Stamp>,
    phrase_started_at: Option<Stamp>,
    transcripts: VecDeque<Transcript>,
    last_recognition_error: bool,
    last_api_error_code: Option<String>,
    asr_attempts: u64,
    asr_successes: u64,
}

/// What one call has done so far.
#[derive(Default)]
struct Round {
    transcribed: bool,
    /// The buffer holds audio that no recognition has seen yet.
    unsent: bool,
}

impl PhraseTranscriber {
    pub fn new(settings: Settings) -> Self {
        PhraseTranscriber {
            settings,
            buffer: Vec::new(),
            last_spoken: None,
            phrase_started_at: None,
            transcripts: VecDeque::new(),
            last_recognition_error: false,
            last_api_error_code: None,
            asr_attempts: 0,
            asr_successes: 0,
        }
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn last_recognition_error(&self) -> bool {
        self.last_recognition_error
    }

    pub fn last_api_error_code(&self) -> Option<&str> {
        self.last_api_error_code.as_deref()
    }

    pub fn asr_attempts(&self) -> u64 {
        self.asr_attempts
    }

    pub fn asr_successes(&self) -> u64 {
        self.asr_successes
    }

    pub fn buffered_len(&self) -> usize {
        self.buffer.len()
    }

    pub fn last_spoken(&self) -> Option<Stamp> {
        self.last_spoken
    }

    pub fn phrase_started_at(&self) -> Option<Stamp> {
        self.phrase_started_at
    }

    /// Results waiting for delivery.
    pub fn has_transcript(&self) -> bool {
        !self.transcripts.is_empty()
    }

    /// Results waiting for delivery, newest first.
    pub fn transcripts(&self) -> impl Iterator<Item = &Transcript> {
        self.transcripts.iter()
    }

    /// The oldest result waiting, or an empty one.
    pub fn take_transcript(&mut self) -> Transcript {
        self.transcripts.pop_back().unwrap_or_else(Transcript::empty)
    }

    pub fn clear(&mut self) {
        self.transcripts.clear();
        self.buffer.clear();
        self.last_spoken = None;
        self.phrase_started_at = None;
    }

    /// Takes what is queued and sends whatever has become a complete phrase. `now` is the wall
    /// clock, for the "quiet for `phrase_timeout`" check once the queue is empty. True if a phrase
    /// went to recognition (whether or not it was understood).
    pub fn transcribe_queue(
        &mut self,
        queue: &mut impl ChunkSource,
        recognizer: Option<&mut dyn Recognizer>,
        query: &Query<'_>,
        now: Stamp,
    ) -> Result<bool, AsrFailure> {
        let mut recognizer = recognizer;
        let mut round = Round::default();
        let is_google = self.settings.engine == Engine::Google;
        let timeout = self.settings.phrase_timeout.saturating_mul(1_000_000);

        if self.settings.segmented {
            while let Some(chunk) = queue.pop() {
                if self.phrase_started_at.is_none() {
                    self.phrase_started_at = Some(chunk.at);
                }
                self.buffer.extend_from_slice(&chunk.data);
                self.last_spoken = Some(chunk.at);
                // A chunk the segmenter cut at its length cap only accumulates, until a natural
                // boundary comes or so much audio has gathered that waiting any longer is worse.
                let natural_end = chunk.end != Some(SegmentEnd::MaxDuration);
                if natural_end || self.buffered_seconds() >= MAX_PHRASE_SECONDS {
                    self.finalize(&mut round, &mut recognizer, query)?;
                } else if is_google && queue.is_empty() {
                    self.interim_send(&mut round, &mut recognizer, query)?;
                }
                if round.transcribed {
                    return Ok(true);
                }
            }
            return Ok(round.transcribed);
        }

        while let Some(chunk) = queue.pop() {
            if let Some(last) = self.last_spoken {
                if chunk.at.0 - last.0 > timeout {
                    if is_google && !round.unsent {
                        self.reset_only(&mut round);
                    } else {
                        self.finalize(&mut round, &mut recognizer, query)?;
                    }
                }
            }
            if self.phrase_started_at.is_none() {
                self.phrase_started_at = Some(chunk.at);
            }
            self.buffer.extend_from_slice(&chunk.data);
            self.last_spoken = Some(chunk.at);
            round.unsent = true;

            if self.buffered_seconds() >= MAX_PHRASE_SECONDS {
                self.finalize(&mut round, &mut recognizer, query)?;
            } else if is_google && queue.is_empty() {
                self.interim_send(&mut round, &mut recognizer, query)?;
            }
            if round.transcribed {
                return Ok(true);
            }
        }

        if !self.buffer.is_empty() {
            if let Some(last) = self.last_spoken {
                if now.0 - last.0 > timeout {
                    if is_google && !round.unsent {
                        self.reset_only(&mut round);
                    } else {
                        self.finalize(&mut round, &mut recognizer, query)?;
                    }
                }
            }
        }
        Ok(round.transcribed)
    }

    /// Seconds of audio in the buffer; 0 while the format is not usable.
    fn buffered_seconds(&self) -> f64 {
        let format = self.settings.format;
        let bytes_per_second = u64::from(format.sample_rate) * u64::from(format.sample_width) * u64::from(format.channels);
        if bytes_per_second == 0 {
            return 0.0;
        }
        self.buffer.len() as f64 / bytes_per_second as f64
    }

    /// Sends the buffer and starts the next phrase.
    fn finalize(
        &mut self,
        round: &mut Round,
        recognizer: &mut Option<&mut dyn Recognizer>,
        query: &Query<'_>,
    ) -> Result<(), AsrFailure> {
        // A failed send leaves the buffer as it was: the next call sends it again.
        if self.send_buffer(recognizer, query)? {
            round.transcribed = true;
        }
        self.buffer.clear();
        self.phrase_started_at = None;
        round.unsent = false;
        Ok(())
    }

    /// Sends the buffer and keeps it: the next chunk makes it longer and it goes out again.
    fn interim_send(
        &mut self,
        round: &mut Round,
        recognizer: &mut Option<&mut dyn Recognizer>,
        query: &Query<'_>,
    ) -> Result<(), AsrFailure> {
        if self.send_buffer(recognizer, query)? {
            round.transcribed = true;
        }
        round.unsent = false;
        Ok(())
    }

    fn reset_only(&mut self, round: &mut Round) {
        self.buffer.clear();
        self.phrase_started_at = None;
        round.unsent = false;
    }

    /// Recognises the buffer, with silence around it when a segmenter cut it. False if there is nothing to send.
    fn send_buffer(
        &mut self,
        recognizer: &mut Option<&mut dyn Recognizer>,
        query: &Query<'_>,
    ) -> Result<bool, AsrFailure> {
        if self.buffer.is_empty() {
            return Ok(false);
        }
        if self.settings.segmented {
            let padded = pad_with_silence(&self.buffer, self.settings.format);
            self.recognize_phrase(&padded, recognizer, query)
        } else {
            let audio = std::mem::take(&mut self.buffer);
            let outcome = self.recognize_phrase(&audio, recognizer, query);
            self.buffer = audio;
            outcome
        }
    }

    fn recognize_phrase(
        &mut self,
        audio: &[u8],
        recognizer: &mut Option<&mut dyn Recognizer>,
        query: &Query<'_>,
    ) -> Result<bool, AsrFailure> {
        if matches!(self.settings.engine, Engine::Google | Engine::Cloud) {
            self.last_recognition_error = false;
            self.last_api_error_code = None;
        }

        let started = Instant::now();
        let mut best = Transcript::empty();
        let mut first_error: Option<String> = None;

        match recognizer.as_deref_mut() {
            None => first_error = Some("RuntimeError".to_string()),
            Some(recognizer) => {
                let force_language = query.languages.len() == 1;
                for (language, country) in query.languages.iter().zip(query.countries.iter()) {
                    let request = Request {
                        pcm: audio,
                        format: self.settings.format,
                        language,
                        country,
                        avg_logprob: query.avg_logprob,
                        no_speech_prob: query.no_speech_prob,
                        no_repeat_ngram_size: query.no_repeat_ngram_size,
                        force_language,
                    };
                    match recognizer.recognize(&request) {
                        Err(RecognizeError::NoMatch) => continue,
                        Err(RecognizeError::Api { code }) => {
                            self.last_recognition_error = true;
                            self.last_api_error_code = Some(code);
                            first_error.get_or_insert_with(|| "TranscriptionApiError".to_string());
                            continue;
                        }
                        Err(RecognizeError::Other { kind }) => {
                            self.last_recognition_error = true;
                            first_error.get_or_insert(kind);
                            continue;
                        }
                        Ok(found) => {
                            if found.confidence > best.confidence {
                                best = Transcript {
                                    text: found.text,
                                    confidence: found.confidence,
                                    language: Some(language.clone()),
                                    asr_ms: None,
                                };
                            }
                            if found.definitive {
                                break;
                            }
                        }
                    }
                }
            }
        }

        self.asr_attempts += 1;
        let succeeded = !best.text.is_empty();
        if let (Some(exception_type), false) = (first_error, succeeded) {
            let source = if self.settings.speaker { "speaker" } else { "mic" };
            return Err(AsrFailure { source: source.to_string(), exception_type });
        }
        if succeeded {
            self.last_recognition_error = false;
            self.asr_successes += 1;
            best.asr_ms = Some(started.elapsed().as_millis() as u64);
            self.update_transcript(best);
        }
        Ok(true)
    }

    /// A finished phrase is always a new entry; the oldest is dropped when the list is over its limit.
    fn update_transcript(&mut self, result: Transcript) {
        if self.transcripts.len() as i64 > self.settings.max_phrases {
            self.transcripts.pop_back();
        }
        self.transcripts.push_front(result);
    }
}

/// `data` with digital silence before and after, as long as the format's rate and width make it.
/// The length is computed in floating point and truncated, as Python did (`int(bytes_per_ms * ms)`).
pub fn pad_with_silence(data: &[u8], format: Format) -> Vec<u8> {
    let bytes_per_ms = f64::from(format.sample_rate) * f64::from(format.sample_width) / 1000.0;
    let pre = (bytes_per_ms * f64::from(SEGMENT_PRE_PAD_MS)) as usize;
    let post = (bytes_per_ms * f64::from(SEGMENT_POST_PAD_MS)) as usize;
    let mut out = Vec::with_capacity(pre + data.len() + post);
    out.resize(pre, 0);
    out.extend_from_slice(data);
    out.resize(pre + data.len() + post, 0);
    out
}
