//! Device enumeration through WASAPI (cpal).

use cpal::traits::{DeviceTrait, HostTrait};

use super::devices::{loopback_name, Device, DeviceList};

/// What is plugged in and enabled right now. A device that cannot be read is skipped,
/// the way PortAudio's list leaves out what it cannot open.
pub fn list_devices() -> Result<DeviceList, String> {
    let host = cpal::default_host();

    let mics = host.input_devices().map_err(|e| format!("cannot list microphones: {e}"))?;
    let mics: Vec<Device> =
        mics.filter_map(|device| describe(&device, |d| d.default_input_config(), |name| name)).collect();
    let speakers = host.output_devices().map_err(|e| format!("cannot list playback devices: {e}"))?;
    let speakers: Vec<Device> =
        speakers.filter_map(|device| describe(&device, |d| d.default_output_config(), |name| loopback_name(&name))).collect();

    Ok(DeviceList {
        mics,
        default_mic: host.default_input_device().and_then(|device| name_of(&device)),
        speakers,
        default_speaker: host.default_output_device().and_then(|device| name_of(&device)).map(|name| loopback_name(&name)),
    })
}

/// The name Windows shows and PortAudio reports, e.g. `Microphone (UGREEN Camera)`. cpal's own
/// `name()` is only the part before the parentheses; the whole is its first extended line.
fn name_of<D: DeviceTrait>(device: &D) -> Option<String> {
    let description = device.description().ok()?;
    Some(description.extended().first().cloned().unwrap_or_else(|| description.name().to_string()))
}

fn describe<D: DeviceTrait, E>(
    device: &D,
    config: impl Fn(&D) -> Result<cpal::SupportedStreamConfig, E>,
    rename: impl Fn(String) -> String,
) -> Option<Device> {
    let name = name_of(device)?;
    let config = config(device).ok()?;
    Some(Device { name: rename(name), channels: config.channels(), default_sample_rate: config.sample_rate() })
}
