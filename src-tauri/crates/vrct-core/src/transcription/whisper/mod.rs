//! Local Whisper (faster-whisper's models, run by CTranslate2), in this process.
//!
//! `features` makes the log-mel spectrogram the model takes, `provider` is the recognition engine
//! around a model (segment filtering, confidence), and, with the `ct2` feature, `model` runs the
//! model itself: the encoder, language detection, and the decoder with its prompt, the way
//! `faster_whisper.WhisperModel.transcribe` does for VRCT's options.

pub mod features;
#[cfg(feature = "ct2")]
pub mod model;
pub mod provider;
