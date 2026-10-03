//! Offscreen HWND capture. PrintWindow reads the selected window's surface;
//! overlapping desktop applications never become OCR input.
use image::RgbImage;
use std::ffi::c_void;
use std::ptr::null_mut;
use windows_sys::Win32::{
    Foundation::*, Graphics::Gdi::*, System::Threading::*, UI::WindowsAndMessaging::*,
};

#[link(name = "user32")]
extern "system" {
    fn PrintWindow(hwnd: HWND, hdc: HDC, flags: u32) -> BOOL;
}

pub struct HwndCapture {
    title: String,
    hwnd: HWND,
}
impl HwndCapture {
    pub fn new(title: String) -> Self {
        Self {
            title,
            hwnd: null_mut(),
        }
    }
    pub fn set_title(&mut self, title: &str) {
        if self.title != title {
            self.title = title.into();
            self.hwnd = null_mut();
        }
    }
    pub fn capture(&mut self) -> Result<Option<RgbImage>, String> {
        // All HWND/GDI objects are created, used and released on this thread.
        unsafe {
            if self.hwnd.is_null() || IsWindow(self.hwnd) == 0 {
                self.hwnd = find_window(&self.title);
            }
            if self.hwnd.is_null() || IsIconic(self.hwnd) != 0 {
                return Ok(None);
            }
            let mut rect: RECT = std::mem::zeroed();
            if GetWindowRect(self.hwnd, &mut rect) == 0 {
                self.hwnd = null_mut();
                return Ok(None);
            }
            let width = rect.right - rect.left;
            let height = rect.bottom - rect.top;
            if width <= 0
                || height <= 0
                || width > 16384
                || height > 16384
                || width as u64 * height as u64 > 64 * 1024 * 1024
            {
                return Ok(None);
            }
            let dc = GetWindowDC(self.hwnd);
            if dc.is_null() {
                return Ok(None);
            }
            let mut objects = Objects {
                hwnd: self.hwnd,
                dc,
                memory: null_mut(),
                bitmap: null_mut(),
                old: null_mut(),
            };
            objects.memory = CreateCompatibleDC(dc);
            if objects.memory.is_null() {
                return Ok(None);
            }
            objects.bitmap = CreateCompatibleBitmap(dc, width, height);
            if objects.bitmap.is_null() {
                return Ok(None);
            }
            objects.old = SelectObject(objects.memory, objects.bitmap);
            if PrintWindow(self.hwnd, objects.memory, 2) == 0 {
                return Ok(None);
            }
            SelectObject(objects.memory, objects.old);
            objects.old = null_mut();
            let mut info: BITMAPINFO = std::mem::zeroed();
            info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            info.bmiHeader.biWidth = width;
            info.bmiHeader.biHeight = -height;
            info.bmiHeader.biPlanes = 1;
            info.bmiHeader.biBitCount = 32;
            info.bmiHeader.biCompression = BI_RGB;
            let mut bgra = vec![0u8; width as usize * height as usize * 4];
            if GetDIBits(
                objects.memory,
                objects.bitmap,
                0,
                height as u32,
                bgra.as_mut_ptr().cast::<c_void>(),
                &mut info,
                DIB_RGB_COLORS,
            ) != height
            {
                return Ok(None);
            }
            let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
            for pixel in bgra.chunks_exact(4) {
                rgb.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
            }
            let Some(image) = RgbImage::from_raw(width as u32, height as u32, rgb) else {
                return Ok(None);
            };
            if blank(&image, 3.0, 20.0) {
                return Ok(None);
            }
            Ok(Some(image))
        }
    }
}
struct Objects {
    hwnd: HWND,
    dc: HDC,
    memory: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
}
impl Drop for Objects {
    fn drop(&mut self) {
        unsafe {
            if !self.old.is_null() && !self.memory.is_null() {
                SelectObject(self.memory, self.old);
            }
            if !self.bitmap.is_null() {
                DeleteObject(self.bitmap);
            }
            if !self.memory.is_null() {
                DeleteDC(self.memory);
            }
            ReleaseDC(self.hwnd, self.dc);
        }
    }
}
struct WindowSearch<'a> {
    title: &'a str,
    exact: HWND,
    partial: HWND,
    process: HWND,
}
unsafe extern "system" fn enumerate(hwnd: HWND, parameter: LPARAM) -> BOOL {
    let search = unsafe { &mut *(parameter as *mut WindowSearch<'_>) };
    unsafe {
        if IsWindowVisible(hwnd) == 0 {
            return 1;
        }
        let count = GetWindowTextLengthW(hwnd).clamp(0, 32768) as usize;
        if count > 0 {
            let mut title = vec![0u16; count + 1];
            let count =
                GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32).max(0) as usize;
            let title = String::from_utf16_lossy(&title[..count]);
            if title == search.title && search.exact.is_null() {
                search.exact = hwnd;
            }
            if title.to_lowercase().contains(&search.title.to_lowercase())
                && search.partial.is_null()
            {
                search.partial = hwnd;
            }
        }
        if search.process.is_null() {
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, &mut pid);
            if process_name(pid).is_some_and(|name| {
                name.trim_end_matches(".exe")
                    == search.title.to_lowercase().trim_end_matches(".exe")
            }) {
                search.process = hwnd;
            }
        }
    }
    1
}
fn find_window(title: &str) -> HWND {
    let mut search = WindowSearch {
        title,
        exact: null_mut(),
        partial: null_mut(),
        process: null_mut(),
    };
    unsafe {
        EnumWindows(Some(enumerate), &mut search as *mut _ as LPARAM);
    }
    if !search.exact.is_null() {
        search.exact
    } else if !search.partial.is_null() {
        search.partial
    } else {
        search.process
    }
}
pub(crate) fn process_name(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut path = vec![0u16; 32768];
        let mut length = path.len() as u32;
        let success = QueryFullProcessImageNameW(handle, 0, path.as_mut_ptr(), &mut length);
        CloseHandle(handle);
        if success == 0 {
            return None;
        }
        let path = String::from_utf16_lossy(&path[..length as usize]);
        Some(path.rsplit(['\\', '/']).next()?.to_lowercase())
    }
}
pub fn blank(image: &RgbImage, mean_threshold: f64, variance_threshold: f64) -> bool {
    if image.as_raw().is_empty() {
        return true;
    }
    let count = image.as_raw().len() as f64;
    let sum = image.as_raw().iter().map(|&x| x as f64).sum::<f64>();
    let squared = image
        .as_raw()
        .iter()
        .map(|&x| (x as f64).powi(2))
        .sum::<f64>();
    let mean = sum / count;
    mean < mean_threshold || squared / count - mean * mean < variance_threshold
}
