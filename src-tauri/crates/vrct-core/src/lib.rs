//! VRCT backend core.
//!
//! The UI talks to the backend with `{endpoint, data}` requests and receives
//! `{status, endpoint, result}` responses. This crate keeps that contract:
//! every application endpoint is served in-process. The native controller
//! audits the recorded public surface before the application starts.

pub mod audio;
pub mod auth;
pub mod controller;
pub mod errors;
pub mod models;
pub mod native;
pub mod ocr;
pub mod device_monitor;
pub mod telemetry;
pub mod config;
pub mod osc_query;
pub mod openvr;
pub mod overlay;
pub mod pipeline;
pub mod protocol;
pub mod router;
pub mod runtime;
pub mod rpc;
pub mod settings;
pub mod setters;
pub mod sinks;
pub mod transcription;
pub mod translation;
pub mod transliteration;
pub mod updater;
