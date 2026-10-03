//! Speech to text, in this process.
//!
//! `phrases` decides when the audio a recorder queued is sent for recognition and keeps the
//! results until they are delivered. It follows `AudioTranscriber.transcribeAudioQueue` in
//! `models/transcription/transcription_transcriber.py` and is checked against its recorded behaviour.
//! The engines are `Recognizer`s: `openai` (Groq, OpenAI, a custom server) and `deepgram` are
//! network calls (`cloud` makes them callable from the transcriber's thread), `languages` is the
//! table of language codes they read and `clip` makes the 16 kHz WAV they upload. `whisper` is the
//! local engine.

pub mod clip;
pub mod cloud;
pub mod deepgram;
pub mod languages;
pub mod openai;
pub mod phrases;
pub mod whisper;
