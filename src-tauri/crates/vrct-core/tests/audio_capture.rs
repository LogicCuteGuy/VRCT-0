//! WASAPI capture. Only what needs no microphone is run here: refusing an unknown device and
//! opening the default speaker's loopback (it records what is playing; the bytes are only counted).

#![cfg(windows)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use vrct_core::audio::capture::{Capture, Source};
use vrct_core::audio::wasapi::list_devices;

#[test]
fn an_unknown_microphone_is_an_error_not_a_hang() {
    let error = Capture::start(Source::Microphone, "No Such Microphone (test)", |_| {}, |_| {}).err().expect("must fail");
    assert!(error.contains("no microphone named"), "{error}");
}

#[test]
fn an_unknown_speaker_is_an_error() {
    let error = Capture::start(Source::Speaker, "No Such Speaker (test) [Loopback]", |_| {}, |_| {}).err().expect("must fail");
    assert!(error.contains("no playback device named \"No Such Speaker (test)\""), "{error}");
}

#[test]
fn the_default_speakers_loopback_opens_runs_and_stops() {
    let Some(name) = list_devices().expect("listing").default_speaker else {
        eprintln!("no playback device, skipping");
        return;
    };
    let bytes = Arc::new(AtomicUsize::new(0));
    let counted = bytes.clone();
    let mut capture = Capture::start(
        Source::Speaker,
        &name,
        move |pcm| {
            assert_eq!(pcm.len() % 2, 0, "PCM16 comes in whole samples");
            counted.fetch_add(pcm.len(), Ordering::Relaxed);
        },
        |error| panic!("stream error: {error}"),
    )
    .expect("opening loopback");
    std::thread::sleep(Duration::from_millis(500));
    capture.stop();
    let seen = bytes.load(Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(bytes.load(Ordering::Relaxed), seen, "nothing arrives after stop");
    eprintln!("loopback delivered {seen} bytes of 16 kHz mono PCM16 in 0.5 s");
}
