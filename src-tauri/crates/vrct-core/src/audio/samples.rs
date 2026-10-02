//! Raw device samples to the 16-bit little endian PCM `normalize` takes.
//!
//! PortAudio opened every stream as int16 and let the host API convert. WASAPI in shared
//! mode (what cpal gives) delivers the device's own format, usually 32-bit float, so the
//! conversion happens here. Interleaved channels stay interleaved; one sample maps to one sample.

/// The formats a device can hand over, with the byte layout cpal uses (native endian, little on Windows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawFormat {
    U8,
    I8,
    I16,
    U16,
    /// 24 significant bits stored in a 32-bit word as a plain integer (range ±2^23).
    I24,
    I32,
    F32,
    F64,
}

impl RawFormat {
    pub fn bytes_per_sample(self) -> usize {
        match self {
            RawFormat::U8 | RawFormat::I8 => 1,
            RawFormat::I16 | RawFormat::U16 => 2,
            RawFormat::I24 | RawFormat::I32 | RawFormat::F32 => 4,
            RawFormat::F64 => 8,
        }
    }

    /// Converts whole samples; a trailing partial sample is ignored.
    pub fn to_pcm16(self, raw: &[u8]) -> Vec<u8> {
        let width = self.bytes_per_sample();
        let mut out = Vec::with_capacity(raw.len() / width * 2);
        for sample in raw.chunks_exact(width) {
            out.extend_from_slice(&self.sample_to_i16(sample).to_le_bytes());
        }
        out
    }

    fn sample_to_i16(self, sample: &[u8]) -> i16 {
        match self {
            RawFormat::U8 => ((sample[0] as i16) - 128) << 8,
            RawFormat::I8 => (sample[0] as i8 as i16) << 8,
            RawFormat::I16 => i16::from_le_bytes([sample[0], sample[1]]),
            RawFormat::U16 => (u16::from_le_bytes([sample[0], sample[1]]) as i32 - 32768) as i16,
            RawFormat::I24 => (i32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]) >> 8) as i16,
            RawFormat::I32 => (i32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]) >> 16) as i16,
            RawFormat::F32 => float_to_i16(f32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]) as f64),
            RawFormat::F64 => float_to_i16(f64::from_le_bytes([
                sample[0], sample[1], sample[2], sample[3], sample[4], sample[5], sample[6], sample[7],
            ])),
        }
    }
}

/// Full scale is ±1.0; louder is clipped, not wrapped. NaN is silence (an `as` cast maps it to 0).
fn float_to_i16(value: f64) -> i16 {
    (value.clamp(-1.0, 1.0) * 32767.0).round() as i16
}
