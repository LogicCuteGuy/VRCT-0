//! What the cloud engines have in common: they are asynchronous, the transcriber's thread is not.

use std::future::Future;

use tokio::runtime::Handle;

use super::phrases::{Recognition, RecognizeError, Recognizer, Request};

/// An engine that answers over the network.
pub trait CloudRecognizer: Send + Sync {
    fn recognize(&self, request: &Request<'_>) -> impl Future<Output = Result<Recognition, RecognizeError>> + Send;
}

/// A cloud engine as a [`Recognizer`]: each request blocks the calling thread until the answer
/// comes. Call it from a plain thread (the transcriber's), never from one of the runtime's own.
pub struct Blocking<P> {
    provider: P,
    runtime: Handle,
}

impl<P: CloudRecognizer> Blocking<P> {
    pub fn new(provider: P, runtime: Handle) -> Self {
        Blocking { provider, runtime }
    }
}

impl<P: CloudRecognizer> Recognizer for Blocking<P> {
    fn recognize(&mut self, request: &Request<'_>) -> Result<Recognition, RecognizeError> {
        self.runtime.block_on(self.provider.recognize(request))
    }
}

/// The `TRANSCRIPTION_API_*` codes the Python provider classes attached to a failure.
pub mod code {
    pub const AUTH_FAILED: &str = "TRANSCRIPTION_API_AUTH_FAILED";
    pub const RATE_LIMITED: &str = "TRANSCRIPTION_API_RATE_LIMITED";
    pub const TIMEOUT: &str = "TRANSCRIPTION_API_TIMEOUT";
    pub const SERVER_ERROR: &str = "TRANSCRIPTION_API_SERVER_ERROR";
}

pub(super) fn api_error(code: &str) -> RecognizeError {
    RecognizeError::Api { code: code.to_string() }
}

pub(super) fn other(kind: &str) -> RecognizeError {
    RecognizeError::Other { kind: kind.to_string() }
}
