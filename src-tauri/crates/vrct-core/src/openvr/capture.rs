//! OpenVR D3D11 left-eye mirror capture. Function tables are pinned to Valve
//! SDK v2.15.6 (System_026, Compositor_029). Resources stay on the creating
//! thread, the mirror is acquired once, and the shared lease outlives it.
use super::native::{self, Lease};
use image::RgbImage;
use std::{ffi::c_void, ptr::null_mut};
use windows_sys::Win32::{Foundation::CloseHandle, System::Threading::*};

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct TextureDesc {
    width: u32,
    height: u32,
    mips: u32,
    array: u32,
    format: u32,
    samples: u32,
    quality: u32,
    usage: u32,
    bind: u32,
    cpu: u32,
    misc: u32,
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
}
impl MirrorCapture {
    pub fn new() -> Result<Self, String> {
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
        };
        unsafe {
            let get_adapter: unsafe extern "system" fn(*mut i32) =
                std::mem::transmute((*system)[8]);
            let mut adapter_index = -1;
            get_adapter(&mut adapter_index);
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
                let code = mirror(0, capture.device, &mut capture.srv);
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
        unsafe {
            let focus: unsafe extern "system" fn() -> u32 =
                std::mem::transmute((*self.compositor)[24]);
            let renderer: unsafe extern "system" fn() -> u32 =
                std::mem::transmute((*self.compositor)[25]);
            let pid = renderer();
            if pid == 0 || pid != focus() || !vrchat_process(pid) {
                return Ok(None);
            }
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
            let image = result?;
            if image
                .as_raw()
                .windows(2)
                .all(|pixels| pixels[0] == pixels[1])
            {
                return Ok(None);
            }
            Ok(Some(image))
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
