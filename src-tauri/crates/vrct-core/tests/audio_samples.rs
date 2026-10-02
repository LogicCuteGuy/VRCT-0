//! `audio::samples`: device sample formats to PCM16 little endian.

use vrct_core::audio::samples::RawFormat;

fn pcm(bytes: &[u8]) -> Vec<i16> {
    bytes.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect()
}

fn f32s(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

#[test]
fn float_full_scale_and_silence() {
    let out = pcm(&RawFormat::F32.to_pcm16(&f32s(&[0.0, 1.0, -1.0, 0.5, -0.5])));
    assert_eq!(out, vec![0, 32767, -32767, 16384, -16384]);
}

#[test]
fn float_beyond_full_scale_clips_and_nan_is_silence() {
    let out = pcm(&RawFormat::F32.to_pcm16(&f32s(&[1.5, -3.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY])));
    assert_eq!(out, vec![32767, -32767, 0, 32767, -32767]);
}

#[test]
fn float64_matches_float32() {
    let raw: Vec<u8> = [0.25f64, -0.75].iter().flat_map(|v| v.to_le_bytes()).collect();
    assert_eq!(pcm(&RawFormat::F64.to_pcm16(&raw)), vec![8192, -24575]);
}

#[test]
fn sixteen_bit_passes_through() {
    let raw: Vec<u8> = [0i16, 1, -1, 32767, -32768].iter().flat_map(|v| v.to_le_bytes()).collect();
    assert_eq!(RawFormat::I16.to_pcm16(&raw), raw);
}

#[test]
fn wider_integers_keep_the_top_sixteen_bits() {
    let i32s: Vec<u8> = [0i32, i32::MAX, i32::MIN, 0x0001_8000].iter().flat_map(|v| v.to_le_bytes()).collect();
    assert_eq!(pcm(&RawFormat::I32.to_pcm16(&i32s)), vec![0, 32767, -32768, 1]);
    let i24s: Vec<u8> = [0i32, (1 << 23) - 1, -(1 << 23), 0x100].iter().flat_map(|v| v.to_le_bytes()).collect();
    assert_eq!(pcm(&RawFormat::I24.to_pcm16(&i24s)), vec![0, 32767, -32768, 1]);
}

#[test]
fn unsigned_and_eight_bit_formats_are_centred() {
    assert_eq!(pcm(&RawFormat::U8.to_pcm16(&[128, 255, 0])), vec![0, 127 << 8, -32768]);
    assert_eq!(pcm(&RawFormat::I8.to_pcm16(&[0, 127, 0x80])), vec![0, 127 << 8, -32768]);
    let u16s: Vec<u8> = [32768u16, 0, 65535].iter().flat_map(|v| v.to_le_bytes()).collect();
    assert_eq!(pcm(&RawFormat::U16.to_pcm16(&u16s)), vec![0, -32768, 32767]);
}

#[test]
fn a_partial_trailing_sample_is_ignored_and_empty_gives_empty() {
    assert_eq!(RawFormat::I16.to_pcm16(&[1, 0, 2]).len(), 2);
    assert!(RawFormat::F32.to_pcm16(&[]).is_empty());
    assert!(RawFormat::F32.to_pcm16(&[0, 0, 0]).is_empty());
}

#[test]
fn widths_are_what_the_device_buffers_use() {
    let widths: Vec<usize> = [RawFormat::U8, RawFormat::I16, RawFormat::I24, RawFormat::I32, RawFormat::F32, RawFormat::F64]
        .iter()
        .map(|f| f.bytes_per_sample())
        .collect();
    assert_eq!(widths, vec![1, 2, 4, 4, 4, 8]);
}
