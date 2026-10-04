//! Weight-free local Whisper components replay historical faster-whisper behavior:
//! log-mel features, model requests and segment handling.
//! `fixtures/whisper_golden.json` is frozen at `16cb286c`; see `fixtures/README.md`.

use std::path::PathBuf;
use std::sync::Mutex;

use base64::Engine as _;
use serde_json::Value;
use sha2::{Digest, Sha256};
use vrct_core::transcription::phrases::{Format, RecognizeError, Recognizer, Request};
use vrct_core::transcription::whisper::features::{FeatureExtractor, LogMel, NB_MAX_FRAMES};
use vrct_core::transcription::whisper::provider::{Info, LocalWhisper, Options, Segment, Transcribe};

fn golden() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/whisper_golden.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn floats(encoded: &str) -> Vec<f32> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(encoded).unwrap();
    bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()
}

fn max_difference(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

#[test]
fn the_mel_filter_bank_is_faster_whispers() {
    let golden = golden();
    for size in [80usize, 128] {
        let expected = floats(golden["mel_filters"][size.to_string()]["data"].as_str().unwrap());
        let got = FeatureExtractor::new(size).mel_filters().to_vec();
        let worst = max_difference(&expected, &got);
        assert!(worst < 1e-6, "{size} mel bands: filters differ by {worst}");
    }
}

#[test]
fn the_log_mel_spectrogram_is_faster_whispers() {
    let golden = golden();
    let cases = golden["features"].as_array().unwrap();
    assert!(cases.len() >= 12);
    let mut worst_overall = 0.0f32;
    for case in cases {
        let label = format!("{} with {} mel bands", case["label"].as_str().unwrap(), case["feature_size"]);
        let samples = floats(case["samples"].as_str().unwrap());
        let size = case["feature_size"].as_u64().unwrap() as usize;
        let mel = FeatureExtractor::new(size).compute(&samples);
        let shape = (case["shape"][0].as_u64().unwrap() as usize, case["shape"][1].as_u64().unwrap() as usize);
        assert_eq!(shape, (mel.n_mels, mel.frames), "{label}: shape");
        let worst = max_difference(&floats(case["features"].as_str().unwrap()), &mel.data);
        assert!(worst < 2e-4, "{label}: values differ by up to {worst}");
        worst_overall = worst_overall.max(worst);
    }
    eprintln!("largest difference from numpy: {worst_overall}");
}

#[test]
fn slicing_and_padding_follow_numpy() {
    let mel = LogMel { n_mels: 2, frames: 4, data: vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0] };
    assert_eq!(vec![2.0, 3.0, 6.0, 7.0], mel.slice(1, 2).data);
    assert_eq!(vec![3.0, 4.0, 7.0, 8.0], mel.slice(2, 10).data);
    assert_eq!(vec![1.0, 2.0, 5.0, 6.0], mel.pad_or_trim(2).data);
    assert_eq!(vec![1.0, 2.0, 3.0, 4.0, 0.0, 0.0, 5.0, 6.0, 7.0, 8.0, 0.0, 0.0], mel.pad_or_trim(6).data);
    assert_eq!(3000, NB_MAX_FRAMES);
}

// ---- the provider ----

/// What the provider asked the model: samples, language, options.
type Asked = (Vec<f32>, Option<String>, Options);

struct Scripted {
    segments: Vec<Segment>,
    info: Info,
    asked: Mutex<Vec<Asked>>,
}

impl Transcribe for Scripted {
    fn transcribe(&self, samples: &[f32], language: Option<&str>, options: &Options) -> Result<(Vec<Segment>, Info), String> {
        self.asked.lock().unwrap().push((samples.to_vec(), language.map(str::to_string), options.clone()));
        Ok((self.segments.clone(), self.info.clone()))
    }
}

#[test]
fn the_provider_asks_and_filters_as_python_did() {
    let golden = golden();
    let cases = golden["provider"].as_array().unwrap();
    assert!(cases.len() >= 100);
    let pcm: Vec<u8> = (0..3200).map(|i| ((i * 37 + 11) % 256) as u8).collect();
    let (mut answered, mut key_errors) = (0, 0);
    for case in cases {
        let label = format!(
            "{} ({}/{} force={} limits={}/{})",
            case["script"], case["language"], case["country"], case["force_language"], case["avg_logprob"], case["no_speech_prob"]
        );
        let segments = case["segments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| Segment {
                text: s[0].as_str().unwrap().to_string(),
                avg_logprob: s[1].as_f64().unwrap(),
                no_speech_prob: s[2].as_f64().unwrap(),
            })
            .collect();
        let info = Info {
            language: case["info"]["language"].as_str().unwrap().to_string(),
            language_probability: case["info"]["language_probability"].as_f64().unwrap(),
        };
        let mut engine = LocalWhisper::new(Scripted { segments, info, asked: Mutex::new(Vec::new()) });
        let got = engine.recognize(&Request {
            pcm: &pcm,
            format: Format { sample_rate: 16000, sample_width: 2, channels: 1 },
            language: case["language"].as_str().unwrap(),
            country: case["country"].as_str().unwrap(),
            avg_logprob: case["avg_logprob"].as_f64().unwrap(),
            no_speech_prob: case["no_speech_prob"].as_f64().unwrap(),
            no_repeat_ngram_size: case["no_repeat_ngram_size"].as_u64().unwrap() as u32,
            force_language: case["force_language"].as_bool().unwrap(),
        });

        match (case["result"].get("ok"), case["result"].get("other"), &got) {
            (Some(ok), _, Ok(found)) => {
                answered += 1;
                assert_eq!(ok["text"].as_str().unwrap(), found.text, "{label}");
                assert_eq!(ok["confidence"].as_f64().unwrap(), found.confidence, "{label}");
                assert_eq!(ok["definitive"].as_bool().unwrap(), found.definitive, "{label}");
            }
            (_, Some(kind), Err(RecognizeError::Other { kind: got })) => {
                key_errors += 1;
                assert_eq!(kind.as_str().unwrap(), got, "{label}");
            }
            _ => panic!("{label}: Python gave {}, Rust gave {got:?}", case["result"]),
        }

        let asked = engine.model().asked.lock().unwrap();
        match (case["asked"].as_object(), asked.first()) {
            (Some(expected), Some((samples, language, options))) => {
                let kwargs = &expected["kwargs"];
                assert_eq!(kwargs["language"].as_str(), language.as_deref(), "{label}: language");
                assert_eq!(kwargs["log_prob_threshold"].as_f64().unwrap(), options.avg_logprob, "{label}");
                assert_eq!(kwargs["no_speech_threshold"].as_f64().unwrap(), options.no_speech_prob, "{label}");
                assert_eq!(kwargs["no_repeat_ngram_size"].as_u64().unwrap() as u32, options.no_repeat_ngram_size, "{label}");
                assert_eq!(expected["samples"].as_u64().unwrap() as usize, samples.len(), "{label}");
                let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
                assert_eq!(expected["samples_sha"].as_str().unwrap(), hex::encode(Sha256::digest(&bytes)), "{label}: samples");
            }
            (None, None) => {}
            (expected, got) => panic!("{label}: Python asked the model {expected:?}, Rust {got:?}"),
        }
    }
    assert!(answered > 80 && key_errors >= 20);
}
