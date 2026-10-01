//! "Copy the message to the clipboard and paste it into the VR game"
//! (`ENABLE_CLIPBOARD`).
//!
//! Python still owns the SteamVR app name (it comes from OpenVR) and sends it
//! with each request; Rust does the focus, copy and Ctrl+V. Same order and
//! rules as `models/clipboard/clipboard.py`:
//!
//! 1. with a window name, try to focus a window whose title contains it, then
//!    one whose process is called that;
//! 2. copy the text whatever happened above (so the user can paste by hand);
//! 3. press Ctrl+V only if a window really took the focus, otherwise the
//!    keystroke would land in whatever else has it.
//!
//! Focus and Ctrl+V are Windows-only, as they were. The sink is only offered
//! to Python on Windows (`sinks::IMPLEMENTED`); elsewhere Python's own
//! clipboard code keeps running.

#[cfg(windows)]
mod win;

use std::sync::mpsc::{channel, Sender};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

/// Lets a freshly focused window settle before the clipboard is touched.
const FOCUS_SETTLE: Duration = Duration::from_millis(200);

/// The operating-system side, so the sequencing above can be tested alone.
pub trait Desktop: Send + 'static {
    /// Bring a window matching `name` (title substring, then process name) to
    /// the foreground. True when one accepted the focus.
    fn focus_window(&self, name: &str) -> bool;
    fn copy(&self, text: &str) -> bool;
    /// Press Ctrl+V in the focused window.
    fn paste(&self) -> bool;
}

/// Real desktop on Windows; elsewhere nothing works and every step reports so.
#[cfg(windows)]
pub use win::WindowsDesktop as SystemDesktop;

#[cfg(not(windows))]
pub struct SystemDesktop;

#[cfg(not(windows))]
impl Desktop for SystemDesktop {
    fn focus_window(&self, _: &str) -> bool {
        false
    }
    fn copy(&self, _: &str) -> bool {
        false
    }
    fn paste(&self) -> bool {
        false
    }
}

/// True when the text was pasted (not merely copied).
pub fn copy_and_paste(desktop: &impl Desktop, text: &str, window: Option<&str>, settle: Duration) -> bool {
    let focused = match window {
        Some(name) => {
            let focused = desktop.focus_window(name);
            if focused {
                thread::sleep(settle);
            }
            focused
        }
        None => false,
    };
    if !desktop.copy(text) {
        eprintln!("[sinks] clipboard: could not copy to the clipboard");
        return false;
    }
    focused && desktop.paste()
}

struct Job {
    text: String,
    window: Option<String>,
}

/// Runs requests one at a time on a worker thread: focusing waits for the
/// window to settle, and that must not stall the sidecar's output loop.
pub struct ClipboardSink {
    jobs: Mutex<Sender<Job>>,
}

impl ClipboardSink {
    pub fn new() -> Self {
        Self::with_desktop(SystemDesktop, FOCUS_SETTLE)
    }

    pub fn with_desktop(desktop: impl Desktop, settle: Duration) -> Self {
        let (jobs, queue) = channel::<Job>();
        thread::Builder::new()
            .name("vrct-clipboard".into())
            .spawn(move || {
                // Ends when the sink is dropped and the queue closes.
                for job in queue {
                    copy_and_paste(&desktop, &job.text, job.window.as_deref(), settle);
                }
            })
            .expect("spawn clipboard thread");
        Self { jobs: Mutex::new(jobs) }
    }

    /// Queue a copy (and paste into `window`, when given). An empty window
    /// name counts as none: as a title substring it would match every window.
    pub fn copy_and_paste(&self, text: &str, window: Option<&str>) -> Result<(), String> {
        let job = Job {
            text: text.to_string(),
            window: window.filter(|name| !name.is_empty()).map(str::to_string),
        };
        self.jobs
            .lock()
            .unwrap()
            .send(job)
            .map_err(|_| "clipboard worker is gone".to_string())
    }
}

impl Default for ClipboardSink {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Fake {
        calls: Arc<Mutex<Vec<String>>>,
        focus_works: bool,
        copy_works: bool,
    }

    impl Fake {
        fn new(focus_works: bool, copy_works: bool) -> Self {
            Self { focus_works, copy_works, ..Self::default() }
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Desktop for Fake {
        fn focus_window(&self, name: &str) -> bool {
            self.calls.lock().unwrap().push(format!("focus {name}"));
            self.focus_works
        }
        fn copy(&self, text: &str) -> bool {
            self.calls.lock().unwrap().push(format!("copy {text}"));
            self.copy_works
        }
        fn paste(&self) -> bool {
            self.calls.lock().unwrap().push("paste".into());
            true
        }
    }

    fn run(fake: &Fake, window: Option<&str>) -> bool {
        copy_and_paste(fake, "hi", window, Duration::ZERO)
    }

    #[test]
    fn focused_window_gets_the_text_pasted_after_it_is_copied() {
        let fake = Fake::new(true, true);
        assert!(run(&fake, Some("VRChat")));
        assert_eq!(fake.calls(), ["focus VRChat", "copy hi", "paste"]);
    }

    #[test]
    fn without_a_window_the_text_is_only_copied() {
        let fake = Fake::new(true, true);
        assert!(!run(&fake, None));
        assert_eq!(fake.calls(), ["copy hi"]);
    }

    #[test]
    fn a_window_that_refuses_focus_means_copy_only_never_a_blind_ctrl_v() {
        let fake = Fake::new(false, true);
        assert!(!run(&fake, Some("VRChat")));
        assert_eq!(fake.calls(), ["focus VRChat", "copy hi"]);
    }

    #[test]
    fn nothing_is_pasted_when_the_copy_failed() {
        let fake = Fake::new(true, false);
        assert!(!run(&fake, Some("VRChat")));
        assert_eq!(fake.calls(), ["focus VRChat", "copy hi"]);
    }

    #[test]
    fn the_worker_runs_requests_in_order_and_treats_an_empty_window_as_none() {
        let fake = Fake::new(true, true);
        let sink = ClipboardSink::with_desktop(fake.clone(), Duration::ZERO);
        sink.copy_and_paste("one", Some("")).unwrap();
        sink.copy_and_paste("two", Some("Game")).unwrap();
        drop(sink); // closes the queue; the worker drains it first

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while fake.calls().len() < 4 && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(fake.calls(), ["copy one", "focus Game", "copy two", "paste"]);
    }
}
