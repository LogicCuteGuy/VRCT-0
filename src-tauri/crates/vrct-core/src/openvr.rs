//! Process-wide OpenVR ownership shared by overlay and clipboard. Valve's
//! VR_ShutdownInternal disconnects the whole process; only the final lease may
//! call it. Initialization and final shutdown run under the same mutex.
#[cfg(all(windows, target_arch = "x86_64"))]
pub mod capture;
use std::sync::{Arc, Mutex};

pub trait Connection: Send + Sync {
    fn shutdown(&self);
}

struct State<T> {
    connection: Option<Arc<T>>,
    references: usize,
}
type Factory<T> = dyn Fn() -> Result<T, String> + Send + Sync;
pub struct SessionManager<T: Connection> {
    state: Mutex<State<T>>,
    factory: Box<Factory<T>>,
}
impl<T: Connection> SessionManager<T> {
    pub fn new(factory: impl Fn() -> Result<T, String> + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                connection: None,
                references: 0,
            }),
            factory: Box::new(factory),
        })
    }
    pub fn acquire(self: &Arc<Self>) -> Result<SessionLease<T>, String> {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.connection.is_none() {
            state.connection = Some(Arc::new((self.factory)()?));
        }
        let connection = state
            .connection
            .as_ref()
            .expect("connection initialized")
            .clone();
        state.references += 1;
        Ok(SessionLease {
            manager: self.clone(),
            connection,
        })
    }
}
pub struct SessionLease<T: Connection> {
    manager: Arc<SessionManager<T>>,
    connection: Arc<T>,
}
impl<T: Connection> SessionLease<T> {
    pub fn connection(&self) -> &T {
        &self.connection
    }
}
impl<T: Connection> Drop for SessionLease<T> {
    fn drop(&mut self) {
        let mut state = self.manager.state.lock().unwrap_or_else(|p| p.into_inner());
        state.references -= 1;
        if state.references == 0 {
            if let Some(connection) = state.connection.take() {
                connection.shutdown();
            }
        }
    }
}

#[cfg(all(windows, target_arch = "x86_64"))]
pub(crate) mod native {
    use super::*;
    use libloading::Library;
    use std::{
        ffi::{c_char, c_void, CStr},
        path::PathBuf,
        sync::OnceLock,
    };
    type Init = unsafe extern "C" fn(*mut u32, u32) -> usize;
    type Shutdown = unsafe extern "C" fn();
    type Interface = unsafe extern "C" fn(*const c_char, *mut u32) -> *const c_void;
    pub struct Api {
        _library: Library,
        shutdown: Shutdown,
        interface: Interface,
    }
    impl Api {
        fn load() -> Result<Self, String> {
            // SAFETY: exact bundled absolute DLL path and Valve SDK exports.
            let library = unsafe { Library::new(library_path()?) }.map_err(|e| e.to_string())?;
            let (init, shutdown, interface) = unsafe {
                (
                    *library
                        .get::<Init>(b"VR_InitInternal\0")
                        .map_err(|e| e.to_string())?,
                    *library
                        .get::<Shutdown>(b"VR_ShutdownInternal\0")
                        .map_err(|e| e.to_string())?,
                    *library
                        .get::<Interface>(b"VR_GetGenericInterface\0")
                        .map_err(|e| e.to_string())?,
                )
            };
            let mut error = 0;
            // Background = 3, so a lease never launches SteamVR itself.
            unsafe {
                init(&mut error, 3);
            }
            if error != 0 {
                return Err(format!("OpenVR initialization failed ({error})"));
            }
            Ok(Self {
                _library: library,
                shutdown,
                interface,
            })
        }
        /// # Safety
        /// T must match the pinned SDK function table requested by `name`.
        /// Every call through the returned pointer must retain this lease.
        pub unsafe fn interface<T>(&self, name: &CStr) -> Result<*const T, String> {
            let mut error = 0;
            let table = (self.interface)(name.as_ptr(), &mut error);
            if error != 0 || table.is_null() {
                return Err(format!(
                    "OpenVR {} interface unavailable ({error})",
                    name.to_string_lossy()
                ));
            }
            Ok(table.cast())
        }
    }
    impl Connection for Api {
        fn shutdown(&self) {
            unsafe {
                (self.shutdown)();
            }
        }
    }
    pub type Lease = SessionLease<Api>;
    pub fn acquire() -> Result<Lease, String> {
        static MANAGER: OnceLock<Arc<SessionManager<Api>>> = OnceLock::new();
        MANAGER
            .get_or_init(|| SessionManager::new(Api::load))
            .acquire()
    }
    pub fn library_path() -> Result<PathBuf, String> {
        let executable = std::env::current_exe().map_err(|e| e.to_string())?;
        let packaged = executable
            .parent()
            .ok_or("No executable directory")?
            .join("openvr_api.dll");
        if packaged.is_file() {
            return Ok(packaged);
        }
        #[cfg(debug_assertions)]
        {
            let development = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../resources/openvr/openvr_api.dll");
            if development.is_file() {
                return Ok(development);
            }
        }
        Err("Bundled OpenVR library not found".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Fake(Arc<AtomicUsize>);
    impl Connection for Fake {
        fn shutdown(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    #[test]
    fn clipboard_release_does_not_disconnect_overlay_and_final_release_does() {
        let initializes = Arc::new(AtomicUsize::new(0));
        let shutdowns = Arc::new(AtomicUsize::new(0));
        let init = initializes.clone();
        let stop = shutdowns.clone();
        let manager = SessionManager::new(move || {
            init.fetch_add(1, Ordering::SeqCst);
            Ok(Fake(stop.clone()))
        });
        let overlay = manager.acquire().unwrap();
        {
            let _clipboard = manager.acquire().unwrap();
        }
        assert_eq!(initializes.load(Ordering::SeqCst), 1);
        assert_eq!(shutdowns.load(Ordering::SeqCst), 0);
        drop(overlay);
        assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
        drop(manager.acquire().unwrap());
        assert_eq!(initializes.load(Ordering::SeqCst), 2);
        assert_eq!(shutdowns.load(Ordering::SeqCst), 2);
    }
    #[test]
    fn failed_acquire_has_no_shutdown_and_can_retry() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = attempts.clone();
        let shutdowns = Arc::new(AtomicUsize::new(0));
        let stop = shutdowns.clone();
        let manager = SessionManager::new(move || {
            if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                Err("no SteamVR".into())
            } else {
                Ok(Fake(stop.clone()))
            }
        });
        assert!(manager.acquire().is_err());
        assert_eq!(shutdowns.load(Ordering::SeqCst), 0);
        drop(manager.acquire().unwrap());
        assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn concurrent_clipboard_sessions_cannot_shutdown_while_overlay_holds_lease() {
        let shutdowns = Arc::new(AtomicUsize::new(0));
        let stop = shutdowns.clone();
        let manager = SessionManager::new(move || Ok(Fake(stop.clone())));
        let overlay = manager.acquire().unwrap();
        let workers: Vec<_> = (0..32)
            .map(|_| {
                let manager = manager.clone();
                std::thread::spawn(move || {
                    for _ in 0..100 {
                        drop(manager.acquire().unwrap());
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(shutdowns.load(Ordering::SeqCst), 0);
        drop(overlay);
        assert_eq!(shutdowns.load(Ordering::SeqCst), 1);
    }
}
