//! Offline dataset preparation and evaluation through the real VRCT pipeline.
pub mod audio;
pub mod dataset;
pub mod evaluate;
pub mod metrics;
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub(crate) fn invalid(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message.into()).into()
}
