use super::{render::Frame, Driver, DriverFactory, Settings, Size};
pub struct Factory;
impl DriverFactory for Factory {
    fn connect(&self) -> Result<Box<dyn Driver>, String> {
        #[cfg(all(windows, target_arch = "x86_64"))]
        {
            Ok(Box::new(windows::Native::connect()?))
        }
        #[cfg(not(all(windows, target_arch = "x86_64")))]
        {
            Err("Native OpenVR overlays require Windows x64".into())
        }
    }
}
#[cfg(all(windows, target_arch = "x86_64"))]
mod windows {
    use super::*;
    use crate::openvr::native::{acquire, Lease};
    use std::ffi::{c_char, c_void, CString};
    // Exact table slots from Valve SDK2.15.6, revision092406... openvr_capi.h.
    // C exports use extern C; callbacks in the returned tables use stdcall.
    #[repr(C)]
    struct OverlayTable {
        _find: usize,
        create: Option<unsafe extern "system" fn(*const c_char, *const c_char, *mut u64) -> u32>,
        _subview: usize,
        destroy: Option<unsafe extern "system" fn(u64) -> u32>,
        _skip0: [usize; 10],
        color: Option<unsafe extern "system" fn(u64, f32, f32, f32) -> u32>,
        _get_color: usize,
        alpha: Option<unsafe extern "system" fn(u64, f32) -> u32>,
        _skip1: [usize; 5],
        width: Option<unsafe extern "system" fn(u64, f32) -> u32>,
        _skip2: [usize; 12],
        transform: Option<unsafe extern "system" fn(u64, u32, *const [[f32; 4]; 3]) -> u32>,
        _skip3: [usize; 7],
        show: Option<unsafe extern "system" fn(u64) -> u32>,
        _skip4: [usize; 18],
        raw: Option<unsafe extern "system" fn(u64, *const c_void, u32, u32, u32) -> u32>,
    }
    #[repr(C)]
    struct SystemTable {
        _skip0: [usize; 18],
        role: Option<unsafe extern "system" fn(u32) -> u32>,
        _skip1: [usize; 2],
        connected: Option<unsafe extern "system" fn(u32) -> bool>,
        _skip2: [usize; 8],
        poll: Option<unsafe extern "system" fn(*mut Event, u32) -> bool>,
    }
    #[repr(C)]
    struct Event {
        kind: u32,
        index: u32,
        age: f32,
        _align: u32,
        data: [u64; 6],
    }
    pub struct Native {
        lease: Lease,
        handles: [u64; 2],
    }
    fn check(error: u32, action: &str) -> Result<(), String> {
        if error == 0 {
            Ok(())
        } else {
            Err(format!("OpenVR overlay {action} failed ({error})"))
        }
    }
    impl Native {
        pub fn connect() -> Result<Self, String> {
            let lease = acquire()?;
            let mut result = Self {
                lease,
                handles: [0; 2],
            };
            let table = result.overlay()?;
            let create = table
                .create
                .ok_or("OpenVR CreateOverlay function missing")?;
            let show = table.show.ok_or("OpenVR ShowOverlay function missing")?;
            let color = table
                .color
                .ok_or("OpenVR SetOverlayColor function missing")?;
            // Unique keys allow a blocked old worker to keep its lease safely
            // without interfering with a replacement worker's overlay handles.
            static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            for index in 0..2 {
                let name = CString::new(format!(
                    "VRCT.{}.{}.{}",
                    std::process::id(),
                    generation,
                    index
                ))
                .map_err(|e| e.to_string())?;
                let mut handle = 0;
                check(
                    unsafe { create(name.as_ptr(), name.as_ptr(), &mut handle) },
                    "create",
                )?;
                result.handles[index] = handle;
                check(unsafe { show(handle) }, "show")?;
                check(unsafe { color(handle, 1., 1., 1.) }, "color")?;
            }
            Ok(result)
        }
        fn overlay(&self) -> Result<&OverlayTable, String> {
            // SAFETY: pinned version matches table layout and self holds lease.
            let table = unsafe {
                self.lease
                    .connection()
                    .interface::<OverlayTable>(c"FnTable:IVROverlay_028")?
            };
            Ok(unsafe { &*table })
        }
        fn system(&self) -> Result<&SystemTable, String> {
            let table = unsafe {
                self.lease
                    .connection()
                    .interface::<SystemTable>(c"FnTable:IVRSystem_026")?
            };
            Ok(unsafe { &*table })
        }
    }
    impl Driver for Native {
        fn image(&mut self, size: Size, frame: &Frame) -> Result<(), String> {
            if frame.width == 0
                || frame.height == 0
                || frame.pixels.len() != frame.width as usize * frame.height as usize * 4
            {
                return Err("Invalid overlay RGBA frame".into());
            }
            let raw = self
                .overlay()?
                .raw
                .ok_or("OpenVR SetOverlayRaw function missing")?;
            // OpenVR copies the buffer synchronously; Frame lives through call.
            check(
                unsafe {
                    raw(
                        self.handles[size.index()],
                        frame.pixels.as_ptr().cast(),
                        frame.width,
                        frame.height,
                        4,
                    )
                },
                "image",
            )
        }
        fn settings(&mut self, size: Size, settings: &Settings, alpha: f32) -> Result<(), String> {
            let overlay = self.overlay()?;
            let system = self.system()?;
            let handle = self.handles[size.index()];
            let set_alpha = overlay
                .alpha
                .ok_or("OpenVR SetOverlayAlpha function missing")?;
            let width = overlay
                .width
                .ok_or("OpenVR SetOverlayWidthInMeters function missing")?;
            check(unsafe { set_alpha(handle, alpha) }, "alpha")?;
            check(unsafe { width(handle, settings.width) }, "width")?;
            let tracker = match settings.tracker.as_str() {
                "LeftHand" => unsafe { system.role.ok_or("OpenVR controller role missing")?(1) },
                "RightHand" => unsafe { system.role.ok_or("OpenVR controller role missing")?(2) },
                _ => 0,
            };
            if tracker < 64
                && unsafe {
                    system
                        .connected
                        .ok_or("OpenVR connected function missing")?(tracker)
                }
            {
                let matrix = super::super::transform(settings);
                check(
                    unsafe {
                        overlay
                            .transform
                            .ok_or("OpenVR transform function missing")?(
                            handle, tracker, &matrix
                        )
                    },
                    "transform",
                )?;
            }
            Ok(())
        }
        fn active(&mut self) -> Result<bool, String> {
            let poll = self
                .system()?
                .poll
                .ok_or("OpenVR PollNextEvent function missing")?;
            let mut event = Event {
                kind: 0,
                index: 0,
                age: 0.,
                _align: 0,
                data: [0; 6],
            };
            // Bound draining to avoid starving shutdown on a flooded event queue.
            for _ in 0..256 {
                if !unsafe { poll(&mut event, std::mem::size_of::<Event>() as u32) } {
                    break;
                }
                if event.kind == 700 {
                    return Ok(false);
                }
            }
            Ok(true)
        }
    }
    impl Drop for Native {
        fn drop(&mut self) {
            if let Ok(table) = self.overlay() {
                if let Some(destroy) = table.destroy {
                    for &handle in &self.handles {
                        if handle != 0 {
                            unsafe {
                                destroy(handle);
                            }
                        }
                    }
                }
            }
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn valve_sdk_function_slots_match_pinned_header() {
            assert_eq!(std::mem::offset_of!(OverlayTable, create), 8);
            assert_eq!(std::mem::offset_of!(OverlayTable, color), 14 * 8);
            assert_eq!(std::mem::offset_of!(OverlayTable, alpha), 16 * 8);
            assert_eq!(std::mem::offset_of!(OverlayTable, width), 22 * 8);
            assert_eq!(std::mem::offset_of!(OverlayTable, transform), 35 * 8);
            assert_eq!(std::mem::offset_of!(OverlayTable, show), 43 * 8);
            assert_eq!(std::mem::offset_of!(OverlayTable, raw), 62 * 8);
            assert_eq!(std::mem::offset_of!(SystemTable, role), 18 * 8);
            assert_eq!(std::mem::offset_of!(SystemTable, connected), 21 * 8);
            assert_eq!(std::mem::offset_of!(SystemTable, poll), 30 * 8);
            assert_eq!(std::mem::size_of::<Event>(), 64);
        }
    }
}
