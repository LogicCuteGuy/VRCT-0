use crate::{invalid, Result};
use rand::{RngCore, SeedableRng};
use rand_chacha::ChaCha20Rng;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
    process::Command,
};
pub const SAMPLE_RATE: u32 = 16_000;
pub const NOISE_ALGORITHM: &str = "ChaCha20-BoxMuller-v1";
#[derive(Debug)]
pub struct Pcm {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
}
impl Pcm {
    pub fn mono(&self) -> Vec<f32> {
        self.samples
            .chunks_exact(self.channels as usize)
            .map(|frame| frame.iter().sum::<f32>() / self.channels as f32)
            .collect()
    }
    pub fn standard(&self) -> bool {
        self.sample_rate == SAMPLE_RATE && self.channels == 1
    }
}
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    Ok(hex::encode(digest.finalize()))
}
pub fn deterministic_seed(parts: &[&str]) -> u64 {
    let digest = Sha256::digest(parts.join("\x1f").as_bytes());
    u64::from_le_bytes(digest[..8].try_into().unwrap())
}
pub fn read_pcm16_wav(path: &Path) -> Result<Pcm> {
    let data = std::fs::read(path)?;
    if data.len() < 12 || &data[..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return Err(invalid("expected RIFF PCM16 WAV"));
    }
    let declared = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
    let limit = declared
        .checked_add(8)
        .filter(|n| *n <= data.len())
        .ok_or_else(|| invalid("truncated WAV container"))?;
    let mut offset = 12;
    let mut format = None;
    let mut samples = None;
    while offset + 8 <= limit {
        let name = &data[offset..offset + 4];
        let size = u32::from_le_bytes(data[offset + 4..offset + 8].try_into().unwrap()) as usize;
        offset += 8;
        let end = offset
            .checked_add(size)
            .filter(|n| *n <= limit)
            .ok_or_else(|| invalid("truncated WAV chunk"))?;
        if name == b"fmt " {
            if size < 16 {
                return Err(invalid("truncated WAV format"));
            }
            let tag = u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap());
            let channels = u16::from_le_bytes(data[offset + 2..offset + 4].try_into().unwrap());
            let rate = u32::from_le_bytes(data[offset + 4..offset + 8].try_into().unwrap());
            let align = u16::from_le_bytes(data[offset + 12..offset + 14].try_into().unwrap());
            let bits = u16::from_le_bytes(data[offset + 14..offset + 16].try_into().unwrap());
            let extensible_pcm = tag == 0xfffe
                && size >= 40
                && data[offset + 24..offset + 40]
                    == [1, 0, 0, 0, 0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113];
            if (tag != 1 && !extensible_pcm)
                || bits != 16
                || channels == 0
                || rate == 0
                || align as u32 != channels as u32 * 2
            {
                return Err(invalid("expected signed16 PCM WAV format"));
            }
            format = Some((channels, rate));
        } else if name == b"data" {
            samples = Some(&data[offset..end]);
        }
        offset = end
            .checked_add(size % 2)
            .ok_or_else(|| invalid("WAV offset overflow"))?;
    }
    let (channels, rate) = format.ok_or_else(|| invalid("WAV has no format"))?;
    let bytes = samples.ok_or_else(|| invalid("WAV has no audio data"))?;
    if bytes.len() % (channels as usize * 2) != 0 {
        return Err(invalid("truncated PCM frame"));
    }
    Ok(Pcm {
        sample_rate: rate,
        channels,
        samples: bytes
            .chunks_exact(2)
            .map(|p| i16::from_le_bytes([p[0], p[1]]) as f32 / 32768.)
            .collect(),
    })
}
pub fn write_pcm16_wav(path: &Path, samples: &[f32], rate: u32) -> Result<()> {
    if rate == 0 || samples.iter().any(|x| !x.is_finite()) {
        return Err(invalid("invalid PCM sample/rate"));
    }
    let size = u32::try_from(
        samples
            .len()
            .checked_mul(2)
            .ok_or_else(|| invalid("PCM too large"))?,
    )
    .map_err(|_| invalid("WAV exceeds4GiB"))?;
    let riff = size
        .checked_add(36)
        .ok_or_else(|| invalid("WAV exceeds4GiB"))?;
    let byte_rate = rate
        .checked_mul(2)
        .ok_or_else(|| invalid("invalid sample rate"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut output = File::create(path)?;
    output.write_all(b"RIFF")?;
    output.write_all(&riff.to_le_bytes())?;
    output.write_all(b"WAVEfmt ")?;
    output.write_all(&16u32.to_le_bytes())?;
    output.write_all(&1u16.to_le_bytes())?;
    output.write_all(&1u16.to_le_bytes())?;
    output.write_all(&rate.to_le_bytes())?;
    output.write_all(&byte_rate.to_le_bytes())?;
    output.write_all(&2u16.to_le_bytes())?;
    output.write_all(&16u16.to_le_bytes())?;
    output.write_all(b"data")?;
    output.write_all(&size.to_le_bytes())?;
    for &sample in samples {
        output.write_all(
            &((sample * 32767.).round_ties_even().clamp(-32768., 32767.) as i16).to_le_bytes(),
        )?;
    }
    Ok(())
}
fn rms(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        0.
    } else {
        (samples.iter().map(|x| (x * x) as f64).sum::<f64>() / samples.len() as f64).sqrt()
    }
}
pub fn normalize_light(samples: &[f32]) -> Vec<f32> {
    let mut out = samples.to_vec();
    let power = rms(&out);
    if power > 0. {
        let gain = (10f64.powf(-24. / 20.) / power).min(10f64.powf(6. / 20.)) as f32;
        for x in &mut out {
            *x *= gain;
        }
    }
    let peak = out.iter().map(|x| x.abs()).fold(0f32, f32::max);
    let limit = 10f32.powf(-1. / 20.);
    if peak > limit {
        for x in &mut out {
            *x *= limit / peak;
        }
    }
    for x in &mut out {
        *x = x.clamp(-1., 1.);
    }
    out
}
pub fn convert_to_standard_wav(
    source: &Path,
    destination: &Path,
    ffmpeg: &str,
) -> Result<Vec<f32>> {
    if !source.is_file() {
        return Err(invalid(format!(
            "audio source does not exist: {}",
            source.display()
        )));
    }
    if destination.exists() && same_file::is_same_file(source, destination)? {
        return Err(invalid("audio destination would overwrite source"));
    }
    let decoded = read_pcm16_wav(source).ok().filter(Pcm::standard);
    let samples = if let Some(decoded) = decoded {
        decoded.samples
    } else {
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let output = Command::new(ffmpeg)
            .args(["-v", "error", "-y", "-i"])
            .arg(source)
            .args(["-ac", "1", "-ar", "16000", "-sample_fmt", "s16"])
            .arg(destination)
            .output()
            .map_err(|e| {
                invalid(format!(
                    "ffmpeg required for conversion (use --ffmpeg): {e}"
                ))
            })?;
        if !output.status.success() {
            let _ = std::fs::remove_file(destination);
            return Err(invalid(format!(
                "ffmpeg failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let decoded = read_pcm16_wav(destination)?;
        if !decoded.standard() {
            return Err(invalid("ffmpeg output is not16k mono PCM16"));
        }
        decoded.samples
    };
    let samples = normalize_light(&samples);
    write_pcm16_wav(destination, &samples, SAMPLE_RATE)?;
    Ok(samples)
}
pub fn mix_at_snr(clean: &[f32], noise: &[f32], snr: f64) -> Result<Vec<f32>> {
    if !snr.is_finite() || clean.iter().chain(noise).any(|x| !x.is_finite()) {
        return Err(invalid("noise mixing requires finite samples/SNR"));
    }
    if clean.is_empty() || noise.is_empty() {
        return Ok(clean.to_vec());
    }
    if clean.len() != noise.len() {
        return Err(invalid("noise length must equal clean length"));
    }
    let (signal, power) = (rms(clean), rms(noise));
    if signal == 0. || power == 0. {
        return Ok(clean.to_vec());
    }
    let gain = (signal / 10f64.powf(snr / 20.) / power) as f32;
    let mut out: Vec<f32> = clean.iter().zip(noise).map(|(a, b)| a + b * gain).collect();
    let peak = out.iter().map(|x| x.abs()).fold(0f32, f32::max);
    if peak > 0.98 {
        for x in &mut out {
            *x *= 0.98 / peak;
        }
    }
    Ok(out.into_iter().map(|x| x.clamp(-1., 1.)).collect())
}
pub fn make_noise_variant(
    clean: &[f32],
    kind: &str,
    snr: f64,
    seed: u64,
    environment: Option<&[f32]>,
) -> Result<Vec<f32>> {
    let mut key = [0u8; 32];
    key[..8].copy_from_slice(&seed.to_le_bytes());
    let mut rng = ChaCha20Rng::from_seed(key);
    let noise = match kind {
        "white" => {
            let mut out = Vec::with_capacity(clean.len());
            while out.len() < clean.len() {
                let u = ((rng.next_u64() >> 11) as f64 + 0.5) / ((1u64 << 53) as f64);
                let v = (rng.next_u64() >> 11) as f64 / ((1u64 << 53) as f64);
                let magnitude = (-2. * u.ln()).sqrt();
                let angle = std::f64::consts::TAU * v;
                out.push((magnitude * angle.cos()) as f32);
                if out.len() < clean.len() {
                    out.push((magnitude * angle.sin()) as f32);
                }
            }
            out
        }
        "environment" => {
            let input = environment
                .filter(|e| !e.is_empty())
                .ok_or_else(|| invalid("environment noise is empty or missing"))?;
            // Rejection sampling keeps the chosen cyclic window uniform.
            let bound = input.len() as u64;
            let threshold = bound.wrapping_neg() % bound;
            let start = loop {
                let v = rng.next_u64();
                if v >= threshold {
                    break (v % bound) as usize;
                }
            };
            let mut out: Vec<f32> = (0..clean.len())
                .map(|i| input[(i + start) % input.len()])
                .collect();
            if !out.is_empty() {
                let mean = (out.iter().map(|x| *x as f64).sum::<f64>() / out.len() as f64) as f32;
                for x in &mut out {
                    *x -= mean;
                }
            }
            out
        }
        _ => return Err(invalid(format!("unsupported noise kind: {kind}"))),
    };
    mix_at_snr(clean, &noise, snr)
}
