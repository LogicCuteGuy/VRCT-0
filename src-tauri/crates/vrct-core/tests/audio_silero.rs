//! Native `audio::silero` replays the historical Python `SileroFrameProbability`
//! synthetic-audio contract in `fixtures/silero_golden.json`, frozen at `16cb286c`.
//! Provenance and native runtime preparation/test commands: `fixtures/README.md`.
//! These tests require the native ONNX Runtime and fail clearly if it is absent.

use std::path::PathBuf;
#[path = "common/onnxruntime.rs"]
mod onnxruntime;

use base64::Engine as _;
use serde_json::Value;
use sha2::{Digest, Sha256};
use vrct_core::audio::silero::SileroFrameProbability;
use vrct_core::audio::vad::{FrameProbability, VadConfig, VadSegmenter};
use vrct_core::audio::FRAME_SAMPLES;

fn fixture() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/silero_golden.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn engine() -> SileroFrameProbability {
    let path = onnxruntime::library();
    SileroFrameProbability::with_library(&path)
        .unwrap_or_else(|e| panic!("Cannot load native ONNX Runtime {}: {e}", path.display()))
}

fn decode(b64: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD.decode(b64).unwrap()
}

fn samples(pcm: &[u8]) -> Vec<f32> {
    pcm.chunks_exact(2).map(|pair| f32::from(i16::from_le_bytes([pair[0], pair[1]])) / 32768.0).collect()
}

#[test]
fn probabilities_match_python_frame_by_frame() {
    let mut engine = engine();
    let golden = fixture();
    let mut worst = 0.0f32;
    let mut checked = 0;
    for scenario in golden["scenarios"].as_array().unwrap() {
        let name = scenario["name"].as_str().unwrap();
        let wanted = scenario["probs"].as_array().unwrap();
        engine.reset();
        let mut index = 0;
        for op in scenario["ops"].as_array().unwrap() {
            if op.get("reset").is_some() {
                engine.reset();
                continue;
            }
            let frame = samples(&decode(op["pcm"].as_str().unwrap()));
            let got = engine.probability(&frame).unwrap();
            let want = wanted[index].as_f64().unwrap() as f32;
            let diff = (got - want).abs();
            worst = worst.max(diff);
            assert!(diff < 1e-5, "{name} frame {index}: Rust {got}, Python {want}");
            index += 1;
            checked += 1;
        }
        assert_eq!(index, wanted.len(), "{name}: frame count");
    }
    eprintln!("{checked} frames, largest difference {worst:e}");
    assert!(checked > 150);
}

#[test]
fn reset_puts_state_and_context_back() {
    let mut engine = engine();
    let golden = fixture();
    let scenario = golden["scenarios"].as_array().unwrap().iter().find(|s| s["name"] == "vowel_a").unwrap();
    let frames: Vec<Vec<f32>> =
        scenario["ops"].as_array().unwrap().iter().take(10).map(|op| samples(&decode(op["pcm"].as_str().unwrap()))).collect();
    let run = |engine: &mut SileroFrameProbability| -> Vec<f32> {
        frames.iter().map(|frame| engine.probability(frame).unwrap()).collect()
    };
    engine.reset();
    let first = run(&mut engine);
    let carried = run(&mut engine);
    assert_ne!(first, carried, "state must carry from frame to frame");
    engine.reset();
    assert_eq!(run(&mut engine), first, "after a reset the same frames give the same numbers");
}

#[test]
fn wrong_frame_size_is_an_error() {
    let mut engine = engine();
    assert!(engine.probability(&vec![0.0; FRAME_SAMPLES - 1]).is_err());
    assert!(engine.probability(&vec![0.0; FRAME_SAMPLES + 1]).is_err());
}

#[test]
fn the_whole_chain_cuts_the_same_segments_as_python() {
    let engine = engine();
    let golden = fixture();
    let pipeline = &golden["pipeline"];
    let pcm = decode(pipeline["pcm"].as_str().unwrap());
    let chunk = pipeline["chunk_bytes"].as_u64().unwrap() as usize;

    let mut segmenter = VadSegmenter::with_ids(engine, VadConfig::default(), vrct_core::audio::vad::SegmentIds::starting_at(0));
    let mut got = Vec::new();
    for part in pcm.chunks(chunk) {
        got.extend(segmenter.process(part).unwrap());
    }
    got.extend(segmenter.flush().unwrap());

    let wanted = pipeline["segments"].as_array().unwrap();
    assert!(!wanted.is_empty());
    assert_eq!(got.len(), wanted.len(), "segment count");
    for (segment, want) in got.iter().zip(wanted) {
        assert_eq!(segment.audio.len() as u64, want["len"].as_u64().unwrap(), "length");
        assert_eq!(hex::encode(Sha256::digest(&segment.audio)), want["sha256"].as_str().unwrap(), "audio");
        assert_eq!(segment.segment_id, want["id"].as_u64().unwrap(), "id");
        assert_eq!(segment.reason.as_str(), want["reason"].as_str().unwrap(), "reason");
    }
}
