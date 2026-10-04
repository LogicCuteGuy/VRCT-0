//! WASAPI endpoint enumeration and registered ASIO drivers on Windows.

use cpal::traits::{DeviceTrait, HostTrait};

use super::devices::{loopback_name, Device, DeviceList, WASAPI_HOST, ASIO_HOST};

/// Registered ASIO drivers, without loading or disturbing an active driver.
pub fn asio_driver_names() -> Vec<String> {
    use windows_sys::Win32::System::Registry::*;
    let key_name: Vec<u16> = "SOFTWARE\\ASIO\0".encode_utf16().collect();
    let mut key = std::ptr::null_mut();
    if unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, key_name.as_ptr(), 0, KEY_READ, &mut key) } != 0 {
        return Vec::new();
    }
    let mut names = Vec::new();
    for index in 0..1024 {
        let mut name = [0u16; 256];
        let mut length = name.len() as u32;
        let result = unsafe { RegEnumKeyExW(key, index, name.as_mut_ptr(), &mut length, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) };
        if result == 259 { break; }
        if result == 0 { names.push(String::from_utf16_lossy(&name[..length as usize])); }
    }
    unsafe { RegCloseKey(key); }
    names.sort();
    names
}

pub fn host_names() -> Vec<String> {
    let mut names = vec![WASAPI_HOST.to_owned()];
    if !asio_driver_names().is_empty() { names.push(ASIO_HOST.to_owned()); }
    names
}

pub fn list_devices_for_host(host: &str) -> Result<DeviceList, String> {
    match host {
        WASAPI_HOST | "" => list_devices(),
        ASIO_HOST => {
            // Actual stream format is read when the selected driver is opened.
            // Listing registered drivers must not load other drivers while recording.
            let devices: Vec<Device> = asio_driver_names().into_iter().map(|name| Device {
                name, channels: 0, default_sample_rate: 0,
            }).collect();
            Ok(DeviceList { mics: devices.clone(), speakers: devices, default_mic: None, default_speaker: None })
        }
        "NoHost" => Ok(DeviceList::default()),
        _ => Err(format!("unsupported audio host {host:?}")),
    }
}

/// Share a single CPAL driver instance between mic and speaker capture. ASIO
/// allows one driver globally; another driver can be selected after capture stops.
type AsioDeviceCache = std::sync::Mutex<Option<(String, std::sync::Arc<cpal::Device>)>>;
static ASIO_DEVICE: std::sync::OnceLock<AsioDeviceCache> = std::sync::OnceLock::new();

pub(super) struct AudioApartment(bool);
impl AudioApartment {
    pub(super) fn initialize(host: &str) -> Result<Self, String> {
        if host != ASIO_HOST { return Ok(Self(false)); }
        use windows_sys::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
        let result = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
        if result < 0 { return Err(format!("ASIO COM initialization failed: 0x{:08x}", result as u32)); }
        Ok(Self(true))
    }
}
impl Drop for AudioApartment {
    fn drop(&mut self) {
        if self.0 { unsafe { windows_sys::Win32::System::Com::CoUninitialize(); } }
    }
}

pub(super) fn release_asio_device() {
    if let Some(cache) = ASIO_DEVICE.get() {
        if let Ok(mut cached) = cache.lock() {
            if cached.as_ref().is_some_and(|(_, device)| std::sync::Arc::strong_count(device) == 1) {
                *cached = None;
            }
        }
    }
}

pub(super) fn asio_device(name: &str) -> Result<std::sync::Arc<cpal::Device>, String> {
    use std::sync::{Arc, Mutex, OnceLock};
    let mut cached = ASIO_DEVICE.get_or_init(|| Mutex::new(None)).lock().map_err(|_| "ASIO driver lock poisoned")?;
    if let Some((current, device)) = cached.as_ref() {
        if current == name { return Ok(device.clone()); }
        if Arc::strong_count(device) > 1 {
            return Err(format!("ASIO driver {current:?} is in use; stop both captures before choosing {name:?}"));
        }
    }
    *cached = None;
    static ASIO: OnceLock<asio_sys::Asio> = OnceLock::new();
    let driver = ASIO.get_or_init(asio_sys::Asio::new).load_driver(name)
        .map_err(|e| format!("ASIO driver {name:?} could not be opened: {e}"))?;
    type AsioDevice = <cpal::platform::AsioHost as HostTrait>::Device;
    let device: cpal::Device = AsioDevice {
        driver: Arc::new(driver),
        asio_streams: Arc::new(Mutex::new(asio_sys::AsioStreams { input: None, output: None })),
        current_callback_flag: Arc::new(std::sync::atomic::AtomicU32::new(u32::MAX)),
    }.into();
    let device = Arc::new(device);
    *cached = Some((name.to_owned(), device.clone()));
    Ok(device)
}

/// Keep the selected driver alive while its native, possibly modal panel is open.
pub fn open_asio_control_panel(name: &str) -> Result<(), String> {
    static PANEL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _panel = PANEL.try_lock().map_err(|_| "An ASIO control panel is already open")?;
    let _apartment = AudioApartment::initialize(ASIO_HOST)?;
    let device = asio_device(name)?;
    // ASIOControlPanel operates on the process's currently loaded driver. The
    // device lease prevents replacement or unloading until this call returns.
    unsafe extern "C" { fn vrct_asio_control_panel() -> i32; }
    let result = unsafe { vrct_asio_control_panel() };
    drop(device);
    release_asio_device();
    if result == -1000 && name.starts_with("VB-Matrix ") {
        return open_vb_matrix();
    }
    if result != 0 { return Err(format!("ASIO control panel failed for {name:?}: {result}")); }
    Ok(())
}

fn open_vb_matrix() -> Result<(), String> {
    use windows_sys::Win32::{Foundation::{BOOL, HWND, LPARAM}, UI::WindowsAndMessaging::*};
    unsafe extern "system" fn find(hwnd: HWND, parameter: LPARAM) -> BOOL {
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid); }
        if crate::ocr::hwnd::process_name(pid).is_some_and(|name|
            matches!(name.as_str(), "vbaudiomatrixcoconut_x64.exe" | "vbaudiomatrixcoconut.exe" | "vbaudiomatrix_x64.exe" | "vbaudiomatrix.exe")) {
            let mut title = [0u16; 256];
            let length = unsafe { GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32) }.max(0) as usize;
            if String::from_utf16_lossy(&title[..length]).starts_with("VB-Audio Matrix") {
                unsafe { *(parameter as *mut HWND) = hwnd; }
                return 0;
            }
        }
        1
    }
    let mut window: HWND = std::ptr::null_mut();
    unsafe { EnumWindows(Some(find), &mut window as *mut HWND as LPARAM); }
    if !window.is_null() {
        unsafe { ShowWindow(window, SW_RESTORE); SetForegroundWindow(window); }
        return Ok(());
    }
    let folder = std::path::PathBuf::from(std::env::var_os("ProgramFiles(x86)").ok_or("Windows Program Files directory is unavailable")?)
        .join("VB/VBAudioMatrix");
    for name in ["VBAudioMatrixCoconut_x64.exe", "VBAudioMatrix_x64.exe", "VBAudioMatrixCoconut.exe", "VBAudioMatrix.exe"] {
        let path = folder.join(name);
        if path.is_file() {
            std::process::Command::new(path).spawn().map_err(|e| format!("Could not open VB-Matrix: {e}"))?;
            return Ok(());
        }
    }
    Err("This driver has no ASIO panel and the VB-Matrix application was not found".into())
}

/// What is plugged in and enabled right now. A device that cannot be read is skipped,
/// the way PortAudio's list leaves out what it cannot open.
pub fn list_devices() -> Result<DeviceList, String> {
    let host = cpal::default_host();

    let mics = host.input_devices().map_err(|e| format!("cannot list microphones: {e}"))?;
    let mics: Vec<Device> =
        mics.filter_map(|device| describe(&device, |d| d.default_input_config(), |name| name)).collect();
    let speakers = host.output_devices().map_err(|e| format!("cannot list playback devices: {e}"))?;
    let mut speakers: Vec<Device> =
        speakers.filter_map(|device| describe(&device, |d| d.default_output_config(), |name| loopback_name(&name))).collect();
    // Speaker is the receiving STT role, not a playback-only device category.
    // Virtual mixer outputs and recording inputs are valid sources too.
    speakers.extend(mics.iter().cloned());

    Ok(DeviceList {
        mics,
        default_mic: host.default_input_device().and_then(|device| name_of(&device)),
        speakers,
        default_speaker: host.default_output_device().and_then(|device| name_of(&device)).map(|name| loopback_name(&name)),
    })
}

/// The name Windows shows and PortAudio reports, e.g. `Microphone (UGREEN Camera)`. cpal's own
/// `name()` is only the part before the parentheses; the whole is its first extended line.
pub(super) fn name_of<D: DeviceTrait>(device: &D) -> Option<String> {
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

#[cfg(test)]
mod asio_tests {
    use super::*;

    #[test]
    #[ignore = "Opens the selected driver's native control panel; close the panel if it is modal"]
    fn installed_asio_control_panel_opens() {
        let name = std::env::var("VRCT_ASIO_PROBE_DRIVER").expect("set VRCT_ASIO_PROBE_DRIVER");
        open_asio_control_panel(&name).unwrap();
    }

    #[test]
    fn asio_enumeration_reports_registered_names_for_both_capture_roles() {
        let expected = asio_driver_names();
        let list = list_devices_for_host(ASIO_HOST).unwrap();
        assert_eq!(list.mics.iter().map(|d| d.name.clone()).collect::<Vec<_>>(), expected);
        assert_eq!(list.speakers, list.mics);
        assert!(list.speakers.iter().all(|d| !d.name.ends_with(" [Loopback]")));
        assert!(list_devices_for_host("unknown audio host").is_err());
    }

    #[test]
    #[ignore = "Opens the driver named by VRCT_ASIO_PROBE_DRIVER for format inspection; does not capture audio"]
    fn installed_asio_driver_reports_input_format_and_shares_one_instance() {
        let name = std::env::var("VRCT_ASIO_PROBE_DRIVER").expect("set VRCT_ASIO_PROBE_DRIVER");
        let _apartment = AudioApartment::initialize(ASIO_HOST).unwrap();
        let first = asio_device(&name).unwrap();
        let second = asio_device(&name).unwrap();
        assert!(std::sync::Arc::ptr_eq(&first, &second));
        let format = first.default_input_config().unwrap();
        assert!(format.channels() > 0 && format.sample_rate() > 0);
        assert!(asio_device("missing driver").err().unwrap().contains("is in use"));
        eprintln!("{name}: {} input channels, {} Hz, {:?}", format.channels(), format.sample_rate(), format.sample_format());
        drop(first);
        drop(second);
        release_asio_device();
    }

    #[test]
    #[ignore = "Briefly captures ASIO buffers and counts bytes only; requires VRCT_ASIO_PROBE_DRIVER"]
    fn asio_mic_and_speaker_streams_share_driver_and_stop_independently() {
        use super::super::capture::{Capture, Source};
        use std::sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}};
        use std::time::{Duration, Instant};
        let name = std::env::var("VRCT_ASIO_PROBE_DRIVER").expect("set VRCT_ASIO_PROBE_DRIVER");
        let mic_bytes = Arc::new(AtomicUsize::new(0));
        let speaker_bytes = Arc::new(AtomicUsize::new(0));
        let failures = Arc::new(Mutex::new(Vec::new()));
        let start = |source, count: Arc<AtomicUsize>| {
            let errors = failures.clone();
            Capture::start_on_host(source, ASIO_HOST, &name,
                move |pcm| { count.fetch_add(pcm.len(), Ordering::Relaxed); },
                move |error| { errors.lock().unwrap().push(error); }).unwrap()
        };
        let mut mic = start(Source::Microphone, mic_bytes.clone());
        let mut speaker = start(Source::Speaker, speaker_bytes.clone());
        let deadline = Instant::now() + Duration::from_secs(3);
        while mic_bytes.load(Ordering::Relaxed) == 0 || speaker_bytes.load(Ordering::Relaxed) == 0 {
            assert!(Instant::now() < deadline, "both ASIO streams must deliver buffers");
            std::thread::sleep(Duration::from_millis(20));
        }
        mic.stop();
        let before = speaker_bytes.load(Ordering::Relaxed);
        let deadline = Instant::now() + Duration::from_secs(3);
        while speaker_bytes.load(Ordering::Relaxed) == before {
            assert!(Instant::now() < deadline, "stopping mic must not stop speaker");
            std::thread::sleep(Duration::from_millis(20));
        }
        speaker.stop();
        assert!(ASIO_DEVICE.get().unwrap().lock().unwrap().is_none(), "last stopped stream must release the driver");
        let failures = failures.lock().unwrap().clone();
        assert!(failures.is_empty(), "{failures:?}");
        eprintln!("ASIO mic/speaker received buffers; speaker continued after mic stopped; all streams closed");
    }
}
