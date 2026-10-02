//! `transcription::phrases` against what `AudioTranscriber.transcribeAudioQueue` did on the same
//! queues (`fixtures/regenerate_phrases_golden.py` records it): what is sent for recognition and
//! when, what is left in the buffer, the transcript list and the counters after every call.

use std::collections::VecDeque;
use std::path::PathBuf;

use serde_json::Value;
use sha2::{Digest, Sha256};
use vrct_core::audio::vad::SegmentEnd;
use vrct_core::transcription::phrases::{
    pad_with_silence, Chunk, Engine, Format, PhraseTranscriber, Query, Recognition, RecognizeError, Recognizer, Request,
    Settings, Stamp,
};

fn golden() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/phrases_golden.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// byte i = (seed + 7 * i) % 251, as the generator makes it.
fn audio_bytes(len: usize, seed: u64) -> Vec<u8> {
    (0..len).map(|i| ((seed + 7 * (i as u64 % 251)) % 251) as u8).collect()
}

/// Answers from the recorded replies in order and notes every request.
struct Scripted {
    name: String,
    replies: VecDeque<Value>,
    calls: Vec<Value>,
}

impl Recognizer for Scripted {
    fn recognize(&mut self, request: &Request<'_>) -> Result<Recognition, RecognizeError> {
        self.calls.push(serde_json::json!({
            "language": request.language,
            "country": request.country,
            "len": request.pcm.len(),
            "sha": hex::encode(Sha256::digest(request.pcm)),
            "kwargs": {
                "avg_logprob": request.avg_logprob,
                "no_speech_prob": request.no_speech_prob,
                "no_repeat_ngram_size": request.no_repeat_ngram_size,
                "force_language": request.force_language,
            },
        }));
        let reply = self.replies.pop_front().unwrap_or_else(|| panic!("{}: the Rust side asked more than Python did", self.name));
        if reply.get("unknown").is_some() {
            Err(RecognizeError::NoMatch)
        } else if let Some(code) = reply.get("api_error") {
            Err(RecognizeError::Api { code: code.as_str().unwrap().to_string() })
        } else if reply.get("error").is_some() {
            Err(RecognizeError::Other { kind: "RuntimeError".to_string() })
        } else {
            Ok(Recognition {
                text: reply["text"].as_str().unwrap().to_string(),
                confidence: reply["confidence"].as_f64().unwrap(),
                definitive: reply["definitive"].as_bool().unwrap(),
            })
        }
    }
}

fn engine(name: &str) -> Engine {
    match name {
        "Google" => Engine::Google,
        "Whisper" => Engine::Whisper,
        _ => Engine::Cloud,
    }
}

fn end_of(reason: &Value) -> Option<SegmentEnd> {
    match reason.as_str() {
        Some("silence") => Some(SegmentEnd::Silence),
        Some("flush") => Some(SegmentEnd::Flush),
        Some("max_duration") => Some(SegmentEnd::MaxDuration),
        _ => None,
    }
}

fn strings(value: &Value) -> Vec<String> {
    value.as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect()
}

fn ms(stamp: Option<Stamp>) -> Value {
    stamp.map_or(Value::Null, |s| Value::from(s.0 / 1000))
}

fn transcript_json(text: &str, confidence: f64, language: Option<&str>) -> Value {
    serde_json::json!({"text": text, "confidence": confidence, "language": language})
}

fn drained_json(t: &mut PhraseTranscriber) -> Vec<Value> {
    let mut out = Vec::new();
    while t.has_transcript() {
        let got = t.take_transcript();
        out.push(transcript_json(&got.text, got.confidence, got.language.as_deref()));
    }
    out.push(serde_json::json!({"empty": t.take_transcript().text}));
    out
}

fn state_json(t: &PhraseTranscriber, queue_left: usize, transcripts: Vec<Value>) -> Value {
    serde_json::json!({
        "last_sample_len": t.buffered_len(),
        "last_spoken_ms": ms(t.last_spoken()),
        "phrase_started_ms": ms(t.phrase_started_at()),
        "queue_left": queue_left,
        "transcripts": transcripts,
        "last_recognition_error": t.last_recognition_error(),
        "last_api_error_code": t.last_api_error_code(),
        "asr_attempts": t.asr_attempts(),
        "asr_successes": t.asr_successes(),
    })
}

#[test]
fn every_scenario_sends_and_keeps_what_python_did() {
    let golden = golden();
    let scenarios = golden["scenarios"].as_array().unwrap();
    assert!(scenarios.len() >= 340);
    let (mut sent, mut raised, mut kept) = (0, 0, 0);

    for scenario in scenarios {
        let name = scenario["name"].as_str().unwrap();
        let cfg = &scenario["config"];
        let settings = Settings {
            speaker: cfg["speaker"].as_bool().unwrap(),
            format: Format {
                sample_rate: cfg["sample_rate"].as_u64().unwrap() as u32,
                sample_width: cfg["sample_width"].as_u64().unwrap() as u32,
                channels: cfg["channels"].as_u64().unwrap() as u32,
            },
            phrase_timeout: cfg["phrase_timeout"].as_i64().unwrap(),
            max_phrases: cfg["max_phrases"].as_i64().unwrap(),
            engine: engine(cfg["engine"].as_str().unwrap()),
            segmented: cfg["vad_segmented"].as_bool().unwrap(),
        };
        let mut transcriber = PhraseTranscriber::new(settings.clone());
        let mut recognizer = Scripted { name: name.to_string(), replies: scenario["replies"].as_array().unwrap().iter().cloned().collect(), calls: Vec::new() };
        let mut queue: VecDeque<Chunk> = VecDeque::new();

        for (index, step) in scenario["steps"].as_array().unwrap().iter().enumerate() {
            let label = format!("{name}, step {index}");
            let input = &scenario["inputs"][index];
            for chunk in input["chunks"].as_array().unwrap() {
                queue.push_back(Chunk {
                    data: audio_bytes(chunk["len"].as_u64().unwrap() as usize, chunk["seed"].as_u64().unwrap()),
                    at: Stamp::from_millis(chunk["at_ms"].as_i64().unwrap()),
                    end: if settings.segmented { end_of(&chunk["reason"]) } else { None },
                });
            }
            let languages = strings(&input["languages"]);
            let countries = strings(&input["countries"]);
            let query = Query {
                languages: &languages,
                countries: &countries,
                avg_logprob: input["avg_logprob"].as_f64().unwrap(),
                no_speech_prob: input["no_speech_prob"].as_f64().unwrap(),
                no_repeat_ngram_size: input["no_repeat_ngram_size"].as_u64().unwrap() as u32,
            };
            let calls_before = recognizer.calls.len();
            let outcome = transcriber.transcribe_queue(
                &mut queue,
                Some(&mut recognizer),
                &query,
                Stamp::from_millis(input["now_ms"].as_i64().unwrap()),
            );

            match (&step["error"], &outcome) {
                (Value::Null, Ok(result)) => assert_eq!(step["result"].as_bool().unwrap(), *result, "{label}: result"),
                (Value::Null, Err(error)) => panic!("{label}: Rust failed with {error:?}, Python did not"),
                (expected, Ok(result)) => panic!("{label}: Python raised {expected}, Rust returned {result}"),
                (expected, Err(error)) => {
                    raised += 1;
                    assert_eq!("ASR_ERROR", expected["code"], "{label}");
                    assert_eq!("asr", expected["stage"], "{label}");
                    assert_eq!(expected["source"].as_str().unwrap(), error.source, "{label}");
                    assert_eq!(expected["exception_type"].as_str().unwrap(), error.exception_type, "{label}");
                }
            }

            let calls = &recognizer.calls[calls_before..];
            sent += calls.len();
            assert_eq!(step["calls"].as_array().unwrap().as_slice(), calls, "{label}: what was sent for recognition");

            let snapshot: Vec<Value> =
                transcriber.transcripts().map(|t| transcript_json(&t.text, t.confidence, t.language.as_deref())).collect();
            kept += snapshot.len();
            assert_eq!(step["state"], state_json(&transcriber, queue.len(), snapshot), "{label}: state afterwards");

            if input["drain"].as_bool().unwrap() {
                assert_eq!(step["drained"].as_array().unwrap().as_slice(), drained_json(&mut transcriber).as_slice(), "{label}: delivered");
            }
            if input["clear"].as_bool().unwrap() {
                transcriber.clear();
                assert_eq!(
                    step["after_clear"],
                    serde_json::json!({
                        "last_sample_len": transcriber.buffered_len(),
                        "last_spoken_ms": ms(transcriber.last_spoken()),
                        "phrase_started_ms": ms(transcriber.phrase_started_at()),
                        "transcripts": usize::from(transcriber.has_transcript()),
                    }),
                    "{label}: after clear"
                );
            }
        }
        assert!(recognizer.replies.is_empty(), "{name}: Python used replies that Rust did not ask for");
    }
    assert!(sent > 2000 && raised > 100 && kept > 300, "the golden should exercise sends, failures and kept transcripts");
}

#[test]
fn padding_uses_the_rate_and_width_truncated() {
    let data = [1u8, 2, 3];
    let padded = pad_with_silence(&data, Format { sample_rate: 16000, sample_width: 2, channels: 1 });
    assert_eq!(padded.len(), 9600 + 3 + 16000);
    assert!(padded[..9600].iter().all(|b| *b == 0) && padded[9600..9603] == data && padded[9603..].iter().all(|b| *b == 0));
    // 11025 * 3 / 1000 = 33.075 bytes per ms: 9922.5 and 16537.5 truncate.
    let odd = pad_with_silence(&[], Format { sample_rate: 11025, sample_width: 3, channels: 1 });
    assert_eq!(odd.len(), 9922 + 16537);
}
