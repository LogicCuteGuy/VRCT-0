//! The device side of the energy-threshold recorder, without a device: how a `RawSource` hands out what the
//! audio thread collected. A PortAudio stream's `read(CHUNK)` waits for a whole chunk, and it must not be
//! possible to stay stuck in it once the stream is closed or has failed.
#![cfg(windows)]

use std::io;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use vrct_core::audio::raw::{mix_down, RawSource};
use vrct_core::transcription::energy::Source;

const CHUNK_BYTES_MONO: usize = 1024 * 2;

fn ramp(from: usize, bytes: usize) -> Vec<u8> {
    (from..from + bytes).map(|i| (i % 251) as u8).collect()
}

#[test]
fn it_reports_the_format_of_the_stream() {
    let (source, _feed, _closer) = RawSource::detached(44_100, 2, 1 << 20);
    assert_eq!((source.chunk(), source.sample_rate(), source.sample_width(), source.channels()), (1024, 44_100, 2, 2));
}

#[test]
fn a_read_waits_for_a_whole_chunk() {
    let (mut source, feed, _closer) = RawSource::detached(16_000, 1, 1 << 20);
    let (tx, rx) = mpsc::channel();
    let reader = thread::spawn(move || tx.send(source.read()).unwrap());

    feed.push(&ramp(0, CHUNK_BYTES_MONO / 2));
    assert!(rx.recv_timeout(Duration::from_millis(150)).is_err(), "half a chunk is not enough");
    feed.push(&ramp(CHUNK_BYTES_MONO / 2, CHUNK_BYTES_MONO / 2));
    let chunk = rx.recv_timeout(Duration::from_secs(5)).expect("a whole chunk is there").unwrap();
    assert_eq!(chunk, ramp(0, CHUNK_BYTES_MONO));
    reader.join().unwrap();
}

#[test]
fn chunks_come_out_in_order_and_the_rest_waits() {
    let (mut source, feed, _closer) = RawSource::detached(16_000, 1, 1 << 20);
    feed.push(&ramp(0, CHUNK_BYTES_MONO * 2 + 100));
    assert!(source.available());
    assert_eq!(source.read().unwrap(), ramp(0, CHUNK_BYTES_MONO));
    assert_eq!(source.read().unwrap(), ramp(CHUNK_BYTES_MONO, CHUNK_BYTES_MONO));
    assert!(source.available(), "the 100 bytes left over are audio too");
}

#[test]
fn a_chunk_is_a_whole_number_of_frames_of_every_channel() {
    let (mut source, feed, _closer) = RawSource::detached(48_000, 2, 1 << 20);
    feed.push(&ramp(0, 1024 * 4));
    assert_eq!(source.read().unwrap().len(), 1024 * 4, "1024 frames of two 16-bit channels");
}

#[test]
fn nothing_is_available_before_any_audio_arrives() {
    let (mut source, _feed, _closer) = RawSource::detached(16_000, 1, 1 << 20);
    assert!(!source.available());
}

#[test]
fn only_recent_audio_is_kept_when_nobody_reads() {
    // Room for two chunks; five arrive. The oldest three are gone, and so is the start of the fourth only if
    // that is needed to stay in whole frames.
    let (mut source, feed, _closer) = RawSource::detached(16_000, 1, CHUNK_BYTES_MONO * 2);
    for chunk in 0..5 {
        feed.push(&ramp(chunk * CHUNK_BYTES_MONO, CHUNK_BYTES_MONO));
    }
    assert_eq!(source.read().unwrap(), ramp(3 * CHUNK_BYTES_MONO, CHUNK_BYTES_MONO));
    assert_eq!(source.read().unwrap(), ramp(4 * CHUNK_BYTES_MONO, CHUNK_BYTES_MONO));
}

#[test]
fn dropping_old_audio_never_splits_a_frame() {
    // Stereo: a frame is 4 bytes. Two bytes too many are kept as a whole frame less, not as a shifted stream.
    let (mut source, feed, _closer) = RawSource::detached(16_000, 2, 4096 + 2);
    feed.push(&ramp(0, 4096 + 4));
    assert_eq!(source.read().unwrap(), ramp(4, 4096));
}

#[test]
fn closing_ends_a_read_that_is_waiting() {
    let (mut source, _feed, closer) = RawSource::detached(16_000, 1, 1 << 20);
    let (tx, rx) = mpsc::channel();
    let reader = thread::spawn(move || tx.send(source.read()).unwrap());
    thread::sleep(Duration::from_millis(100));
    closer.close();
    let outcome = rx.recv_timeout(Duration::from_secs(5)).expect("the read returned");
    assert_eq!(outcome.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    reader.join().unwrap();
}

#[test]
fn a_failed_stream_ends_the_read_with_its_message() {
    let (mut source, feed, _closer) = RawSource::detached(16_000, 1, 1 << 20);
    feed.fail("device unplugged");
    let error = source.read().unwrap_err();
    assert!(error.to_string().contains("device unplugged"), "{error}");
}

#[test]
fn a_failed_stream_keeps_failing_instead_of_waiting_for_audio_that_cannot_come() {
    let (mut source, feed, _closer) = RawSource::detached(16_000, 1, 1 << 20);
    feed.fail("device unplugged");
    assert!(source.read().is_err());
    assert!(source.read().is_err(), "a second read does not block");
}

#[test]
fn audio_collected_before_a_close_is_not_handed_out_after_it() {
    let (mut source, feed, closer) = RawSource::detached(16_000, 1, 1 << 20);
    feed.push(&ramp(0, CHUNK_BYTES_MONO));
    closer.close();
    assert!(source.read().is_err());
}

#[test]
fn stereo_is_mixed_down_by_averaging_each_frame() {
    let pcm: Vec<u8> = [1000i16, 3000, -2000, -4000, 7, 8, i16::MAX, i16::MAX].iter().flat_map(|s| s.to_le_bytes()).collect();
    let mixed: Vec<i16> = mix_down(&pcm, 2).chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
    assert_eq!(mixed, [2000, -3000, 7, i16::MAX], "(7 + 8) / 2 truncates toward zero");
}

#[test]
fn more_than_two_channels_are_averaged_too_and_a_partial_frame_is_ignored() {
    let samples = [300i16, 600, 900, -300, -600, -900, 5];
    let pcm: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let mixed: Vec<i16> = mix_down(&pcm, 3).chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
    assert_eq!(mixed, [600, -600]);
}
