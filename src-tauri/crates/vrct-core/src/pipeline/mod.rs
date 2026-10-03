//! The message pipeline: what happens to a transcript, a typed message or an OCR line between "it was
//! recognised" and "everyone has been told" (`Controller._processMessage` and friends).
//!
//! * [`process`] is the pipeline and its four entry points; [`spec`] holds what differs per direction.
//! * [`host`] is everything the pipeline calls out to (translator, outputs, UI).
//! * [`keywords`] is the word filter, [`format`] the chatbox text, [`history`] the translation context.

pub mod errors;
pub mod format;
pub mod history;
pub mod host;
pub mod keywords;
pub mod process;
pub mod spec;

pub use host::{Host, LargeLog, SmallLog, TranslateError, Translated};
pub use process::{Pipeline, PipelineError};
