//! Native, resumable annotation jobs and dataset preparation. No subprocesses.
pub mod dataset;
mod filesystem;
pub mod gemini;
pub mod job;
mod random;

pub type Result<T> = std::result::Result<T, String>;
