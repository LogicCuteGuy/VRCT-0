//! Google's free speech endpoint (`speech-api/v2/recognize`, the one `speech_recognition` calls the
//! "Google Speech Recognition API"): a FLAC clip is posted, newline-separated JSON blocks come back.
//! A port of `GoogleProvider` and `Recognizer.recognize_google(.., with_confidence=True,
//! join_all_results=True)` from `custom_speech_recognition` 3.10.4.5.
//!
//! The fork's change over the original library is `join_all_results`: a clip with a pause in it can come
//! back as one block per utterance, and every block's best transcript is joined, with the confidences
//! averaged, instead of only the first block being kept.
//!
//! The FLAC is written by [`flac`] (pure Rust). It is not the bitstream Python's `flac --best` makes, but it
//! decodes to the same samples, which is all the endpoint looks at.

use std::time::Duration;

use serde_json::Value;

use super::clip::to_mono_16bit;
use super::cloud::{other, CloudRecognizer};
use super::languages;
use super::phrases::{Recognition, RecognizeError, Request};
use crate::translation::http::post_bytes;

pub const ENDPOINT: &str = "http://www.google.com/speech-api/v2/recognize";
/// The key `speech_recognition` falls back to; Google may revoke it at any time (and has said so).
pub const DEFAULT_KEY: &str = "AIzaSyBOti4mM-6x9WDnZIjIeyEU21OpBXqWBgw";
/// `GOOGLE_RECOGNIZE_TIMEOUT_SECONDS`: the recorder's `operation_timeout`.
const WAIT: Duration = Duration::from_secs(10);

pub struct GoogleProvider {
    key: String,
    endpoint: String,
    wait: Duration,
}

impl GoogleProvider {
    pub fn new() -> Self {
        GoogleProvider { key: DEFAULT_KEY.into(), endpoint: ENDPOINT.into(), wait: WAIT }
    }

    pub fn with_key(mut self, key: &str) -> Self {
        self.key = key.into();
        self
    }

    /// Another server to ask (tests use a local one).
    pub fn with_endpoint(mut self, endpoint: &str) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    /// How long to wait for an answer.
    pub fn with_wait(mut self, wait: Duration) -> Self {
        self.wait = wait;
        self
    }
}

impl Default for GoogleProvider {
    fn default() -> Self {
        Self::new()
    }
}

/// The shortest block FLAC decoders accept (`FLAC__MIN_BLOCK_SIZE`).
const MIN_BLOCK: usize = 16;

/// A FLAC file holding `pcm16` (mono, 16-bit little endian samples) at `sample_rate`.
///
/// The encoder records the shortest block it wrote, the last one included, as the stream's minimum, which
/// strict decoders refuse below 16 samples. A clip that would end in such a block gets up to 15 samples
/// of silence (under a millisecond) added to it.
pub fn flac(pcm16: &[u8], sample_rate: u32) -> Result<Vec<u8>, String> {
    use flacenc::component::BitRepr;
    use flacenc::error::Verify;

    let mut samples: Vec<i32> = pcm16.chunks_exact(2).map(|pair| i32::from(i16::from_le_bytes([pair[0], pair[1]]))).collect();
    let config = flacenc::config::Encoder::default().into_verified().map_err(|(_, error)| format!("FLAC settings: {error}"))?;
    let last_block = if samples.len() > config.block_size { samples.len() % config.block_size } else { samples.len() };
    if last_block != 0 && last_block < MIN_BLOCK {
        samples.resize(samples.len() + MIN_BLOCK - last_block, 0);
    }
    let source = flacenc::source::MemSource::from_samples(&samples, 1, 16, sample_rate as usize);
    let stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size).map_err(|error| format!("FLAC encoding: {error}"))?;
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream.write(&mut sink).map_err(|error| format!("FLAC output: {error}"))?;
    Ok(sink.as_slice().to_vec())
}

/// What the endpoint made of the clip.
#[derive(Debug, Clone, PartialEq)]
pub enum Heard {
    /// Nothing recognised (`UnknownValueError`).
    Nothing,
    Text { text: String, confidence: f64 },
}

fn failure(kind: &str) -> RecognizeError {
    other(kind)
}

/// Python's `len()`, for the JSON types it works on.
fn length(value: &Value) -> Result<usize, RecognizeError> {
    match value {
        Value::Array(items) => Ok(items.len()),
        Value::Object(map) => Ok(map.len()),
        Value::String(text) => Ok(text.chars().count()),
        _ => Err(failure("TypeError")),
    }
}

/// `recognize_google`'s reading of the reply: the best transcript of every block that has one, joined with
/// spaces, with the average of their confidences. Failures are the exceptions Python raised on odd replies.
pub fn parse_reply(reply: &str) -> Result<Heard, RecognizeError> {
    // Every non-empty line is a JSON block; blocks with an empty `result` are skipped.
    let mut matched: Vec<Value> = Vec::new();
    for line in reply.split('\n') {
        if line.is_empty() {
            continue;
        }
        let block: Value = serde_json::from_str(line).map_err(|_| failure("JSONDecodeError"))?;
        let result = match &block {
            Value::Object(map) => map.get("result").ok_or_else(|| failure("KeyError"))?,
            _ => return Err(failure("TypeError")),
        };
        if length(result)? == 0 {
            continue;
        }
        match result {
            Value::Array(items) => matched.push(items[0].clone()),
            // A string's first character is no dictionary: it is skipped below, as in Python.
            Value::String(text) => matched.push(Value::String(text.chars().take(1).collect())),
            _ => return Err(failure("KeyError")),
        }
    }

    let mut transcripts: Vec<Value> = Vec::new();
    let mut confidences: Vec<Value> = Vec::new();
    for candidate in &matched {
        let Value::Object(candidate) = candidate else { continue };
        let alternatives = match candidate.get("alternative") {
            None => continue,
            Some(alternatives) => alternatives,
        };
        if length(alternatives)? == 0 {
            continue;
        }
        let best = match alternatives {
            // `"confidence" in candidate["alternative"]` asks whether the list holds that very string, which a
            // list of dictionaries never does: the first alternative is always the one taken.
            Value::Array(items) => {
                if items.iter().any(|item| item.as_str() == Some("confidence")) {
                    return Err(failure("TypeError"));
                }
                items[0].clone()
            }
            Value::String(_) => continue,
            _ => return Err(failure("KeyError")),
        };
        match &best {
            Value::Object(best) => {
                let Some(transcript) = best.get("transcript") else { continue };
                transcripts.push(transcript.clone());
                confidences.push(best.get("confidence").cloned().unwrap_or_else(|| Value::from(0.5)));
            }
            Value::String(text) => {
                if text.contains("transcript") {
                    return Err(failure("TypeError"));
                }
            }
            Value::Array(items) => {
                if items.iter().any(|item| item.as_str() == Some("transcript")) {
                    return Err(failure("TypeError"));
                }
            }
            _ => return Err(failure("TypeError")),
        }
    }

    if transcripts.is_empty() {
        return Ok(Heard::Nothing);
    }
    let mut parts = Vec::with_capacity(transcripts.len());
    for transcript in &transcripts {
        parts.push(transcript.as_str().ok_or_else(|| failure("TypeError"))?);
    }
    let mut total = 0.0f64;
    for confidence in &confidences {
        total += match confidence {
            Value::Number(number) => number.as_f64().unwrap_or(f64::NAN),
            Value::Bool(flag) => f64::from(u8::from(*flag)),
            _ => return Err(failure("TypeError")),
        };
    }
    Ok(Heard::Text { text: parts.join(" "), confidence: total / confidences.len() as f64 })
}

impl CloudRecognizer for GoogleProvider {
    async fn recognize(&self, request: &Request<'_>) -> Result<Recognition, RecognizeError> {
        let language = languages::code(request.language, request.country, "Google").ok_or_else(|| failure("KeyError"))?;
        // `get_flac_data(convert_rate=None if rate >= 8000 else 8000, convert_width=2)`: the clip keeps its own
        // rate (the header below says so too). Rates under 8 kHz would be resampled; no device offers them.
        if request.format.sample_rate < 8000 {
            return Err(failure("error"));
        }
        let mono = to_mono_16bit(request.pcm, request.format).map_err(|_| failure("error"))?;
        let clip = flac(&mono, request.format.sample_rate).map_err(|_| failure("error"))?;

        let query = [("client", "chromium".to_string()), ("lang", language.to_string()), ("key", self.key.clone()), ("pFilter", "0".to_string())];
        let content_type = format!("audio/x-flac; rate={}", request.format.sample_rate);
        let reply = post_bytes(&self.endpoint, &[], &query, &content_type, &clip, self.wait, 1).await.map_err(|_| failure("RequestError"))?;
        // `urlopen` raises on 4xx and 5xx (`HTTPError`), which the library reports as `RequestError`.
        if reply.status >= 400 {
            return Err(failure("RequestError"));
        }
        let text = String::from_utf8(reply.body).map_err(|_| failure("UnicodeDecodeError"))?;
        Ok(match parse_reply(&text)? {
            Heard::Nothing => Recognition { text: String::new(), confidence: 0.0, definitive: false },
            Heard::Text { text, confidence } => Recognition { text, confidence, definitive: false },
        })
    }
}
