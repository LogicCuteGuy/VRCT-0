//! Checks against the machine's real audio devices. They open the default microphone, so they never run
//! with the rest: ignored unless asked for by name, and then they listen for a few seconds.
//!
//!     cargo test -p vrct-core --test hardware -- --ignored --nocapture
#![cfg(windows)]

use std::thread;
use std::time::{Duration, Instant};

use vrct_core::audio::raw::WasapiPlatform;
use vrct_core::transcription::native::Platform;
use vrct_core::transcription::queue::Queue;
use vrct_core::transcription::recorder::EnergyParams;
use vrct_core::transcription::session::Kind;

#[test]
#[ignore = "opens the default microphone"]
fn the_default_microphone_reports_levels_through_the_energy_recorder() {
    let platform = WasapiPlatform::locate();
    let devices = platform.devices();
    let name = devices.default_mic.clone().expect("this machine has a default microphone");
    let mic = devices.resolve_mic(&name).expect("the default microphone is in the list").clone();
    println!("microphone: {} ({} channel(s), {} Hz)", mic.name, mic.channels, mic.default_sample_rate);

    let params = EnergyParams { energy_threshold: 300.0, dynamic_energy_threshold: true, phrase_time_limit: 3.0, record_timeout: 3.0 };
    let recorder = platform.energy_recorder(Kind::Mic, &mic, params).expect("the microphone opens");
    println!("recording {:?}", recorder.format());
    let (audio, energy) = (Queue::bounded(20), Queue::bounded(1000));
    recorder.record_into(audio.clone(), Some(energy.clone())).expect("listening starts");

    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(4) {
        thread::sleep(Duration::from_millis(50));
        assert!(!recorder.device_error().is_set(), "the device failed: {:?}", recorder.device_error().info());
    }
    recorder.stop();

    let mut levels = Vec::new();
    while let Some(level) = energy.try_pop() {
        levels.push(level);
    }
    println!("{} level reports, loudest {}, {} phrase(s) queued", levels.len(), levels.iter().max().unwrap_or(&0), audio.len());
    assert!(levels.len() > 20, "a microphone delivers about 15 chunks a second; got {}", levels.len());
}
