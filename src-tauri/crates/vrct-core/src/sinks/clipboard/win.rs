//! Win32 side of the clipboard sink: the same calls the Python version made
//! through ctypes (`EnumWindows`, `AttachThreadInput`, `SetForegroundWindow`,
//! `keybd_event`), with the clipboard written directly instead of via `clip`.

use std::path::Path;
use std::ptr::{copy_nonoverlapping, null_mut};
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Foundation::{CloseHandle, GlobalFree, BOOL, HWND, LPARAM, TRUE};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows_sys::Win32::System::Ole::CF_UNICODETEXT;
use windows_sys::Win32::System::Threading::{
    AttachThreadInput, GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{keybd_event, KEYEVENTF_KEYUP, VK_CONTROL};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, EnumWindows, GetForegroundWindow, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsWindowVisible, SetForegroundWindow, ShowWindow, SW_RESTORE,
};

use super::Desktop;

const VK_V: u8 = 0x56;
/// Another program may hold the clipboard for a moment.
const OPEN_ATTEMPTS: u32 = 10;
const OPEN_RETRY: Duration = Duration::from_millis(20);

pub struct WindowsDesktop;

impl Desktop for WindowsDesktop {
    fn focus_window(&self, name: &str) -> bool {
        let windows = top_level_windows();
        let wanted = name.to_lowercase();
        // Title substring first, then the executable's name, like Python.
        let by_title = windows.iter().filter(|hwnd| {
            window_title(**hwnd).is_some_and(|title| title.to_lowercase().contains(&wanted))
        });
        if by_title.into_iter().any(|hwnd| focus(*hwnd)) {
            return true;
        }
        windows
            .iter()
            .filter(|hwnd| process_name(**hwnd).is_some_and(|exe| exe.to_lowercase() == wanted))
            .any(|hwnd| focus(*hwnd))
    }

    fn copy(&self, text: &str) -> bool {
        // An embedded NUL would end the text early for every reader.
        let wide: Vec<u16> = text
            .encode_utf16()
            .filter(|unit| *unit != 0)
            .chain([0])
            .collect();
        if !open_clipboard() {
            return false;
        }
        let copied = unsafe { set_unicode_text(&wide) };
        unsafe { CloseClipboard() };
        copied
    }

    fn paste(&self) -> bool {
        unsafe {
            // Release in reverse order, and always release Ctrl.
            keybd_event(VK_CONTROL as u8, 0, 0, 0);
            keybd_event(VK_V, 0, 0, 0);
            keybd_event(VK_V, 0, KEYEVENTF_KEYUP, 0);
            keybd_event(VK_CONTROL as u8, 0, KEYEVENTF_KEYUP, 0);
        }
        true
    }
}

fn open_clipboard() -> bool {
    for _ in 0..OPEN_ATTEMPTS {
        if unsafe { OpenClipboard(null_mut()) } != 0 {
            return true;
        }
        thread::sleep(OPEN_RETRY);
    }
    false
}

/// # Safety
/// The clipboard must be open on this thread.
unsafe fn set_unicode_text(wide: &[u16]) -> bool {
    if EmptyClipboard() == 0 {
        return false;
    }
    let memory = GlobalAlloc(GMEM_MOVEABLE, std::mem::size_of_val(wide));
    if memory.is_null() {
        return false;
    }
    let target = GlobalLock(memory) as *mut u16;
    if target.is_null() {
        GlobalFree(memory);
        return false;
    }
    copy_nonoverlapping(wide.as_ptr(), target, wide.len());
    GlobalUnlock(memory);
    // On success the clipboard owns the memory; on failure it is still ours.
    if SetClipboardData(CF_UNICODETEXT as u32, memory).is_null() {
        GlobalFree(memory);
        return false;
    }
    true
}

unsafe extern "system" fn collect_window(hwnd: HWND, found: LPARAM) -> BOOL {
    // Hidden windows can match by title (GDI+ creates one called
    // "GDI+ Window (<exe>)" in many programs) but can never take the paste.
    if IsWindowVisible(hwnd) != 0 {
        (*(found as *mut Vec<HWND>)).push(hwnd);
    }
    TRUE
}

/// Visible top-level windows.
fn top_level_windows() -> Vec<HWND> {
    let mut found: Vec<HWND> = Vec::new();
    unsafe { EnumWindows(Some(collect_window), &mut found as *mut Vec<HWND> as LPARAM) };
    found
}

/// The title, or None for a window without one.
fn window_title(hwnd: HWND) -> Option<String> {
    let length = unsafe { GetWindowTextLengthW(hwnd) };
    if length <= 0 {
        return None;
    }
    let mut buffer = vec![0u16; length as usize + 1];
    let written = unsafe { GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
    (written > 0).then(|| String::from_utf16_lossy(&buffer[..written as usize]))
}

/// File name of the executable that owns the window ("VRChat.exe").
fn process_name(hwnd: HWND) -> Option<String> {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return None;
    }
    let mut buffer = [0u16; 1024];
    let mut size = buffer.len() as u32;
    let ok = unsafe { QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut size) };
    unsafe { CloseHandle(process) };
    if ok == 0 {
        return None;
    }
    let path = String::from_utf16_lossy(&buffer[..size as usize]);
    Path::new(&path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

/// Windows refuses `SetForegroundWindow` from a process that was not just
/// in the foreground. Sharing input state with the current foreground
/// thread for the call is the documented way around that.
fn focus(hwnd: HWND) -> bool {
    unsafe {
        ShowWindow(hwnd, SW_RESTORE);
        let this_thread = GetCurrentThreadId();
        let foreground = GetForegroundWindow();
        let foreground_thread = if foreground.is_null() {
            0
        } else {
            GetWindowThreadProcessId(foreground, null_mut())
        };
        let attached = foreground_thread != 0
            && foreground_thread != this_thread
            && AttachThreadInput(this_thread, foreground_thread, 1) != 0;
        BringWindowToTop(hwnd);
        SetForegroundWindow(hwnd);
        // The return value is not proof (it said yes for a hidden helper
        // window); ask who really has the foreground, since Ctrl+V follows.
        let focused = GetForegroundWindow() == hwnd;
        if attached {
            AttachThreadInput(this_thread, foreground_thread, 0);
        }
        focused
    }
}
