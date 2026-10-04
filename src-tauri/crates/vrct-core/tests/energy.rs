//! The energy-threshold recorder replays the historical `custom_speech_recognition`
//! contract in `fixtures/energy_golden.json`, frozen at `16cb286c`.
//! Provenance and native test commands: `fixtures/README.md`.
//!
//! Scripted reads, clock changes and stopping conditions are replayed here.
//! Phrases, energy, drifting thresholds, consumed reads and time must match Python.

use std::cell::Cell;
use std::io;
use std::path::PathBuf;
use std::rc::Rc;

use serde_json::Value;
use sha2::{Digest, Sha256};
use vrct_core::transcription::energy::{rms, run_listener, Clock, Control, Phrase, Settings, Source, Timing};

fn golden() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/energy_golden.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// `sample_values` of the generator: a waveform of loudness about `amp`, clipped to `width` bytes.
fn pcm(amp: i64, samples: usize, width: usize) -> Vec<u8> {
    let limit = (1i64 << (8 * width - 1)) - 1;
    let pattern = [amp, -(amp * 3 / 4), amp / 2, -(amp / 3)];
    let mut out = Vec::with_capacity(samples * width);
    for i in 0..samples {
        let value = pattern[i % 4].clamp(-limit - 1, limit);
        out.extend_from_slice(&value.to_le_bytes()[..width]);
    }
    out
}

struct FakeClock {
    now: Cell<f64>,
    sleeps: Cell<u32>,
    control: Control,
    resume_after: u32,
}

impl Clock for FakeClock {
    fn now(&self) -> f64 {
        self.now.get()
    }

    fn sleep(&self, seconds: f64) {
        self.now.set(self.now.get() + seconds);
        if (seconds - 0.1).abs() < 1e-12 {
            self.sleeps.set(self.sleeps.get() + 1);
            if self.control.is_paused() && self.sleeps.get().is_multiple_of(self.resume_after) {
                self.control.resume();
            }
        }
    }
}

struct Scripted<'a> {
    chunk: usize,
    rate: u32,
    width: usize,
    script: &'a [Value],
    stop_at: Option<usize>,
    index: usize,
    polls: u64,
    clock: Rc<FakeClock>,
    control: Control,
}

impl Source for Scripted<'_> {
    fn chunk(&self) -> usize {
        self.chunk
    }

    fn sample_rate(&self) -> u32 {
        self.rate
    }

    fn sample_width(&self) -> u32 {
        self.width as u32
    }

    fn available(&mut self) -> bool {
        let Some(item) = self.script.get(self.index) else { return true };
        if self.polls < item.get("polls_false").and_then(Value::as_u64).unwrap_or(0) {
            self.polls += 1;
            return false;
        }
        true
    }

    fn read(&mut self) -> io::Result<Vec<u8>> {
        if self.stop_at == Some(self.index) {
            self.control.stop();
        }
        let Some(item) = self.script.get(self.index) else {
            self.control.stop();
            return Ok(Vec::new());
        };
        self.index += 1;
        self.polls = 0;
        self.clock.now.set(self.clock.now.get() + item["advance"].as_f64().unwrap());
        if item.get("error").is_some() {
            return Err(io::Error::other("scripted read failure"));
        }
        let samples = item.get("samples").and_then(Value::as_u64).map_or(self.chunk, |n| n as usize);
        Ok(if samples == 0 { Vec::new() } else { pcm(item["amp"].as_i64().unwrap(), samples, self.width) })
    }
}

fn number(value: &Value) -> Option<f64> {
    value.as_f64()
}

#[test]
fn the_recorder_finds_phrases_as_the_original_does() {
    let golden = golden();
    let scenarios = golden["scenarios"].as_array().unwrap();
    assert!(scenarios.len() >= 300);
    let (mut phrases_seen, mut with_phrases, mut paused, mut io_ended) = (0, 0, 0, 0);

    for (number_of, scenario) in scenarios.iter().enumerate() {
        let p = &scenario["params"];
        let expected = &scenario["expected"];
        let label = format!("scenario {number_of} ({p})");

        let control = Control::new();
        let clock = Rc::new(FakeClock { now: Cell::new(1000.0), sleeps: Cell::new(0), control: control.clone(), resume_after: scenario["resume_after"].as_u64().unwrap() as u32 });
        let script = scenario["script"].as_array().unwrap();
        let mut source = Scripted {
            chunk: p["chunk"].as_u64().unwrap() as usize,
            rate: p["rate"].as_u64().unwrap() as u32,
            width: p["width"].as_u64().unwrap() as usize,
            script,
            stop_at: scenario["stop_at"].as_u64().map(|n| n as usize),
            index: 0,
            polls: 0,
            clock: clock.clone(),
            control: control.clone(),
        };
        let mut settings = Settings {
            energy_threshold: p["energy_threshold"].as_f64().unwrap(),
            dynamic_energy_threshold: p["dynamic"].as_bool().unwrap(),
            dynamic_energy_adjustment_damping: p["damping"].as_f64().unwrap(),
            dynamic_energy_ratio: p["ratio"].as_f64().unwrap(),
            pause_threshold: p["pause_threshold"].as_f64().unwrap(),
            phrase_threshold: p["phrase_threshold"].as_f64().unwrap(),
            non_speaking_duration: p["non_speaking_duration"].as_f64().unwrap(),
        };
        let timing = Timing {
            phrase_timeout: number(&p["timeout"]),
            phrase_time_limit: number(&p["phrase_time_limit"]),
            record_timeout: number(&p["record_timeout"]).unwrap_or(f64::INFINITY),
        };
        let pause_at = scenario["pause_at"].as_u64();

        let mut energies = Vec::new();
        let mut phrases: Vec<(Phrase, f64, f64)> = Vec::new();
        let ended = run_listener(
            &mut settings,
            &mut source,
            clock.as_ref(),
            &control,
            timing,
            &mut |energy| energies.push(energy),
            &mut |settings, phrase| {
                phrases.push((phrase, settings.energy_threshold, clock.now.get()));
                if pause_at == Some(phrases.len() as u64) {
                    control.pause();
                }
            },
        );
        if ended.is_err() {
            io_ended += 1;
        }

        let want = expected["phrases"].as_array().unwrap();
        assert_eq!(want.len(), phrases.len(), "{label}: number of phrases");
        for (index, (want, (got, threshold, at))) in want.iter().zip(&phrases).enumerate() {
            let at_label = format!("{label}, phrase {index}");
            assert_eq!(want["length"].as_u64().unwrap() as usize, got.raw_data().len(), "{at_label}: length");
            assert_eq!(want["sha"].as_str().unwrap(), hex::encode(Sha256::digest(got.raw_data())), "{at_label}: audio");
            assert_eq!(want["threshold"].as_f64().unwrap(), *threshold, "{at_label}: threshold");
            assert_eq!(want["clock"].as_f64().unwrap(), *at, "{at_label}: clock");
            assert_eq!(want["rate"].as_u64().unwrap() as u32, got.sample_rate, "{at_label}: rate");
            assert_eq!(want["width"].as_u64().unwrap() as u32, got.sample_width, "{at_label}: width");
        }
        let want_energies: Vec<u32> = expected["energies"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as u32).collect();
        assert_eq!(want_energies, energies, "{label}: energies");
        assert_eq!(expected["final_threshold"].as_f64().unwrap(), settings.energy_threshold, "{label}: final threshold");
        assert_eq!(expected["reads"].as_u64().unwrap() as usize, source.index, "{label}: reads");
        assert_eq!(expected["clock"].as_f64().unwrap(), clock.now.get(), "{label}: clock at the end");
        assert_eq!(expected["sleeps"].as_u64().unwrap() as u32, clock.sleeps.get(), "{label}: sleeps");

        phrases_seen += phrases.len();
        with_phrases += usize::from(!phrases.is_empty());
        paused += usize::from(pause_at.is_some_and(|n| phrases.len() as u64 >= n));
    }
    eprintln!("{} scenarios: {phrases_seen} phrases in {with_phrases}, {paused} paused and resumed, {io_ended} ended by a failed read", scenarios.len());
    assert!(phrases_seen >= 300 && with_phrases >= 100 && paused >= 30 && io_ended >= 5);
}

#[test]
fn rms_is_audioops() {
    assert_eq!(0, rms(&[], 2));
    assert_eq!(16, rms(&[0x10, 0x00, 0xf0, 0xff], 2));
    assert_eq!(100, rms(&[100, 0x9c], 1));
    assert_eq!(3, rms(&[3, 0, 0, 0xfd, 0xff, 0xff], 3));
    // The mean square is taken in double precision and truncated, not rounded.
    assert_eq!(1, rms(&[1, 0, 2, 0], 2));
    assert_eq!(32768, rms(&i16::MIN.to_le_bytes(), 2));
    assert_eq!(2147483648, rms(&i32::MIN.to_le_bytes(), 4));
}
