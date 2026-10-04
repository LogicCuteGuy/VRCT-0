//! OpenVR D3D11 mirror capture (left eye by default). Tables are pinned to Valve
//! SDK v2.15.6 (System_026, Compositor_029). Resources stay on the creating
//! thread, the mirror is acquired once, and the shared lease outlives it.
use super::native::{self, Lease};
use image::RgbImage;
use serde::Serialize;
use std::{ffi::c_void, ptr::null_mut};
use windows_sys::Win32::{Foundation::CloseHandle, System::Threading::*};

#[repr(C)]
#[derive(Clone, Copy, Default, Serialize)]
pub struct TextureDesc {
    #[serde(rename = "Width")]
    width: u32,
    #[serde(rename = "Height")]
    height: u32,
    #[serde(rename = "MipLevels")]
    mips: u32,
    #[serde(rename = "ArraySize")]
    array: u32,
    #[serde(rename = "Format")]
    format: u32,
    #[serde(rename = "SampleCount")]
    samples: u32,
    #[serde(rename = "SampleQuality")]
    quality: u32,
    #[serde(rename = "Usage")]
    usage: u32,
    #[serde(rename = "BindFlags")]
    bind: u32,
    #[serde(rename = "CPUAccessFlags")]
    cpu: u32,
    #[serde(rename = "MiscFlags")]
    misc: u32,
}
#[repr(C)]
#[derive(Default)]
struct TrackedPose {
    matrix: [f32; 12],
    velocity: [f32; 3],
    angular_velocity: [f32; 3],
    tracking: i32,
    valid: bool,
    connected: bool,
}
/// Exact SDK v2.15.6 layout, including the caller-initialized size.
#[repr(C)]
#[derive(Default)]
struct FrameTiming {
    counters: [u32; 6],
    system_time: f64,
    times: [f32; 16],
    pose: TrackedPose,
    vsync_ready: u32,
    vsync_first: u32,
    transfer_latency: f32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct SceneState {
    pub renderer_pid: u32,
    pub focus_pid: u32,
    pub frame_index: u32,
}
/// Query scene ownership before acquiring a GPU mirror. The lease keeps the
/// compositor table valid, including while a mirror is temporarily unavailable.
pub struct SceneConnection {
    _lease: Lease,
    compositor: *const [usize; 53],
}
impl SceneConnection {
    pub fn new() -> Result<Self, String> {
        let lease = native::acquire()?;
        let compositor = unsafe {
            lease
                .connection()
                .interface::<[usize; 53]>(c"FnTable:IVRCompositor_029")?
        };
        Ok(Self {
            _lease: lease,
            compositor,
        })
    }
    pub fn scene_processes(&self) -> (u32, u32) {
        unsafe { scene_processes(self.compositor) }
    }
    pub fn scene_state(&self) -> Result<SceneState, String> {
        unsafe { scene_state(self.compositor) }
    }
}
unsafe fn scene_processes(compositor: *const [usize; 53]) -> (u32, u32) {
    unsafe {
        let focus: unsafe extern "system" fn() -> u32 = std::mem::transmute((*compositor)[24]);
        let renderer: unsafe extern "system" fn() -> u32 = std::mem::transmute((*compositor)[25]);
        (renderer(), focus())
    }
}
unsafe fn scene_state(compositor: *const [usize; 53]) -> Result<SceneState, String> {
    unsafe {
        let (renderer_pid, focus_pid) = scene_processes(compositor);
        let get_timing: unsafe extern "system" fn(*mut FrameTiming, u32) -> bool =
            std::mem::transmute((*compositor)[10]);
        let mut timing = FrameTiming::default();
        timing.counters[0] = std::mem::size_of::<FrameTiming>() as u32;
        if !get_timing(&mut timing, 0) {
            return Err("SteamVR frame timing is unavailable".into());
        }
        Ok(SceneState {
            renderer_pid,
            focus_pid,
            frame_index: timing.counters[1],
        })
    }
}
#[derive(Serialize)]
pub struct MirrorDiagnostics {
    pub adapter_index: i32,
    pub view_format: u32,
    pub texture: TextureDesc,
    pub row_pitch: u32,
    pub hmd: String,
    pub recommended_size: [u32; 2],
}
#[repr(C)]
#[derive(Default)]
struct Mapped {
    data: *mut c_void,
    row_pitch: u32,
    depth_pitch: u32,
}
#[repr(C)]
struct Guid {
    a: u32,
    b: u16,
    c: u16,
    d: [u8; 8],
}
#[link(name = "dxgi")]
extern "system" {
    fn CreateDXGIFactory(iid: *const Guid, out: *mut *mut c_void) -> i32;
}
#[link(name = "d3d11")]
extern "system" {
    fn D3D11CreateDevice(
        adapter: *mut c_void,
        driver: u32,
        software: *mut c_void,
        flags: u32,
        levels: *const u32,
        count: u32,
        sdk: u32,
        device: *mut *mut c_void,
        level: *mut u32,
        context: *mut *mut c_void,
    ) -> i32;
}

unsafe fn slot(object: *mut c_void, index: usize) -> usize {
    unsafe { *(*(object as *const *const usize)).add(index) }
}
unsafe fn release(object: *mut c_void) {
    if !object.is_null() {
        unsafe {
            let call: unsafe extern "system" fn(*mut c_void) -> u32 =
                std::mem::transmute(slot(object, 2));
            call(object);
        }
    }
}
fn hr(code: i32, operation: &str) -> Result<(), String> {
    if code < 0 {
        Err(format!("{operation}: HRESULT {:08x}", code as u32))
    } else {
        Ok(())
    }
}

/// !Send raw COM pointers enforce the capture-thread ownership contract.
pub struct MirrorCapture {
    _lease: Lease,
    compositor: *const [usize; 53],
    device: *mut c_void,
    context: *mut c_void,
    srv: *mut c_void,
    texture: *mut c_void,
    staging: *mut c_void,
    desc: TextureDesc,
    format: u32,
    system: *const [usize; 51],
    adapter_index: i32,
    row_pitch: u32,
}
impl MirrorCapture {
    pub fn new() -> Result<Self, String> {
        Self::new_eye(false)
    }
    pub fn new_eye(right: bool) -> Result<Self, String> {
        let lease = native::acquire()?;
        let compositor = unsafe {
            lease
                .connection()
                .interface::<[usize; 53]>(c"FnTable:IVRCompositor_029")?
        };
        let system = unsafe {
            lease
                .connection()
                .interface::<[usize; 51]>(c"FnTable:IVRSystem_026")?
        };
        let mut capture = Self {
            _lease: lease,
            compositor,
            device: null_mut(),
            context: null_mut(),
            srv: null_mut(),
            texture: null_mut(),
            staging: null_mut(),
            desc: TextureDesc::default(),
            format: 0,
            system,
            adapter_index: -1,
            row_pitch: 0,
        };
        unsafe {
            let get_adapter: unsafe extern "system" fn(*mut i32) =
                std::mem::transmute((*system)[8]);
            let mut adapter_index = -1;
            get_adapter(&mut adapter_index);
            capture.adapter_index = adapter_index;
            if adapter_index < 0 {
                return Err("SteamVR has no DXGI adapter".into());
            }
            let mut factory = null_mut();
            let mut adapter = null_mut();
            let guid = Guid {
                a: 0x7b7166ec,
                b: 0x21c7,
                c: 0x44ae,
                d: [0xb2, 0x1a, 0xc9, 0xae, 0x32, 0x1a, 0xe3, 0x69],
            };
            hr(CreateDXGIFactory(&guid, &mut factory), "CreateDXGIFactory")?;
            let initialize = (|| -> Result<(), String> {
                let enumerate: unsafe extern "system" fn(
                    *mut c_void,
                    u32,
                    *mut *mut c_void,
                ) -> i32 = std::mem::transmute(slot(factory, 7));
                hr(
                    enumerate(factory, adapter_index as u32, &mut adapter),
                    "EnumAdapters",
                )?;
                let mut level = 0;
                hr(
                    D3D11CreateDevice(
                        adapter,
                        0,
                        null_mut(),
                        0,
                        std::ptr::null(),
                        0,
                        7,
                        &mut capture.device,
                        &mut level,
                        &mut capture.context,
                    ),
                    "D3D11CreateDevice",
                )?;
                let mirror: unsafe extern "system" fn(i32, *mut c_void, *mut *mut c_void) -> i32 =
                    std::mem::transmute((*compositor)[35]);
                let code = mirror(i32::from(right), capture.device, &mut capture.srv);
                if code != 0 || capture.srv.is_null() {
                    return Err(format!("GetMirrorTextureD3D11: OpenVR {code}"));
                }
                let resource: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) =
                    std::mem::transmute(slot(capture.srv, 7));
                resource(capture.srv, &mut capture.texture);
                if capture.texture.is_null() {
                    return Err("OpenVR mirror has no texture".into());
                }
                let descriptor: unsafe extern "system" fn(*mut c_void, *mut TextureDesc) =
                    std::mem::transmute(slot(capture.texture, 10));
                descriptor(capture.texture, &mut capture.desc);
                let view_descriptor: unsafe extern "system" fn(*mut c_void, *mut u32) =
                    std::mem::transmute(slot(capture.srv, 8));
                let mut view = [0u32; 16];
                view_descriptor(capture.srv, view.as_mut_ptr());
                capture.format = view[0];
                if ![27, 28, 29, 87, 90, 91].contains(&capture.desc.format)
                    || ![28, 29, 87, 91].contains(&capture.format)
                    || capture.desc.samples != 1
                    || capture.desc.array != 1
                    || capture.desc.mips != 1
                    || capture.desc.width == 0
                    || capture.desc.height == 0
                    || capture.desc.width as u64 * capture.desc.height as u64 > 64 * 1024 * 1024
                {
                    return Err("Unsupported OpenVR mirror texture format or shape".into());
                }
                let mut staging = capture.desc;
                staging.format = capture.format;
                staging.usage = 3;
                staging.bind = 0;
                staging.cpu = 0x20000;
                staging.misc = 0;
                let create: unsafe extern "system" fn(
                    *mut c_void,
                    *const TextureDesc,
                    *const c_void,
                    *mut *mut c_void,
                ) -> i32 = std::mem::transmute(slot(capture.device, 5));
                hr(
                    create(
                        capture.device,
                        &staging,
                        std::ptr::null(),
                        &mut capture.staging,
                    ),
                    "CreateTexture2D",
                )?;
                Ok(())
            })();
            release(adapter);
            release(factory);
            initialize?;
        }
        Ok(capture)
    }
    pub fn capture(&mut self) -> Result<Option<RgbImage>, String> {
        let (pid, focus) = self.scene_processes();
        if pid == 0 || pid != focus || !vrchat_process(pid) {
            return Ok(None);
        }
        let image = self.read_mirror()?;
        if self.scene_processes() != (pid, focus) || !vrchat_process(pid) {
            return Ok(None);
        }
        if image
            .as_raw()
            .windows(2)
            .all(|pixels| pixels[0] == pixels[1])
        {
            return Ok(None);
        }
        Ok(Some(image))
    }
    pub fn scene_processes(&self) -> (u32, u32) {
        unsafe { scene_processes(self.compositor) }
    }
    pub fn scene_state(&self) -> Result<SceneState, String> {
        unsafe { scene_state(self.compositor) }
    }
    pub fn diagnostics(&self) -> MirrorDiagnostics {
        let mut size = [0u32; 2];
        let mut name = [0u8; 1024];
        unsafe {
            let recommended: unsafe extern "system" fn(*mut u32, *mut u32) =
                std::mem::transmute((*self.system)[0]);
            recommended(size.as_mut_ptr(), size.as_mut_ptr().add(1));
            let property: unsafe extern "system" fn(u32, i32, *mut u8, u32, *mut i32) -> u32 =
                std::mem::transmute((*self.system)[28]);
            let mut error = 0;
            property(0, 1001, name.as_mut_ptr(), name.len() as u32, &mut error);
        }
        MirrorDiagnostics {
            adapter_index: self.adapter_index,
            view_format: self.format,
            texture: self.desc,
            row_pitch: self.row_pitch,
            hmd: String::from_utf8_lossy(
                &name[..name
                    .iter()
                    .position(|byte| *byte == 0)
                    .unwrap_or(name.len())],
            )
            .into(),
            recommended_size: size,
        }
    }
    /// Unfiltered mirror read for diagnostic probes. Dataset sources must check
    /// scene identity and timing around this call; application OCR uses capture.
    pub fn read_mirror(&mut self) -> Result<RgbImage, String> {
        unsafe {
            let copy: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) =
                std::mem::transmute(slot(self.context, 47));
            copy(self.context, self.staging, self.texture);
            let map: unsafe extern "system" fn(
                *mut c_void,
                *mut c_void,
                u32,
                u32,
                u32,
                *mut Mapped,
            ) -> i32 = std::mem::transmute(slot(self.context, 14));
            let mut mapped = Mapped::default();
            hr(
                map(self.context, self.staging, 0, 1, 0, &mut mapped),
                "Map mirror texture",
            )?;
            self.row_pitch = mapped.row_pitch;
            let result = decode_rows(
                mapped.data,
                mapped.row_pitch,
                self.desc.width,
                self.desc.height,
                [87, 91].contains(&self.format),
            );
            let unmap: unsafe extern "system" fn(*mut c_void, *mut c_void, u32) =
                std::mem::transmute(slot(self.context, 15));
            unmap(self.context, self.staging, 0);
            result
        }
    }
}
impl Drop for MirrorCapture {
    fn drop(&mut self) {
        unsafe {
            release(self.staging);
            release(self.texture);
            if !self.srv.is_null() {
                let release_mirror: unsafe extern "system" fn(*mut c_void) =
                    std::mem::transmute((*self.compositor)[36]);
                release_mirror(self.srv);
            }
            release(self.context);
            release(self.device);
        }
    }
}
unsafe fn decode_rows(
    data: *mut c_void,
    pitch: u32,
    width: u32,
    height: u32,
    bgra: bool,
) -> Result<RgbImage, String> {
    if data.is_null() || pitch < width.checked_mul(4).ok_or("Mirror row too large")? {
        return Err("Invalid mapped mirror rows".into());
    }
    let length = (pitch as usize)
        .checked_mul(height as usize)
        .ok_or("Mirror mapping too large")?;
    if length > 512 * 1024 * 1024 {
        return Err("Mirror mapping exceeds limit".into());
    }
    let bytes = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), length) };
    let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
    for row in bytes.chunks_exact(pitch as usize) {
        for pixel in row[..width as usize * 4].chunks_exact(4) {
            if bgra {
                rgb.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
            } else {
                rgb.extend_from_slice(&pixel[..3]);
            }
        }
    }
    RgbImage::from_raw(width, height, rgb).ok_or("Mirror image shape is invalid".into())
}
fn vrchat_process(pid: u32) -> bool {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut path = vec![0u16; 32768];
        let mut size = path.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, path.as_mut_ptr(), &mut size);
        CloseHandle(handle);
        ok != 0
            && String::from_utf16_lossy(&path[..size as usize])
                .rsplit(['\\', '/'])
                .next()
                .is_some_and(|name| name.eq_ignore_ascii_case("VRChat.exe"))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scene_ownership_is_available_without_gpu_or_frame_timing() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SIZE: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "system" fn renderer() -> u32 {
            10
        }
        unsafe extern "system" fn focus() -> u32 {
            12
        }
        unsafe extern "system" fn unavailable(timing: *mut FrameTiming, _: u32) -> bool {
            SIZE.store(unsafe { (*timing).counters[0] } as usize, Ordering::Release);
            false
        }
        let mut table = [0usize; 53];
        table[24] = focus as *const () as usize;
        table[25] = renderer as *const () as usize;
        table[10] = unavailable as *const () as usize;
        assert_eq!(unsafe { scene_processes(&table) }, (10, 12));
        assert!(unsafe { scene_state(&table) }.is_err());
        assert_eq!(
            SIZE.load(Ordering::Acquire),
            std::mem::size_of::<FrameTiming>()
        );
    }
    #[test]
    fn sdk_2156_frame_timing_layout_has_initialized_size_and_correct_offsets() {
        assert_eq!(std::mem::size_of::<TrackedPose>(), 80);
        assert_eq!(std::mem::size_of::<FrameTiming>(), 192);
        assert_eq!(std::mem::offset_of!(FrameTiming, system_time), 24);
        assert_eq!(std::mem::offset_of!(FrameTiming, pose), 96);
        assert_eq!(std::mem::offset_of!(FrameTiming, vsync_ready), 176);
    }
    #[test]
    fn pitched_bgra_mirror_preserves_top_row_and_discards_padding() {
        let mut rows = [
            1u8, 2, 3, 255, 4, 5, 6, 255, 99, 99, 99, 99, 7, 8, 9, 255, 10, 11, 12, 255, 99, 99,
            99, 99,
        ];
        let frame = unsafe { decode_rows(rows.as_mut_ptr().cast(), 12, 2, 2, true) }.unwrap();
        assert_eq!(frame.as_raw(), &[3, 2, 1, 6, 5, 4, 9, 8, 7, 12, 11, 10]);
        assert!(unsafe { decode_rows(rows.as_mut_ptr().cast(), 4, 2, 2, true) }.is_err());
        assert!(unsafe { decode_rows(null_mut(), 12, 2, 2, false) }.is_err());
    }
}
