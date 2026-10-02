//! VRCT backend core.
//!
//! The UI talks to the backend with `{endpoint, data}` requests and receives
//! `{status, endpoint, result}` responses. This crate keeps that contract:
//! endpoints implemented in Rust are served in-process, everything else is
//! forwarded to the legacy Python sidecar until it is removed.

pub mod audio;
pub mod config;
pub mod protocol;
pub mod router;
pub mod rpc;
pub mod settings;
pub mod setters;
pub mod sinks;
pub mod transcription;
pub mod translation;
pub mod updater;
