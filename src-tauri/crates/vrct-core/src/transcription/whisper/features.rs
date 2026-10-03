//! The log-mel spectrogram Whisper takes as input: a port of faster-whisper's `FeatureExtractor`
//! (numpy), which in turn follows OpenAI's reference.
//!
//! Steps, in the order they run: append 160 zero samples; pad 200 samples each side by reflection;
//! frames of 400 samples every 160, multiplied by a Hann window; the magnitude squared of each
//! frame's real FFT; the mel filter bank; `log10` clamped at 1e-10; nothing lower than 8 below
//! the loudest value anywhere in the clip; scaled to about [-1, 1]. Computed in the same
//! precision as numpy (float32 arrays, the FFT in double precision), so the values agree to
//! about 1e-6, not bit for bit.

use std::f64::consts::PI;

pub const SAMPLE_RATE: usize = 16_000;
pub const N_FFT: usize = 400;
pub const HOP_LENGTH: usize = 160;
pub const CHUNK_LENGTH: usize = 30;
/// Samples in one window the model takes.
pub const N_SAMPLES: usize = CHUNK_LENGTH * SAMPLE_RATE;
/// Frames in one window the model takes.
pub const NB_MAX_FRAMES: usize = N_SAMPLES / HOP_LENGTH;
const N_BINS: usize = N_FFT / 2 + 1;
/// Zero samples appended before the transform (`padding=160` in numpy's version).
const PADDING: usize = 160;

/// A log-mel spectrogram: `n_mels` rows of `frames` values.
#[derive(Debug, Clone, PartialEq)]
pub struct LogMel {
    pub n_mels: usize,
    pub frames: usize,
    /// Row-major: value `(mel, frame)` is at `mel * frames + frame`.
    pub data: Vec<f32>,
}

impl LogMel {
    /// Frames `from..from + length` of every row, as a new spectrogram.
    pub fn slice(&self, from: usize, length: usize) -> LogMel {
        let to = (from + length).min(self.frames);
        let from = from.min(to);
        let frames = to - from;
        let mut data = Vec::with_capacity(self.n_mels * frames);
        for row in 0..self.n_mels {
            data.extend_from_slice(&self.data[row * self.frames + from..row * self.frames + to]);
        }
        LogMel { n_mels: self.n_mels, frames, data }
    }

    /// Exactly `length` frames: cut, or padded at the end with zeros (numpy's `pad_or_trim`).
    pub fn pad_or_trim(&self, length: usize) -> LogMel {
        let mut data = Vec::with_capacity(self.n_mels * length);
        for row in 0..self.n_mels {
            let values = &self.data[row * self.frames..(row + 1) * self.frames];
            data.extend_from_slice(&values[..values.len().min(length)]);
            data.resize((row + 1) * length, 0.0);
        }
        LogMel { n_mels: self.n_mels, frames: length, data }
    }
}

pub struct FeatureExtractor {
    n_mels: usize,
    mel_filters: Vec<f32>,
    window: Vec<f32>,
    fft: Fft,
}

impl FeatureExtractor {
    /// `n_mels` is 80 for most models and 128 for large-v3.
    pub fn new(n_mels: usize) -> Self {
        // np.hanning(n_fft + 1)[:-1]
        let window = (0..N_FFT).map(|n| (0.5 - 0.5 * (2.0 * PI * n as f64 / N_FFT as f64).cos()) as f32).collect();
        FeatureExtractor { n_mels, mel_filters: mel_filters(n_mels), window, fft: Fft::new(N_FFT) }
    }

    pub fn n_mels(&self) -> usize {
        self.n_mels
    }

    pub fn mel_filters(&self) -> &[f32] {
        &self.mel_filters
    }

    /// The log-mel spectrogram of 16 kHz samples in [-1, 1].
    pub fn compute(&self, samples: &[f32]) -> LogMel {
        let mut padded = Vec::with_capacity(samples.len() + PADDING);
        padded.extend_from_slice(samples);
        padded.resize(samples.len() + PADDING, 0.0);

        let reflected = reflect_pad(&padded, N_FFT / 2);
        let frames_total = 1 + (reflected.len() - N_FFT) / HOP_LENGTH;
        // The last frame is dropped (`stft[..., :-1]`).
        let frames = frames_total - 1;

        // magnitudes: N_BINS rows of `frames` values, float32.
        let mut magnitudes = vec![0.0f32; N_BINS * frames];
        let mut input = vec![(0.0f64, 0.0f64); N_FFT];
        let mut spectrum = vec![(0.0f64, 0.0f64); N_FFT];
        for frame in 0..frames {
            let start = frame * HOP_LENGTH;
            for (n, slot) in input.iter_mut().enumerate() {
                *slot = (f64::from(reflected[start + n] * self.window[n]), 0.0);
            }
            self.fft.transform(&input, &mut spectrum);
            for bin in 0..N_BINS {
                // complex64: both parts rounded to float32; abs() is float32 hypot, then squared.
                let (re, im) = (spectrum[bin].0 as f32, spectrum[bin].1 as f32);
                let magnitude = re.hypot(im);
                magnitudes[bin * frames + frame] = magnitude * magnitude;
            }
        }

        // mel_filters (n_mels x N_BINS) @ magnitudes (N_BINS x frames), float32.
        let mut log_spec = vec![0.0f32; self.n_mels * frames];
        for mel in 0..self.n_mels {
            let weights = &self.mel_filters[mel * N_BINS..(mel + 1) * N_BINS];
            let row = &mut log_spec[mel * frames..(mel + 1) * frames];
            for (bin, weight) in weights.iter().enumerate() {
                if *weight == 0.0 {
                    continue;
                }
                let source = &magnitudes[bin * frames..(bin + 1) * frames];
                for (out, value) in row.iter_mut().zip(source) {
                    *out += weight * value;
                }
            }
        }

        let mut loudest = f32::NEG_INFINITY;
        for value in log_spec.iter_mut() {
            *value = value.max(1e-10).log10();
            loudest = loudest.max(*value);
        }
        let floor = loudest - 8.0;
        for value in log_spec.iter_mut() {
            *value = (value.max(floor) + 4.0) / 4.0;
        }
        LogMel { n_mels: self.n_mels, frames, data: log_spec }
    }
}

/// `np.pad(samples, (pad, pad), mode="reflect")`: mirrored around the first and last sample, which
/// are not repeated; a pad longer than the signal keeps reflecting back and forth.
fn reflect_pad(samples: &[f32], pad: usize) -> Vec<f32> {
    let n = samples.len() as isize;
    let reflect = |i: isize| -> f32 {
        if n == 1 {
            return samples[0];
        }
        let period = 2 * (n - 1);
        let mut j = i.rem_euclid(period);
        if j >= n {
            j = period - j;
        }
        samples[j as usize]
    };
    (-(pad as isize)..n + pad as isize).map(reflect).collect()
}

/// The slaney-scaled mel filter bank of librosa as faster-whisper builds it, `n_mels` rows of
/// `N_BINS` weights, rounded to float32.
pub fn mel_filters(n_mels: usize) -> Vec<f32> {
    let sr = SAMPLE_RATE as f64;
    // np.fft.rfftfreq(n=n_fft, d=1/sr): k * (1 / (n * d))
    let d = 1.0 / sr;
    let scale = 1.0 / (N_FFT as f64 * d);
    let fft_freqs: Vec<f64> = (0..N_BINS).map(|k| k as f64 * scale).collect();

    // np.linspace(0, max_mel, n_mels + 2)
    let (min_mel, max_mel) = (0.0f64, 45.245_640_471_924_965f64);
    let count = n_mels + 2;
    let step = (max_mel - min_mel) / (count - 1) as f64;
    let mut mels: Vec<f64> = (0..count).map(|i| i as f64 * step + min_mel).collect();
    mels[count - 1] = max_mel;

    let f_sp = 200.0 / 3.0;
    let mut freqs: Vec<f64> = mels.iter().map(|m| 0.0 + f_sp * m).collect();
    let min_log_hz = 1000.0;
    let min_log_mel = (min_log_hz - 0.0) / f_sp;
    let logstep = 6.4f64.ln() / 27.0;
    for (freq, mel) in freqs.iter_mut().zip(&mels) {
        if *mel >= min_log_mel {
            *freq = min_log_hz * (logstep * (mel - min_log_mel)).exp();
        }
    }

    let fdiff: Vec<f64> = freqs.windows(2).map(|w| w[1] - w[0]).collect();
    let mut weights = vec![0.0f32; n_mels * N_BINS];
    for mel in 0..n_mels {
        let enorm = 2.0 / (freqs[mel + 2] - freqs[mel]);
        for (bin, fft_freq) in fft_freqs.iter().enumerate() {
            let lower = -(freqs[mel] - fft_freq) / fdiff[mel];
            let upper = (freqs[mel + 2] - fft_freq) / fdiff[mel + 1];
            weights[mel * N_BINS + bin] = (0.0f64.max(lower.min(upper)) * enorm) as f32;
        }
    }
    weights
}

/// A complex FFT of any length whose prime factors are small (400 = 2^4 * 5^2), by decimation in time.
struct Fft {
    n: usize,
    factors: Vec<usize>,
    /// `exp(-2 pi i k / n)` for `k` in `0..n`.
    twiddle: Vec<(f64, f64)>,
}

impl Fft {
    fn new(n: usize) -> Self {
        let mut factors = Vec::new();
        let mut rest = n;
        for prime in [2usize, 3, 5, 7] {
            while rest.is_multiple_of(prime) {
                factors.push(prime);
                rest /= prime;
            }
        }
        assert_eq!(rest, 1, "FFT length {n} has a prime factor above 7");
        let twiddle = (0..n)
            .map(|k| {
                let angle = -2.0 * PI * k as f64 / n as f64;
                (angle.cos(), angle.sin())
            })
            .collect();
        Fft { n, factors, twiddle }
    }

    fn transform(&self, input: &[(f64, f64)], output: &mut [(f64, f64)]) {
        self.recurse(input, 1, self.n, output, &self.factors);
    }

    /// `n` points of `input` taken every `stride`, transformed into `output[..n]`.
    fn recurse(&self, input: &[(f64, f64)], stride: usize, n: usize, output: &mut [(f64, f64)], factors: &[usize]) {
        if n == 1 {
            output[0] = input[0];
            return;
        }
        let p = factors[0];
        let m = n / p;
        for r in 0..p {
            self.recurse(&input[r * stride..], stride * p, m, &mut output[r * m..(r + 1) * m], &factors[1..]);
        }
        // Combine the p transforms of length m: X[k + q*m] = sum_r W_n^(r*(k + q*m)) * Y_r[k].
        let step = self.n / n;
        let mut column = vec![(0.0f64, 0.0f64); p];
        for k in 0..m {
            for (r, slot) in column.iter_mut().enumerate() {
                *slot = output[r * m + k];
            }
            for q in 0..p {
                let (mut re, mut im) = (0.0f64, 0.0f64);
                for (r, value) in column.iter().enumerate() {
                    let (wr, wi) = self.twiddle[(r * (k + q * m) * step) % self.n];
                    re += value.0 * wr - value.1 * wi;
                    im += value.0 * wi + value.1 * wr;
                }
                output[k + q * m] = (re, im);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fft_matches_the_defining_sum() {
        let fft = Fft::new(N_FFT);
        let input: Vec<(f64, f64)> = (0..N_FFT).map(|i| (((i * 37 % 101) as f64 - 50.0) / 50.0, 0.0)).collect();
        let mut fast = vec![(0.0, 0.0); N_FFT];
        fft.transform(&input, &mut fast);
        for k in [0usize, 1, 7, 100, 199, 200, 201, 399] {
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for (n, (x, _)) in input.iter().enumerate() {
                let angle = -2.0 * PI * ((k * n) % N_FFT) as f64 / N_FFT as f64;
                re += x * angle.cos();
                im += x * angle.sin();
            }
            assert!((re - fast[k].0).abs() < 1e-9 && (im - fast[k].1).abs() < 1e-9, "bin {k}");
        }
    }

    #[test]
    fn reflection_mirrors_without_repeating_the_edge() {
        assert_eq!(vec![3.0, 2.0, 1.0, 2.0, 3.0, 4.0, 3.0, 2.0], reflect_pad(&[1.0, 2.0, 3.0, 4.0], 2));
        // A pad longer than the signal keeps bouncing back and forth, as numpy's does.
        assert_eq!(vec![2.0, 3.0, 2.0, 1.0, 2.0, 3.0, 2.0, 1.0, 2.0], reflect_pad(&[1.0, 2.0, 3.0], 3));
    }
}
