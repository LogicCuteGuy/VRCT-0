//! Any PCM to 16 kHz mono signed 16-bit: `Pcm16MonoNormalizer`.
//!
//! Python does this with `audioop.lin2lin` (sample width), a numpy channel mean
//! and `audioop.ratecv` (rate). `audioop` is removed from Python 3.13, so the
//! two routines are ported here, bit for bit, instead of being approximated by
//! a resampling library: segment boundaries and the audio handed to the
//! recogniser should not shift when the backend moves.

use super::TARGET_SAMPLE_RATE;

pub struct Pcm16MonoNormalizer {
    sample_rate: u32,
    sample_width: usize,
    channels: usize,
    rate_state: Option<RateState>,
}

/// `audioop.ratecv`'s state for one channel: the position in the output
/// sample grid and the last two (filtered) input samples.
#[derive(Clone, Copy)]
struct RateState {
    d: i32,
    prev: i32,
    cur: i32,
}

impl Pcm16MonoNormalizer {
    pub fn new(sample_rate: u32, sample_width: usize, channels: usize) -> Self {
        Self { sample_rate, sample_width, channels: channels.max(1), rate_state: None }
    }

    pub fn reset(&mut self) {
        self.rate_state = None;
    }

    /// The error cases are the ones Python raises on: a width other than 1-4,
    /// bytes that are not whole samples. A failed call leaves the state alone.
    pub fn process(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        if data.is_empty() {
            return Ok(Vec::new());
        }

        let mut samples = to_i16(data, self.sample_width)?;
        samples.truncate(samples.len() - samples.len() % self.channels);
        if self.channels > 1 {
            samples = samples.chunks_exact(self.channels).map(mean).collect();
        }

        let rate = i64::from(TARGET_SAMPLE_RATE);
        if i64::from(self.sample_rate) == rate {
            return Ok(samples.iter().flat_map(|sample| sample.to_le_bytes()).collect());
        }
        let (out, state) = ratecv(&samples, self.sample_rate as i32, TARGET_SAMPLE_RATE as i32, self.rate_state);
        self.rate_state = Some(state);
        Ok(out.iter().flat_map(|sample| sample.to_le_bytes()).collect())
    }
}

/// `audioop.lin2lin(data, width, 2)` for widths 1-4, and the plain reading of 16-bit data.
pub(crate) fn to_i16(data: &[u8], width: usize) -> Result<Vec<i16>, String> {
    if !(1..=4).contains(&width) {
        return Err(format!("unsupported sample width {width}"));
    }
    if !data.len().is_multiple_of(width) {
        return Err("not a whole number of samples".to_string());
    }
    Ok(match width {
        1 => data.iter().map(|&byte| i16::from(byte as i8) << 8).collect(),
        2 => data.chunks_exact(2).map(|pair| i16::from_le_bytes([pair[0], pair[1]])).collect(),
        3 => data
            .chunks_exact(3)
            .map(|triple| (i32::from_le_bytes([0, triple[0], triple[1], triple[2]]) >> 16) as i16)
            .collect(),
        _ => data
            .chunks_exact(4)
            .map(|quad| (i32::from_le_bytes([quad[0], quad[1], quad[2], quad[3]]) >> 16) as i16)
            .collect(),
    })
}

/// `np.rint(frame.astype(int32).mean()).clip(-32768, 32767)`: ties go to the even integer.
fn mean(frame: &[i16]) -> i16 {
    let sum: i64 = frame.iter().map(|&sample| i64::from(sample)).sum();
    (sum as f64 / frame.len() as f64).round_ties_even().clamp(-32768.0, 32767.0) as i16
}

fn gcd(mut a: i32, mut b: i32) -> i32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// `audioop.ratecv(data, 2, 1, in_rate, out_rate, state)` with the default filter weights
/// (weightA 1, weightB 0, which makes the filter a no-op).
fn ratecv(input: &[i16], in_rate: i32, out_rate: i32, state: Option<RateState>) -> (Vec<i16>, RateState) {
    let divisor = gcd(in_rate, out_rate);
    let (in_rate, out_rate) = (in_rate / divisor, out_rate / divisor);

    let mut state = state.unwrap_or(RateState { d: -out_rate, prev: 0, cur: 0 });
    let mut out = Vec::new();
    let mut rest = input.iter();
    loop {
        while state.d < 0 {
            let Some(&sample) = rest.next() else {
                return (out, state);
            };
            state.prev = state.cur;
            state.cur = i32::from(sample) << 16;
            state.d += out_rate;
        }
        while state.d >= 0 {
            let mixed = f64::from(state.prev) * f64::from(state.d) + f64::from(state.cur) * f64::from(out_rate - state.d);
            out.push(((mixed / f64::from(out_rate)) as i32 >> 16) as i16);
            state.d -= in_rate;
        }
    }
}
