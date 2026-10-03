//! A recorded clip as a recognition engine takes it: 16 kHz mono 16-bit PCM, or a WAV file of that.
//!
//! Python's `AudioData.get_raw_data(convert_rate=16000, convert_width=2)` / `get_wav_data(...)`
//! did the conversion; `audio::normalize` already ports the `audioop` calls it used (to the bit).
//!
//! Stereo audio (a speaker's loopback, in the energy-threshold recorder) reaches the engines through
//! `speech_recognition.AudioFile`, which turns it into mono with `audioop.tomono(frames, width, 1, 1)`:
//! the two channels are ADDED, not averaged, and the sum is clipped. [`tomono_sum`] does the same, so the
//! engines hear what they heard before. (The VAD path hands over 16 kHz mono and never gets here.)

use crate::audio::normalize::{to_i16, Pcm16MonoNormalizer};

use super::phrases::Format;

/// `audioop.tomono(pcm, width, 1, 1)` on interleaved stereo: left plus right of every frame, clipped to
/// the range of `width` bytes.
pub fn tomono_sum(pcm: &[u8], width: usize) -> Result<Vec<u8>, String> {
    if !(1..=4).contains(&width) {
        return Err(format!("unsupported sample width {width}"));
    }
    if !pcm.len().is_multiple_of(width * 2) {
        return Err("not a whole number of frames".to_string());
    }
    let (min, max) = (-(2f64.powi(8 * width as i32 - 1)), 2f64.powi(8 * width as i32 - 1) - 1.0);
    let sample = |bytes: &[u8]| -> f64 {
        match width {
            1 => f64::from(bytes[0] as i8),
            2 => f64::from(i16::from_le_bytes([bytes[0], bytes[1]])),
            3 => f64::from((i32::from(bytes[2] as i8) << 16) | (i32::from(bytes[1]) << 8) | i32::from(bytes[0])),
            _ => f64::from(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])),
        }
    };
    let mut out = Vec::with_capacity(pcm.len() / 2);
    for frame in pcm.chunks_exact(width * 2) {
        let sum = (sample(&frame[..width]) + sample(&frame[width..])).clamp(min, max).floor() as i64;
        out.extend_from_slice(&sum.to_le_bytes()[..width]);
    }
    Ok(out)
}

/// `pcm` in `format`, as 16 kHz mono 16-bit little endian.
pub fn to_16k_mono(pcm: &[u8], format: Format) -> Result<Vec<u8>, String> {
    let width = format.sample_width as usize;
    if format.channels == 2 {
        return Pcm16MonoNormalizer::new(format.sample_rate, width, 1).process(&tomono_sum(pcm, width)?);
    }
    Pcm16MonoNormalizer::new(format.sample_rate, width, format.channels as usize).process(pcm)
}

/// `pcm` in `format`, as mono 16-bit little endian at its own sample rate (`get_flac_data(convert_width=2)`).
pub fn to_mono_16bit(pcm: &[u8], format: Format) -> Result<Vec<u8>, String> {
    let width = format.sample_width as usize;
    let mono = if format.channels == 2 { tomono_sum(pcm, width)? } else { pcm.to_vec() };
    Ok(to_i16(&mono, width)?.iter().flat_map(|sample| sample.to_le_bytes()).collect())
}

/// The bytes of a mono 16-bit WAV file around `pcm16`: the 44-byte header Python's `wave` module
/// writes, then the samples.
pub fn wav_16bit_mono(pcm16: &[u8], sample_rate: u32) -> Vec<u8> {
    wav(pcm16, sample_rate, 2, 1)
}

/// A WAV file with a canonical 44-byte header.
pub fn wav(pcm: &[u8], sample_rate: u32, sample_width: u16, channels: u16) -> Vec<u8> {
    let block_align = sample_width * channels;
    let data_len = pcm.len() as u32;
    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * u32::from(block_align)).to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&(sample_width * 8).to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

/// What the cloud engines upload for a request's clip.
pub fn wav_for_upload(pcm: &[u8], format: Format) -> Result<Vec<u8>, String> {
    Ok(wav_16bit_mono(&to_16k_mono(pcm, format)?, 16_000))
}
