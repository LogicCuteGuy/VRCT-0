//! `audio::normalize` and `audio::vad` against what the Python code did on the
//! same input (`fixtures/regenerate_audio_golden.py` records it).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use sha2::{Digest, Sha256};
use vrct_core::audio::normalize::Pcm16MonoNormalizer;
use vrct_core::audio::vad::{FrameProbability, SegmentIds, SpeechSegment, VadConfig, VadSegmenter};
use vrct_core::audio::{FRAME_SAMPLES, TARGET_SAMPLE_RATE};

fn fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap()
}

fn number(value: &Value, key: &str) -> usize {
    value[key].as_u64().unwrap() as usize
}

// ---- normalizer ----

#[test]
fn normalizer_matches_python_on_every_chunk() {
    let golden = fixture("audio_normalizer_golden.json");
    let cases = golden["cases"].as_array().unwrap();
    assert!(cases.len() >= 20);
    let mut errors = 0;
    for (index, case) in cases.iter().enumerate() {
        let mut normalizer = Pcm16MonoNormalizer::new(
            number(case, "sample_rate") as u32,
            number(case, "sample_width"),
            number(case, "channels"),
        );
        for (number_of_chunk, chunk) in case["chunks"].as_array().unwrap().iter().enumerate() {
            let input = hex::decode(text(chunk, "input")).unwrap();
            let got = normalizer.process(&input);
            let label = format!("case {index} ({case}) chunk {number_of_chunk}", case = case["sample_rate"]);
            if chunk["error"].as_bool().unwrap() {
                errors += 1;
                assert!(got.is_err(), "{label}: Python raised, Rust returned {} bytes", got.unwrap().len());
            } else {
                let got = got.unwrap_or_else(|e| panic!("{label}: {e}"));
                assert_eq!(hex::encode(got), text(chunk, "output"), "{label}");
            }
        }
    }
    assert!(errors >= 4, "the golden should cover error chunks");
}

#[test]
fn normalizer_reset_drops_the_resampler_state() {
    let golden = fixture("audio_normalizer_golden.json");
    let case = &golden["reset_case"];
    let mut normalizer = Pcm16MonoNormalizer::new(
        number(case, "sample_rate") as u32,
        number(case, "sample_width"),
        number(case, "channels"),
    );
    for step in case["steps"].as_array().unwrap() {
        if step.get("reset").is_some() {
            normalizer.reset();
            continue;
        }
        let input = hex::decode(text(step, "input")).unwrap();
        assert_eq!(hex::encode(normalizer.process(&input).unwrap()), text(step, "output"));
    }
}

// ---- segmenter ----

/// The probability engine of the golden: a script, plus a fingerprint of each frame it was given.
#[derive(Default)]
struct Script {
    probs: Vec<f32>,
    calls: Vec<[f64; 3]>,
    resets: usize,
}

#[derive(Clone)]
struct ScriptedEngine(Arc<Mutex<Script>>);

impl FrameProbability for ScriptedEngine {
    fn probability(&mut self, frame: &[f32]) -> Result<f32, String> {
        assert_eq!(frame.len(), FRAME_SAMPLES);
        let mut script = self.0.lock().unwrap();
        let index = script.calls.len();
        script.calls.push([f64::from(frame[0]), f64::from(frame[255]), f64::from(frame[FRAME_SAMPLES - 1])]);
        Ok(script.probs[index])
    }

    fn reset(&mut self) {
        self.0.lock().unwrap().resets += 1;
    }
}

fn frame_bytes(k: usize) -> Vec<u8> {
    (0..FRAME_SAMPLES)
        .flat_map(|i| ((((k * 977 + i * 31 + 12345) % 65536) as i64 - 32768) as i16).to_le_bytes())
        .collect()
}

fn pcm(start: usize, frames: usize, extra: usize) -> Vec<u8> {
    let mut data: Vec<u8> = (0..frames).flat_map(|n| frame_bytes(start + n)).collect();
    data.extend_from_slice(&frame_bytes(start + frames)[..extra]);
    data
}

fn same_segment(got: &SpeechSegment, expected: &Value, context: &str) {
    assert_eq!(got.audio.len(), number(expected, "len"), "{context}: length");
    assert_eq!(hex::encode(Sha256::digest(&got.audio)), text(expected, "sha256"), "{context}: audio");
    assert_eq!(got.segment_id, expected["id"].as_u64().unwrap(), "{context}: id");
    assert_eq!(got.reason.as_str(), text(expected, "reason"), "{context}: reason");
}

fn config(params: &Value) -> VadConfig {
    VadConfig {
        speech_threshold: params["speech_threshold"].as_f64().unwrap(),
        negative_threshold: params["negative_threshold"].as_f64(),
        hangover_frames: number(params, "hangover_frames"),
        max_speech_frames: params["max_speech_frames"].as_u64().map(|n| n as usize),
        min_speech_frames: number(params, "min_speech_frames"),
        pre_speech_pad_frames: number(params, "pre_speech_pad_frames"),
        label: "mic".to_string(),
    }
}

#[test]
fn segmenter_matches_python_in_every_scenario() {
    let golden = fixture("audio_vad_golden.json");
    let scenarios = golden.as_array().unwrap();
    assert!(scenarios.len() >= 20);
    for scenario in scenarios {
        let name = text(scenario, "name");
        let script = Arc::new(Mutex::new(Script {
            probs: scenario["probs"].as_array().unwrap().iter().map(|p| p.as_f64().unwrap() as f32).collect(),
            ..Script::default()
        }));
        let logs = Arc::new(Mutex::new(Vec::<String>::new()));
        let mut segmenter = VadSegmenter::with_ids(ScriptedEngine(script.clone()), config(&scenario["params"]), SegmentIds::starting_at(0));
        let sink = logs.clone();
        segmenter.on_diagnostic(move |line| sink.lock().unwrap().push(line.to_string()));

        for (number_of_step, step) in scenario["steps"].as_array().unwrap().iter().enumerate() {
            let context = format!("{name} step {number_of_step}");
            let expected = &step["result"];
            match text(step, "op") {
                "process" => {
                    let segments = segmenter.process(&pcm(number(step, "start"), number(step, "frames"), number(step, "extra"))).unwrap();
                    let wanted = expected["segments"].as_array().unwrap();
                    assert_eq!(segments.len(), wanted.len(), "{context}: segment count");
                    for (got, want) in segments.iter().zip(wanted) {
                        same_segment(got, want, &context);
                    }
                }
                "flush" => match (segmenter.flush().unwrap(), &expected["segment"]) {
                    (None, Value::Null) => {}
                    (Some(got), want @ Value::Object(_)) => same_segment(&got, want, &context),
                    (got, want) => panic!("{context}: flush gave {got:?}, Python gave {want}"),
                },
                "reset" => segmenter.reset(),
                other => panic!("unknown op {other}"),
            }
            assert_eq!(segmenter.speaking(), expected["speaking"].as_bool().unwrap(), "{context}: speaking");
            let script = script.lock().unwrap();
            assert_eq!(script.calls.len(), number(expected, "engine_calls"), "{context}: engine calls");
            assert_eq!(script.resets, number(expected, "engine_resets"), "{context}: engine resets");
        }

        let script = script.lock().unwrap();
        let calls: Vec<[f64; 3]> = scenario["calls"]
            .as_array()
            .unwrap()
            .iter()
            .map(|call| {
                let call = call.as_array().unwrap();
                [call[0].as_f64().unwrap(), call[1].as_f64().unwrap(), call[2].as_f64().unwrap()]
            })
            .collect();
        assert_eq!(script.calls, calls, "{name}: the frames the engine saw");
        let expected_logs: Vec<&str> = scenario["logs"].as_array().unwrap().iter().map(|l| l.as_str().unwrap()).collect();
        assert_eq!(*logs.lock().unwrap(), expected_logs, "{name}: diagnostic lines");
    }
}

#[test]
fn segment_ids_come_from_one_shared_counter() {
    let ids = SegmentIds::starting_at(10);
    let speaker = || VadSegmenter::with_ids(
        ScriptedEngine(Arc::new(Mutex::new(Script { probs: vec![0.9; 6], ..Script::default() }))),
        VadConfig::default(),
        ids.clone(),
    );
    let (mut first, mut second) = (speaker(), speaker());

    let speak = |segmenter: &mut VadSegmenter<ScriptedEngine>| {
        assert!(segmenter.process(&pcm(0, 3, 0)).unwrap().is_empty());
        segmenter.flush().unwrap().unwrap().segment_id
    };
    // Each took an id when it was made; ending a segment takes the next free one.
    assert_eq!(speak(&mut first), 10);
    assert_eq!(speak(&mut second), 11);
    assert_eq!(speak(&mut first), 12);
    assert_eq!(speak(&mut second), 13);
}

/// An engine that fails on its second frame.
struct Failing(usize);

impl FrameProbability for Failing {
    fn probability(&mut self, _frame: &[f32]) -> Result<f32, String> {
        self.0 += 1;
        if self.0 == 2 {
            Err("model failed".to_string())
        } else {
            Ok(0.0)
        }
    }

    fn reset(&mut self) {}
}

#[test]
fn an_engine_error_stops_process_and_drops_only_the_failed_frame() {
    let mut segmenter = VadSegmenter::with_ids(Failing(0), VadConfig::default(), SegmentIds::starting_at(0));
    assert_eq!(segmenter.process(&pcm(0, 3, 0)).unwrap_err(), "model failed");
    // Frames 1 and 2 are gone; frame 3 is still queued and is scored by the next call.
    assert!(segmenter.process(&[]).unwrap().is_empty());
    assert_eq!(segmenter.engine().0, 3);
    assert!(segmenter.process(&[]).unwrap().is_empty());
    assert_eq!(segmenter.engine().0, 3, "nothing is left to score");
}

#[test]
fn an_engine_error_in_flush_is_reported() {
    let mut segmenter = VadSegmenter::with_ids(Failing(1), VadConfig::default(), SegmentIds::starting_at(0));
    segmenter.process(&pcm(0, 0, 100)).unwrap();
    assert_eq!(segmenter.flush().unwrap_err(), "model failed");
}

#[test]
fn duration_follows_the_target_rate() {
    let segment = SpeechSegment {
        audio: vec![0; (TARGET_SAMPLE_RATE as usize) * 2],
        segment_id: 0,
        reason: vrct_core::audio::vad::SegmentEnd::Flush,
    };
    assert_eq!(segment.duration_ms(), 1000.0);
}
