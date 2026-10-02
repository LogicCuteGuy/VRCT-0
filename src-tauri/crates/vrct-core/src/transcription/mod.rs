//! Speech to text, in this process.
//!
//! `phrases` decides when the audio a recorder queued is sent for recognition and keeps the
//! results until they are delivered. It follows `AudioTranscriber.transcribeAudioQueue` in
//! `models/transcription/transcription_transcriber.py` and is checked against its recorded behaviour.

pub mod phrases;
