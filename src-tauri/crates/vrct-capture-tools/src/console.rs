use std::io::{self, IsTerminal};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

#[cfg(windows)]
#[link(name = "msvcrt")]
extern "C" {
    fn _kbhit() -> i32;
    fn _getwch() -> u16;
}
pub fn read_key() -> Option<char> {
    if !io::stdin().is_terminal() {
        return None;
    }
    #[cfg(windows)]
    unsafe {
        if _kbhit() == 0 {
            return None;
        }
        let value = _getwch();
        if value == 0 || value == 224 {
            _getwch();
            return None;
        }
        return char::from_u32(value as u32).map(|c| c.to_ascii_lowercase());
    }
    #[cfg(not(windows))]
    None
}
pub fn interrupt_flag() -> Result<Arc<AtomicBool>, String> {
    let stopped = Arc::new(AtomicBool::new(false));
    let signal = stopped.clone();
    ctrlc::set_handler(move || signal.store(true, Ordering::Release)).map_err(|e| e.to_string())?;
    Ok(stopped)
}
pub fn confirm_start(stopped: &AtomicBool) -> Result<bool, String> {
    if !io::stdin().is_terminal() {
        return Err(
            "Use --start to send from a non-interactive terminal, or --dry-run to preview".into(),
        );
    }
    println!("Press Enter to START sending, or type Q then Enter to cancel:");
    let mut cancelled = false;
    while !stopped.load(Ordering::Acquire) {
        if let Some(key) = read_key() {
            if key == '\r' || key == '\n' {
                return Ok(!cancelled);
            }
            cancelled |= !key.is_whitespace();
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Ok(false)
}
pub fn finish_default_launch() {
    if std::env::args_os().len() == 1 && io::stdin().is_terminal() {
        println!("Press Enter to close...");
        let mut line = String::new();
        let _ = io::stdin().read_line(&mut line);
    }
}
