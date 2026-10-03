//! Local Whisper as a recognition engine: the part of `LocalWhisperProvider` that is not the model.
//! What the model is asked (language, thresholds), which of its segments count, how their text is
//! joined, and what confidence and `definitive` come back.

use super::super::clip::to_16k_mono;
use super::super::languages;
use super::super::phrases::{Recognition, RecognizeError, Recognizer, Request};

/// What one segment of a transcription reports.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    /// As the model wrote it, usually with a leading space.
    pub text: String,
    pub avg_logprob: f64,
    pub no_speech_prob: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Info {
    /// The language code the model used or detected (`en`, `ja`, ...).
    pub language: String,
    /// How sure the detection was; 1 when the language was given.
    pub language_probability: f64,
}

/// What VRCT asks of a transcription; the rest of faster-whisper's options are fixed (beam size 5,
/// temperature 0, no timestamps, `transcribe`).
#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    /// `log_prob_threshold`: also the bar a segment's `avg_logprob` must clear here.
    pub avg_logprob: f64,
    /// `no_speech_threshold`.
    pub no_speech_prob: f64,
    pub no_repeat_ngram_size: u32,
}

/// A loaded Whisper model, as far as the provider needs it.
pub trait Transcribe: Send {
    /// Transcribes 16 kHz mono samples in [-1, 1]; `language` is a code, or `None` to detect it.
    fn transcribe(&self, samples: &[f32], language: Option<&str>, options: &Options) -> Result<(Vec<Segment>, Info), String>;
}

pub struct LocalWhisper<M> {
    model: M,
}

impl<M: Transcribe> LocalWhisper<M> {
    pub fn new(model: M) -> Self {
        LocalWhisper { model }
    }

    pub fn model(&self) -> &M {
        &self.model
    }
}

/// 16-bit samples as the model takes them: divided by 32768 in float32.
pub fn samples_of(pcm16: &[u8]) -> Vec<f32> {
    pcm16.chunks_exact(2).map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0).collect()
}

impl<M: Transcribe> Recognizer for LocalWhisper<M> {
    fn recognize(&mut self, request: &Request<'_>) -> Result<Recognition, RecognizeError> {
        let pcm = to_16k_mono(request.pcm, request.format).map_err(|_| RecognizeError::Other { kind: "error".into() })?;
        let samples = samples_of(&pcm);

        let key_error = || RecognizeError::Other { kind: "KeyError".into() };
        let table_code = || languages::code(request.language, request.country, "Whisper").ok_or_else(key_error);
        let source_language = if request.force_language { Some(table_code()?) } else { None };

        let options = Options {
            avg_logprob: request.avg_logprob,
            no_speech_prob: request.no_speech_prob,
            no_repeat_ngram_size: request.no_repeat_ngram_size,
        };
        let (segments, info) = self
            .model
            .transcribe(&samples, source_language, &options)
            .map_err(|_| RecognizeError::Other { kind: "RuntimeError".into() })?;

        let mut text = String::new();
        for segment in &segments {
            if segment.avg_logprob < request.avg_logprob || segment.no_speech_prob > request.no_speech_prob {
                continue;
            }
            text.push_str(&segment.text);
        }
        let definitive = request.force_language || table_code()? == info.language;
        Ok(Recognition { text, confidence: info.language_probability, definitive })
    }
}
