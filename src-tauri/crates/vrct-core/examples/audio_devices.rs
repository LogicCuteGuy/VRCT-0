//! Prints the microphones and loopback speakers Rust sees through WASAPI, as JSON.
//! Compare with `DeviceManager` in the Python backend:  cargo run -p vrct-core --example audio_devices

#[cfg(windows)]
fn main() {
    let list = vrct_core::audio::wasapi::list_devices().unwrap_or_else(|e| panic!("{e}"));
    let devices = |devices: &[vrct_core::audio::devices::Device]| {
        devices
            .iter()
            .map(|d| serde_json::json!({"name": d.name, "ch": d.channels, "rate": d.default_sample_rate}))
            .collect::<Vec<_>>()
    };
    let out = serde_json::json!({
        "mics": devices(&list.mics),
        "default_mic": list.default_mic,
        "speakers": devices(&list.speakers),
        "default_speaker": list.default_speaker,
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}

#[cfg(not(windows))]
fn main() {
    eprintln!("WASAPI capture is Windows only");
}
