use clap::ValueEnum;
use image::RgbImage;
use serde_json::{json, Value};
use std::{fmt, time::Instant};

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum Backend {
    Auto,
    Openvr,
    Hwnd,
}
#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum Eye {
    Left,
    Right,
}
impl Eye {
    pub fn name(self) -> &'static str {
        if self == Self::Left {
            "left"
        } else {
            "right"
        }
    }
}
pub struct Frame {
    pub rgb: RgbImage,
    pub captured_monotonic: Instant,
    pub metadata: Value,
}
#[derive(Debug)]
pub struct CaptureUnavailable(pub String);
impl fmt::Display for CaptureUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}
impl std::error::Error for CaptureUnavailable {}
impl From<String> for CaptureUnavailable {
    fn from(message: String) -> Self {
        Self(message)
    }
}
pub trait Source {
    fn capture(&mut self) -> Result<Frame, CaptureUnavailable>;
    fn close(&mut self) -> Result<(), String> {
        Ok(())
    }
}

/// A fresh (PID, process-start, frame-index) identity is mandatory before any
/// mirror read. After-read verification permits frame progress within the same
/// process, but rejects a scene/focus/owner switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VrIdentity {
    pub pid: u32,
    pub started: u64,
    pub frame: u32,
    pub focus: u32,
}
pub fn validate_vr_before(
    before: VrIdentity,
    last: Option<VrIdentity>,
    is_vrchat: bool,
) -> Result<(), CaptureUnavailable> {
    if !is_vrchat || before.pid == 0 || before.pid != before.focus {
        return Err(CaptureUnavailable(
            "VRChat is not the active SteamVR scene".into(),
        ));
    }
    if last.is_some_and(|last| {
        last.pid == before.pid && last.started == before.started && last.frame == before.frame
    }) {
        return Err(CaptureUnavailable(
            "SteamVR has not produced a new frame".into(),
        ));
    }
    Ok(())
}
pub fn validate_vr_after(
    before: VrIdentity,
    after: VrIdentity,
    is_vrchat: bool,
) -> Result<(), CaptureUnavailable> {
    if !is_vrchat
        || before.pid != after.pid
        || before.started != after.started
        || after.pid != after.focus
    {
        return Err(CaptureUnavailable(
            "SteamVR scene changed during capture".into(),
        ));
    }
    Ok(())
}

pub struct CaptureSource {
    backend: Backend,
    eye: Eye,
    owner: std::thread::ThreadId,
    #[cfg(all(windows, target_arch = "x86_64"))]
    mirror: Option<vrct_core::openvr::capture::MirrorCapture>,
    #[cfg(all(windows, target_arch = "x86_64"))]
    scene: Option<vrct_core::openvr::capture::SceneConnection>,
    last: Option<VrIdentity>,
    // Once VRChat is known to be a VR scene, acquisition failure cannot silently
    // fall back to the desktop mirror until that specific process exits.
    vr_scene: Option<(u32, u64)>,
}
impl CaptureSource {
    pub fn new(backend: Backend, eye: Eye) -> Self {
        Self {
            backend,
            eye,
            owner: std::thread::current().id(),
            last: None,
            vr_scene: None,
            #[cfg(all(windows, target_arch = "x86_64"))]
            mirror: None,
            #[cfg(all(windows, target_arch = "x86_64"))]
            scene: None,
        }
    }
    fn check_owner(&self) -> Result<(), CaptureUnavailable> {
        if self.owner != std::thread::current().id() {
            return Err(CaptureUnavailable(
                "Capture must run on its creating thread".into(),
            ));
        }
        Ok(())
    }
    #[cfg(all(windows, target_arch = "x86_64"))]
    fn vr_identity(&self) -> Result<VrIdentity, CaptureUnavailable> {
        use vrct_core::ocr::hwnd::process_started;
        let state = self
            .scene
            .as_ref()
            .ok_or_else(|| {
                CaptureUnavailable("SteamVR is unavailable; waiting to reconnect".into())
            })?
            .scene_state()?;
        let started = process_started(state.renderer_pid)
            .ok_or_else(|| CaptureUnavailable("SteamVR renderer process is unavailable".into()))?;
        Ok(VrIdentity {
            pid: state.renderer_pid,
            focus: state.focus_pid,
            frame: state.frame_index,
            started,
        })
    }
    #[cfg(all(windows, target_arch = "x86_64"))]
    fn capture_vr(&mut self) -> Result<Frame, CaptureUnavailable> {
        use vrct_core::ocr::hwnd::{process_name, vrchat_windows};
        let before = self.vr_identity()?;
        validate_vr_before(
            before,
            self.last,
            process_name(before.pid).as_deref() == Some("vrchat.exe"),
        )?;
        let captured_monotonic = Instant::now();
        let utc = chrono::Utc::now().to_rfc3339();
        let rgb = self.mirror.as_mut().unwrap().read_mirror()?;
        let after = self.vr_identity()?;
        validate_vr_after(
            before,
            after,
            process_name(after.pid).as_deref() == Some("vrchat.exe"),
        )?;
        if vrct_core::ocr::hwnd::blank(&rgb, 3.0, 20.0) {
            return Err(CaptureUnavailable(
                "SteamVR mirror returned an empty image".into(),
            ));
        }
        self.last = Some(before);
        Ok(Frame {
            rgb,
            captured_monotonic,
            metadata: json!({"captured_at":utc,"backend":"openvr_d3d11","eye":self.eye.name(),"renderer_pid":before.pid,"renderer_started":before.started,"compositor_frame_before":before.frame,"compositor_frame_after":after.frame,"windows":vrchat_windows()}),
        })
    }
    #[cfg(windows)]
    fn capture_hwnd(&mut self) -> Result<Frame, CaptureUnavailable> {
        use vrct_core::ocr::hwnd::{capture_verified, vrchat_windows};
        let windows: Vec<_> = vrchat_windows()
            .into_iter()
            .filter(|window| window.visible)
            .collect();
        if windows.len() != 1 {
            return Err(CaptureUnavailable(
                "Expected one VRChat window; launch VRChat or select --backend openvr".into(),
            ));
        }
        let window = &windows[0];
        if window.minimized {
            return Err(CaptureUnavailable(
                "Desktop VRChat window is minimized".into(),
            ));
        }
        let captured_monotonic = Instant::now();
        let utc = chrono::Utc::now().to_rfc3339();
        let rgb = capture_verified(window)?;
        Ok(Frame {
            rgb,
            captured_monotonic,
            metadata: json!({"captured_at":utc,"backend":"hwnd","eye":null,"renderer_pid":window.pid,"renderer_started":window.process_started,"windows":windows}),
        })
    }
}
impl Source for CaptureSource {
    fn capture(&mut self) -> Result<Frame, CaptureUnavailable> {
        self.check_owner()?;
        #[cfg(windows)]
        {
            #[cfg(target_arch = "x86_64")]
            if self.backend != Backend::Hwnd {
                use vrct_core::ocr::hwnd::{process_name, process_started};
                if self.vr_scene.is_some_and(|(pid, started)| {
                    process_started(pid) != Some(started)
                        || process_name(pid).as_deref() != Some("vrchat.exe")
                }) {
                    self.vr_scene = None;
                    self.last = None;
                    self.mirror = None;
                    self.scene = None;
                }
                if self.scene.is_none()
                    && (self.backend == Backend::Openvr || process_running("vrcompositor.exe"))
                {
                    self.scene = Some(vrct_core::openvr::capture::SceneConnection::new()?);
                }
                if let Some(scene) = &self.scene {
                    let (_, focus) = scene.scene_processes();
                    if process_name(focus).as_deref() == Some("vrchat.exe") {
                        let started = process_started(focus).ok_or_else(|| {
                            CaptureUnavailable("VRChat focus identity is unavailable".into())
                        })?;
                        self.vr_scene = Some((focus, started));
                    }
                }
                if self.backend == Backend::Openvr || self.vr_scene.is_some() {
                    if self.scene.is_none() {
                        return Err(CaptureUnavailable(
                            "SteamVR is unavailable; waiting to reconnect".into(),
                        ));
                    }
                    if self.mirror.is_none() {
                        match vrct_core::openvr::capture::MirrorCapture::new_eye(
                            self.eye == Eye::Right,
                        ) {
                            Ok(mirror) => self.mirror = Some(mirror),
                            Err(error) => {
                                self.scene = None;
                                return Err(CaptureUnavailable(error));
                            }
                        }
                    }
                    let result = self.capture_vr();
                    if result.is_err() {
                        self.mirror = None;
                        self.scene = None;
                    }
                    return result;
                }
            }
            self.capture_hwnd()
        }
        #[cfg(not(windows))]
        Err(CaptureUnavailable(
            "Native capture requires Windows x64".into(),
        ))
    }
    fn close(&mut self) -> Result<(), String> {
        self.check_owner().map_err(|e| e.to_string())?;
        #[cfg(all(windows, target_arch = "x86_64"))]
        {
            self.mirror = None;
            self.scene = None;
        }
        self.last = None;
        Ok(())
    }
}
#[cfg(windows)]
fn process_running(name: &str) -> bool {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
        System::Diagnostics::ToolHelp::*,
    };
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut process: PROCESSENTRY32W = std::mem::zeroed();
        process.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = Process32FirstW(snapshot, &mut process);
        let mut found = false;
        while ok != 0 {
            let count = process
                .szExeFile
                .iter()
                .position(|c| *c == 0)
                .unwrap_or(process.szExeFile.len());
            if String::from_utf16_lossy(&process.szExeFile[..count]).eq_ignore_ascii_case(name) {
                found = true;
                break;
            }
            ok = Process32NextW(snapshot, &mut process);
        }
        CloseHandle(snapshot);
        found
    }
}
