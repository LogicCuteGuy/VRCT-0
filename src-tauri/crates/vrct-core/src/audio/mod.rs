//! Audio pipeline pieces that run in this process instead of in the Python sidecar.
//!
//! `normalize` turns whatever PCM a device delivers into 16 kHz mono 16-bit,
//! `vad` cuts that stream into speech segments with a state machine around a
//! per-frame speech probability, which `silero` computes. `devices`, `capture` and `samples` read
//! the microphone or a speaker's loopback through WASAPI, and `pipeline` runs capture, normaliser and
//! VAD together on their own threads. The normaliser and VAD follow `models/transcription/audio_vad.py`
//! and are checked against its output.

pub mod devices;
pub mod host;
pub mod normalize;
pub mod pipeline;
pub mod samples;
pub mod silero;
pub mod vad;
#[cfg(windows)]
pub mod capture;
#[cfg(windows)]
pub mod wasapi;

/// What the speech-segment code consumes: 16 kHz, mono, signed 16-bit little endian.
pub const TARGET_SAMPLE_RATE: u32 = 16_000;
/// Samples per frame handed to the probability engine (32 ms).
pub const FRAME_SAMPLES: usize = 512;
pub const FRAME_BYTES: usize = FRAME_SAMPLES * 2;
