pub mod collector;
pub mod console;
pub mod player;
pub mod probe;
pub mod source;

use std::path::PathBuf;
pub fn base_directory() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|p| p.to_owned()))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
}
pub fn run_id() -> Result<String, String> {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).map_err(|e| e.to_string())?;
    Ok(format!(
        "{}_{}",
        chrono::Utc::now().format("%Y%m%dT%H%M%S_%6fZ"),
        hex::encode(bytes)
    ))
}
/// A deterministic local PRNG: shuffled runs with the same seed repeat across
/// builds; shuffle does not depend on a library's changing algorithm.
pub struct Random(u64);
pub fn parse_seed(value: &str) -> Result<String, String> {
    let value = value.trim();
    let digits = value.strip_prefix(['+', '-']).unwrap_or(value);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("Seed must be a signed decimal integer".into());
    }
    let digits = digits.trim_start_matches('0');
    Ok(if digits.is_empty() {
        "0".into()
    } else if value.starts_with('-') {
        format!("-{digits}")
    } else {
        digits.into()
    })
}
impl Random {
    pub fn new(seed: Option<&str>) -> Result<Self, String> {
        let mut bytes = [0u8; 8];
        if let Some(seed) = seed {
            use sha2::{Digest, Sha256};
            let seed = parse_seed(seed)?;
            if let Ok(integer) = seed.parse::<i64>() {
                bytes = (integer as u64).to_le_bytes();
            } else {
                // Decimal normalization preserves arbitrary-size integer seeds
                // without requiring Python or an unbounded integer dependency.
                bytes.copy_from_slice(&Sha256::digest(seed.as_bytes())[..8]);
            }
        } else {
            getrandom::fill(&mut bytes).map_err(|e| e.to_string())?;
        }
        Ok(Self(u64::from_le_bytes(bytes)))
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
        value ^ (value >> 31)
    }
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / ((1u64 << 53) as f64)
    }
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for index in (1..items.len()).rev() {
            // Rejection avoids modulo bias while keeping seed determinism.
            let size = index as u64 + 1;
            let limit = u64::MAX - u64::MAX % size;
            let mut value = self.next_u64();
            while value >= limit {
                value = self.next_u64();
            }
            items.swap(index, (value % size) as usize);
        }
    }
}
