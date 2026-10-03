//! Read the clipboard target through Valve's OpenVR C API. No OpenVR calls are
//! made until a native clipboard job arrives. Background initialization neither
//! launches SteamVR. Clipboard and native overlay share the process-wide lease.
use super::WindowTarget;

pub(super) struct OpenVr;

#[cfg(test)]
trait VrApi {
    fn initialize(&self) -> Result<(), String>;
    fn applications(&self) -> Result<Vec<(String, String)>, String>;
    fn shutdown(&self);
}

#[cfg(test)]
struct Session<'a, A: VrApi>(&'a A);
#[cfg(test)]
impl<A: VrApi> Drop for Session<'_, A> {
    fn drop(&mut self) {
        self.0.shutdown();
    }
}

#[cfg(test)]
fn resolve(api: &impl VrApi) -> Result<Option<String>, String> {
    api.initialize()?;
    let _session = Session(api);
    // Preserve Python's selection: the first steam.app key, after reading all
    // properties. This is not a check for the currently running scene process.
    Ok(api
        .applications()?
        .into_iter()
        .find(|(key, _)| key.starts_with("steam.app"))
        .map(|(_, name)| name))
}

impl WindowTarget for OpenVr {
    fn window_name(&self) -> Result<Option<String>, String> {
        #[cfg(all(windows, target_arch = "x86_64"))]
        {
            native::window_name()
        }
        #[cfg(not(all(windows, target_arch = "x86_64")))]
        {
            Ok(None)
        }
    }
}

#[cfg(all(windows, target_arch = "x86_64"))]
mod native {
    use crate::openvr::native::acquire;
    #[cfg(test)]
    use crate::openvr::native::library_path;
    use std::ffi::{c_char, CStr};

    // ABI pinned to Valve SDK 2.15.6, commit 0924064316de3effbcd1acf1e309182a2deb1c05,
    // headers/openvr_capi.h. Only the prefix through GetApplicationPropertyString
    // is read. Exports use the C ABI; function-table callbacks use stdcall.
    type Count = unsafe extern "system" fn() -> u32;
    type Key = unsafe extern "system" fn(u32, *mut c_char, u32) -> u32;
    type Name = unsafe extern "system" fn(*mut c_char, u32, *mut c_char, u32, *mut u32) -> u32;

    #[repr(C)]
    struct Applications {
        _manifest: [usize; 3],
        count: Option<Count>,
        key: Option<Key>,
        _launch: [usize; 9],
        name: Option<Name>,
    }

    fn read_applications(table: &Applications) -> Result<Vec<(String, String)>, String> {
        let count = table
            .count
            .ok_or("OpenVR application count function missing")?;
        let key = table.key.ok_or("OpenVR application key function missing")?;
        let name = table
            .name
            .ok_or("OpenVR application name function missing")?;
        let count = unsafe { count() };
        if count > 8192 {
            return Err("OpenVR application count exceeds limit".into());
        }
        let mut applications = Vec::with_capacity(count as usize);
        for index in 0..count {
            // k_unMaxApplicationKeyLength = 128, k_unMaxPropertyStringSize = 32768.
            let mut key_buffer = [0 as c_char; 128];
            let error = unsafe { key(index, key_buffer.as_mut_ptr(), key_buffer.len() as u32) };
            if error != 0 {
                return Err(format!("OpenVR application key failed ({error})"));
            }
            let key_text = text(&key_buffer)?;
            let mut name_buffer = vec![0 as c_char; 32768];
            let mut error = 0;
            // VRApplicationProperty_Name_String = 0.
            let length = unsafe {
                name(
                    key_buffer.as_mut_ptr(),
                    0,
                    name_buffer.as_mut_ptr(),
                    name_buffer.len() as u32,
                    &mut error,
                )
            };
            if error != 0 || length == 0 || length as usize > name_buffer.len() {
                return Err(format!(
                    "OpenVR application name failed ({error}, {length})"
                ));
            }
            let name_text = text(&name_buffer[..length as usize])?;
            applications.push((key_text, name_text));
        }
        Ok(applications)
    }

    fn text(buffer: &[c_char]) -> Result<String, String> {
        let end = buffer
            .iter()
            .position(|byte| *byte == 0)
            .ok_or("OpenVR string is not terminated")?;
        let bytes: Vec<u8> = buffer[..=end].iter().map(|byte| *byte as u8).collect();
        CStr::from_bytes_with_nul(&bytes)
            .map_err(|e| e.to_string())?
            .to_str()
            .map(str::to_string)
            .map_err(|e| e.to_string())
    }

    pub(super) fn window_name() -> Result<Option<String>, String> {
        let lease = acquire()?;
        // SAFETY: the requested SDK version matches Applications, and the lease
        // remains live throughout table reads. Clipboard release cannot shut
        // down OpenVR while an overlay holds another lease.
        let table = unsafe {
            lease
                .connection()
                .interface::<Applications>(c"FnTable:IVRApplications_008")?
        };
        Ok(read_applications(unsafe { &*table })?
            .into_iter()
            .find(|(key, _)| key.starts_with("steam.app"))
            .map(|(_, name)| name))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use libloading::Library;

        #[test]
        fn bundled_sdk_exports_load_without_initializing_steamvr() {
            let library = unsafe { Library::new(library_path().unwrap()) }.unwrap();
            unsafe {
                library
                    .get::<unsafe extern "C" fn(*mut u32, u32) -> usize>(b"VR_InitInternal\0")
                    .unwrap();
                library
                    .get::<unsafe extern "C" fn()>(b"VR_ShutdownInternal\0")
                    .unwrap();
                library.get::<unsafe extern "C" fn(*const c_char, *mut u32) -> *const std::ffi::c_void>(b"VR_GetGenericInterface\0").unwrap();
            } // No initialize, clipboard, or keypress.
            assert_eq!(std::mem::offset_of!(Applications, count), 3 * 8);
            assert_eq!(std::mem::offset_of!(Applications, key), 4 * 8);
            assert_eq!(std::mem::offset_of!(Applications, name), 14 * 8);
        }

        #[test]
        fn native_strings_require_nul_and_valid_utf8() {
            assert_eq!(
                text(&[0xE6u8 as i8, 0x97u8 as i8, 0xA5u8 as i8, 0]).unwrap(),
                "日"
            );
            assert!(text(&[65]).is_err());
            assert!(text(&[-1, 0]).is_err());
            assert_eq!(text(&[0, 65]).unwrap(), "");
        }

        #[test]
        fn function_table_snapshot_validates_buffers_and_native_errors() {
            unsafe extern "system" fn count() -> u32 {
                2
            }
            unsafe extern "system" fn key(index: u32, output: *mut c_char, length: u32) -> u32 {
                assert_eq!(length, 128);
                let text = if index == 0 {
                    c"openvr.overlay"
                } else {
                    c"steam.app.438100"
                };
                std::ptr::copy_nonoverlapping(
                    text.as_ptr(),
                    output,
                    text.to_bytes_with_nul().len(),
                );
                0
            }
            unsafe extern "system" fn name(
                key: *mut c_char,
                property: u32,
                output: *mut c_char,
                length: u32,
                error: *mut u32,
            ) -> u32 {
                assert_eq!(property, 0);
                assert_eq!(length, 32768);
                let text = if CStr::from_ptr(key) == c"steam.app.438100" {
                    c"ゲーム ไทย"
                } else {
                    c"Overlay"
                };
                let bytes = text.to_bytes_with_nul();
                std::ptr::copy_nonoverlapping(text.as_ptr(), output, bytes.len());
                *error = 0;
                bytes.len() as u32
            }
            unsafe extern "system" fn bad_key(_: u32, _: *mut c_char, _: u32) -> u32 {
                200
            }
            unsafe extern "system" fn bad_length(
                _: *mut c_char,
                _: u32,
                _: *mut c_char,
                length: u32,
                _: *mut u32,
            ) -> u32 {
                length + 1
            }
            unsafe extern "system" fn huge_count() -> u32 {
                u32::MAX
            }
            let mut table = Applications {
                _manifest: [0; 3],
                count: Some(count),
                key: Some(key),
                _launch: [0; 9],
                name: Some(name),
            };
            assert_eq!(
                read_applications(&table).unwrap(),
                [
                    ("openvr.overlay".into(), "Overlay".into()),
                    ("steam.app.438100".into(), "ゲーム ไทย".into())
                ]
            );
            table.name = Some(bad_length);
            assert!(read_applications(&table)
                .unwrap_err()
                .contains("name failed"));
            table.key = Some(bad_key);
            assert!(read_applications(&table)
                .unwrap_err()
                .contains("key failed"));
            table.count = Some(huge_count);
            assert!(read_applications(&table)
                .unwrap_err()
                .contains("count exceeds"));
            table.count = None;
            assert!(read_applications(&table)
                .unwrap_err()
                .contains("function missing"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Fake {
        fail_init: bool,
        apps: Result<Vec<(String, String)>, String>,
        calls: RefCell<Vec<&'static str>>,
    }
    impl Fake {
        fn new(apps: &[(&str, &str)]) -> Self {
            Self {
                fail_init: false,
                apps: Ok(apps
                    .iter()
                    .map(|(key, name)| (key.to_string(), name.to_string()))
                    .collect()),
                calls: RefCell::default(),
            }
        }
    }
    impl VrApi for Fake {
        fn initialize(&self) -> Result<(), String> {
            self.calls.borrow_mut().push("init");
            if self.fail_init {
                Err("No SteamVR".into())
            } else {
                Ok(())
            }
        }
        fn applications(&self) -> Result<Vec<(String, String)>, String> {
            self.calls.borrow_mut().push("apps");
            self.apps.clone()
        }
        fn shutdown(&self) {
            self.calls.borrow_mut().push("shutdown");
        }
    }

    #[test]
    fn selection_preserves_the_first_steam_app_and_unicode_name() {
        let api = Fake::new(&[
            ("openvr.overlay", "Other"),
            ("steam.app.1", "ゲーム ไทย"),
            ("steam.app.2", "VRChat"),
        ]);
        assert_eq!(resolve(&api).unwrap().as_deref(), Some("ゲーム ไทย"));
        assert_eq!(*api.calls.borrow(), ["init", "apps", "shutdown"]);
    }

    #[test]
    fn no_steam_app_still_releases_the_session() {
        let api = Fake::new(&[("openvr.overlay", "Other")]);
        assert_eq!(resolve(&api).unwrap(), None);
        assert_eq!(*api.calls.borrow(), ["init", "apps", "shutdown"]);
    }

    #[test]
    fn failed_property_lookup_releases_the_session() {
        let mut api = Fake::new(&[]);
        api.apps = Err("property unavailable".into());
        assert!(resolve(&api).is_err());
        assert_eq!(*api.calls.borrow(), ["init", "apps", "shutdown"]);
    }

    #[test]
    fn failed_initialization_does_not_release_a_session_it_never_acquired() {
        let mut api = Fake::new(&[]);
        api.fail_init = true;
        assert!(resolve(&api).is_err());
        assert_eq!(*api.calls.borrow(), ["init"]);
    }
}
