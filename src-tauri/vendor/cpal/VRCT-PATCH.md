# CPAL 0.17.1 WASAPI lifetime patch

Source: crates.io `cpal` 0.17.1, Apache-2.0 (see LICENSE).

The process-wide WASAPI enumerator was created in the first caller's COM STA.
After that thread exited, subsequent callers could dereference its dead interface
and crash with STATUS_ACCESS_VIOLATION. See
https://github.com/RustAudio/cpal/issues/1302.

This patch uses a thread-local enumerator and initializes COM before its TLS slot,
so the enumerator drops before COM is uninitialized. It removes the enumerator's
unsafe Send/Sync implementations. The existing device and stream APIs are retained.

Regression: `wasapi_enumeration_survives_sequential_thread_exit` in vrct-core.
Remove the path override when an upstream release fixes this lifetime issue.
