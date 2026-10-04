//! Whisper inference replays historical faster-whisper output on real weights/speech.
//! `fixtures/whisper_e2e_golden.json` is frozen at `16cb286c`; see `fixtures/README.md`.
//! Requires `ct2`, faster-whisper-small weights and the recorded clips.
//! Missing external model/audio files are reported and skipped; fixtures are not regenerated.

#![cfg(feature = "ct2")]

use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};
use vrct_core::transcription::whisper::model::WhisperModel;
use vrct_core::transcription::whisper::provider::{Options, Transcribe};

fn golden() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/whisper_e2e_golden.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn model_dir() -> Option<PathBuf> {
    if let Some(given) = std::env::var_os("VRCT_WHISPER_MODEL_SMALL") {
        return Some(PathBuf::from(given));
    }
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    let snapshots = PathBuf::from(home).join(".cache/huggingface/hub/models--Systran--faster-whisper-small/snapshots");
    std::fs::read_dir(snapshots).ok()?.flatten().map(|e| e.path()).find(|p| p.join("model.bin").exists())
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/whisper_fixtures")
}

/// 16 kHz mono 16-bit samples of a WAV file.
fn read_wav(path: &Path) -> Option<Vec<u8>> {
    let bytes = std::fs::read(path).ok()?;
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().ok()?) as usize;
        if &bytes[at..at + 4] == b"data" {
            return Some(bytes[at + 8..(at + 8 + size).min(bytes.len())].to_vec());
        }
        at += 8 + size + (size & 1);
    }
    None
}

fn model() -> Option<WhisperModel> {
    let dir = model_dir()?;
    Some(WhisperModel::load(&dir, "cpu", 0, "int8", 4).expect("the model loads"))
}

fn the_suppressed_tokens_are_faster_whispers(model: &WhisperModel) {
    let golden = golden();
    let expected: Vec<i32> = golden["suppressed_tokens"].as_array().unwrap().iter().map(|v| v.as_i64().unwrap() as i32).collect();
    assert_eq!(expected, model.suppressed_tokens());
    assert!(model.is_multilingual());
}

fn the_prompt_is_faster_whispers(model: &WhisperModel) {
    let golden = golden();
    let prompts = golden["prompts"].as_array().unwrap();
    assert!(prompts.len() >= 6);
    for case in prompts {
        let previous: Vec<usize> = case["previous"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as usize).collect();
        let expected: Vec<u32> = case["prompt"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as u32).collect();
        let language = case["language"].as_str().unwrap();
        assert_eq!(expected, model.prompt_for(&previous, language).unwrap(), "{language} after {} tokens", previous.len());
    }
}

fn speech_is_recognised_as_faster_whisper_does(model: &WhisperModel) {
    let golden = golden();
    let (mut compared, mut worst_logprob, mut worst_probability) = (0, 0.0f64, 0.0f64);
    for case in golden["cases"].as_array().unwrap() {
        let clip = case["clip"].as_str().unwrap();
        let label = format!("{clip} language={} limits={}/{}/{}", case["language"], case["avg_logprob"], case["no_speech_prob"], case["no_repeat_ngram_size"]);
        let Some(pcm) = read_wav(&fixtures().join(format!("{clip}.wav"))) else {
            eprintln!("skipped {label}: the clip is not in target/whisper_fixtures (run the generator)");
            continue;
        };
        if hex::encode(Sha256::digest(&pcm)) != case["clip_sha"].as_str().unwrap() {
            eprintln!("skipped {label}: this clip is not the one the golden was made from");
            continue;
        }
        let samples = vrct_core::transcription::whisper::provider::samples_of(&pcm);
        let options = Options {
            avg_logprob: case["avg_logprob"].as_f64().unwrap(),
            no_speech_prob: case["no_speech_prob"].as_f64().unwrap(),
            no_repeat_ngram_size: case["no_repeat_ngram_size"].as_u64().unwrap() as u32,
        };
        let started = std::time::Instant::now();
        let (segments, info) = model.transcribe(&samples, case["language"].as_str(), &options).expect(&label);
        eprintln!("{label}: {:.1}s", started.elapsed().as_secs_f32());

        assert_eq!(case["info"]["language"].as_str().unwrap(), info.language, "{label}: language");
        let probability_gap = (case["info"]["language_probability"].as_f64().unwrap() - info.language_probability).abs();
        // ruy's int8 kernels are not MKL's: a clip with nothing to hear (probability 0.66) moves by 0.03.
        assert!(probability_gap < 0.05, "{label}: language probability differs by {probability_gap}");
        let expected = case["segments"].as_array().unwrap();
        if matches!(clip, "noise" | "silence") {
            // With nothing to hear, Whisper hallucinates (a token, or a repeating one); whether it does and
            // how long the loop runs hangs on last-bit differences between ruy's int8 kernels and MKL's,
            // and the thresholds sit right on top of the scores. Only the language is compared.
            worst_probability = worst_probability.max(probability_gap);
            compared += 1;
            continue;
        }
        assert_eq!(
            expected.iter().map(|s| s["text"].as_str().unwrap().to_string()).collect::<Vec<_>>(),
            segments.iter().map(|s| s.text.clone()).collect::<Vec<_>>(),
            "{label}: text"
        );
        for (want, got) in expected.iter().zip(&segments) {
            let logprob_gap = (want["avg_logprob"].as_f64().unwrap() - got.avg_logprob).abs();
            let no_speech_gap = (want["no_speech_prob"].as_f64().unwrap() - got.no_speech_prob).abs();
            assert!(logprob_gap < 0.05, "{label}: avg_logprob differs by {logprob_gap}");
            assert!(no_speech_gap < 0.02, "{label}: no_speech_prob differs by {no_speech_gap}");
            worst_logprob = worst_logprob.max(logprob_gap);
        }
        worst_probability = worst_probability.max(probability_gap);
        compared += 1;
    }
    eprintln!("compared {compared} cases; worst avg_logprob gap {worst_logprob:.5}, worst language probability gap {worst_probability:.5}");
}

/// One test, and it ends the process: a thread that has run CTranslate2 hangs when it exits (its
/// thread-local ruy thread pool is joined from the loader-locked thread-exit callback, and the pool's
/// threads need that lock to finish), so the result is reported from the thread and the process exits
/// with it. Production code that runs the model has to keep its thread alive the same way.
#[test]
fn whisper_model_matches_faster_whisper() {
    let Some(model) = model() else { return eprintln!("skipped: no faster-whisper-small model here") };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        the_suppressed_tokens_are_faster_whispers(&model);
        the_prompt_is_faster_whispers(&model);
        speech_is_recognised_as_faster_whisper_does(&model);
    }));
    drop(model);
    match outcome {
        Ok(()) => {
            eprintln!("whisper_model_matches_faster_whisper ... ok");
            std::process::exit(0)
        }
        Err(_) => std::process::exit(101),
    }
}
