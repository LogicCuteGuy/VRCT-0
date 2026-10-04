//! The audio devices as the rest of the app sees them, independent of the OS API.
//!
//! The Python backend lists devices through PortAudio, which exposes several
//! host APIs (MME, DirectSound, WASAPI) and takes a speaker's audio from a
//! WASAPI "loopback" twin of each playback device, named `<playback name>
//! [Loopback]`. WASAPI names retain that convention. ASIO uses registered
//! driver names and input capture for both roles; host-scoped enumeration lives
//! in `wasapi`. What changes for a user
//! is a saved selection made under another host: `DeviceList::resolve_mic`
//! maps it onto the WASAPI device (MME cuts names to 31 characters, so a saved
//! name can be a prefix of the real one).

/// The host name PortAudio gave WASAPI.
pub const WASAPI_HOST: &str = "Windows WASAPI";
pub const ASIO_HOST: &str = "ASIO";
/// Placeholders the UI is given when nothing is available.
pub const NO_HOST: &str = "NoHost";
pub const NO_DEVICE: &str = "NoDevice";
pub const LOOPBACK_SUFFIX: &str = " [Loopback]";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub name: String,
    pub channels: u16,
    pub default_sample_rate: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceList {
    pub mics: Vec<Device>,
    pub default_mic: Option<String>,
    /// Receiving STT sources: recording inputs and playback loopback sources.
    pub speakers: Vec<Device>,
    pub default_speaker: Option<String>,
}

/// The loopback name for a playback device.
pub fn loopback_name(playback_name: &str) -> String {
    format!("{playback_name}{LOOPBACK_SUFFIX}")
}

impl DeviceList {
    /// What `getMicDevices().keys()` gives: the one host, or the placeholder when there is no microphone.
    pub fn hosts(&self) -> Vec<&'static str> {
        vec![if self.mics.is_empty() { NO_HOST } else { WASAPI_HOST }]
    }

    pub fn mic_names(&self) -> Vec<&str> {
        if self.mics.is_empty() {
            return vec![NO_DEVICE];
        }
        self.mics.iter().map(|device| device.name.as_str()).collect()
    }

    pub fn speaker_names(&self) -> Vec<&str> {
        if self.speakers.is_empty() {
            return vec![NO_DEVICE];
        }
        self.speakers.iter().map(|device| device.name.as_str()).collect()
    }

    /// The microphone a saved selection stands for. The host the selection was made under is
    /// ignored (it can be MME or DirectSound); only the device name counts.
    pub fn resolve_mic(&self, saved_name: &str) -> Option<&Device> {
        resolve(&self.mics, saved_name)
    }

    pub fn resolve_speaker(&self, saved_name: &str) -> Option<&Device> {
        resolve(&self.speakers, saved_name)
    }
}

/// An exact name wins; failing that a saved name that is the start of exactly one device's name
/// (a name MME cut short). Several candidates mean the saved name does not say which, so none.
fn resolve<'a>(devices: &'a [Device], saved_name: &str) -> Option<&'a Device> {
    if saved_name.is_empty() {
        return None;
    }
    if let Some(device) = devices.iter().find(|device| device.name == saved_name) {
        return Some(device);
    }
    let mut candidates = devices.iter().filter(|device| device.name.starts_with(saved_name));
    match (candidates.next(), candidates.next()) {
        (Some(only), None) => Some(only),
        _ => None,
    }
}
