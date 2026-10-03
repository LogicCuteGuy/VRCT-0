//! Speech to text, in this process.
//!
//! `phrases` decides when the audio a recorder queued is sent for recognition and keeps the
//! results until they are delivered. It follows `AudioTranscriber.transcribeAudioQueue` in
//! `models/transcription/transcription_transcriber.py` and is checked against its recorded behaviour.
//! The engines are `Recognizer`s: `openai` (Groq, OpenAI, a custom server) and `deepgram` are
//! network calls (`cloud` makes them callable from the transcriber's thread), `languages` is the
//! table of language codes they read and `clip` makes the 16 kHz WAV they upload. `whisper` is the
//! local engine. `google` is the free Google endpoint (FLAC upload). `energy` is the recorder that decides where a phrase starts and ends in the audio.
//!
//! `recorder` pushes what a device hears onto `queue`s (the energy-threshold recorder, or the Silero one), and
//! `session` runs a recorder, a transcriber and the threads between them for one microphone or speaker,
//! reporting a failure once (`failure`). `native` decides, from the settings, which device, recorder and
//! engine a session uses and builds them on a `Platform` (on Windows, `audio::raw`).

pub mod clip;
pub mod cloud;
pub mod deepgram;
pub mod energy;
pub mod failure;
pub mod google;
pub mod languages;
pub mod native;
pub mod openai;
pub mod phrases;
pub mod queue;
pub mod recorder;
pub mod session;
pub mod whisper;
