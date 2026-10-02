//! The app's settings, owned here instead of by the Python sidecar's `Config`.
//!
//! `schema` lists every setting and how a new value is checked, `validators` holds the rules
//! that look at the value's structure, `defaults` the values before config.json is read and
//! `store` the `Settings` object that loads, changes and writes them. `tests/settings.rs`
//! checks all of it against what the real Python `Config` does.

pub mod defaults;
pub mod env;
pub mod pyvalue;
pub mod schema;
pub mod store;
pub mod validators;

pub use env::{Devices, Env, Paths};
pub use schema::{Rejected, PROPS};
pub use store::{channel_for_version, config_text, SetError, Settings, DEBOUNCE};
