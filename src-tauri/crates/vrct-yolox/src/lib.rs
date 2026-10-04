//! Native detector tooling. Network topology follows Megvii YOLOX (Apache-2.0).
pub mod ema;
pub mod evaluation;
pub mod graph;
pub mod network;
pub mod quantization;
pub mod training;
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
