//! A recorded clip as a recognition engine takes it: 16 kHz mono 16-bit PCM, or a WAV file of that.
//!
//! Python's `AudioData.get_raw_data(convert_rate=16000, convert_width=2)` / `get_wav_data(...)`
//! did the conversion; `audio::normalize` already ports the `audioop` calls it used (to the
//! bit). A stereo clip is mixed down by the mean of its channels; Python's `AudioFile` and
//! `pydub` rounded that mix differently, which can move a sample by one step at most.

use crate::audio::normalize::Pcm16MonoNormalizer;

use super::phrases::Format;

/// `pcm` in `format`, as 16 kHz mono 16-bit little endian.
pub fn to_16k_mono(pcm: &[u8], format: Format) -> Result<Vec<u8>, String> {
    Pcm16MonoNormalizer::new(format.sample_rate, format.sample_width as usize, format.channels as usize).process(pcm)
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
