//! The optional message log (`LOGGER_FEATURE`): one line per sent/received
//! message, appended to a file under the logs folder.
//!
//! Python still decides *whether* logging is on and which file to use
//! (`start`/`stop`); Rust formats and writes the lines exactly as
//! `logging.Formatter('%(asctime)s - %(name)s - %(levelname)s - %(message)s')`
//! with a `TruncatingFileHandler` did, so existing logs and tooling read the
//! same.
//!
//! One deliberate difference: Python's `logging.getLogger("log")` keeps every
//! handler it ever attached, so after switching the feature off and on again
//! it wrote each line to every earlier log file too. Here only the current
//! file is written.

use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Mutex;

use chrono::{DateTime, Local, TimeZone};

const LOGGER_NAME: &str = "log";
const LEVEL: &str = "INFO";
/// `setupLogger`'s `maxBytes`: the file is emptied in place when it would pass this.
const MAX_BYTES: u64 = 10 * 1024 * 1024;
/// Python opens the file in text mode, so `\n` becomes the platform's line end.
const EOL: &str = if cfg!(windows) { "\r\n" } else { "\n" };

/// One formatted record, line terminator included.
pub fn format_record<Tz: TimeZone>(now: DateTime<Tz>, message: &str) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let line = format!(
        "{} - {LOGGER_NAME} - {LEVEL} - {message}\n",
        now.format("%Y-%m-%d %H:%M:%S,%3f")
    );
    if EOL == "\n" {
        line
    } else {
        line.replace('\n', EOL)
    }
}

struct Target {
    path: PathBuf,
    /// Opened with the first line, like `delay=True`.
    file: Option<File>,
}

#[derive(Default)]
pub struct LoggerSink {
    target: Mutex<Option<Target>>,
}

impl LoggerSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Log to `path` from now on (the file is created with the first line).
    pub fn start(&self, path: &str) -> Result<(), String> {
        if path.is_empty() {
            return Err("logger start without a path".into());
        }
        let path = PathBuf::from(path);
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        *self.target.lock().unwrap() = Some(Target { path, file: None });
        Ok(())
    }

    pub fn stop(&self) {
        *self.target.lock().unwrap() = None;
    }

    /// Append `message` as an INFO record. Without a `start` it is dropped,
    /// as a disabled Python logger dropped it.
    pub fn info(&self, message: &str) -> Result<(), String> {
        self.write(&format_record(Local::now(), message))
    }

    fn write(&self, record: &str) -> Result<(), String> {
        let mut guard = self.target.lock().unwrap();
        let Some(target) = guard.as_mut() else {
            return Ok(());
        };
        if target.file.is_none() {
            // Not `append(true)`: on Windows an append-only handle may not be
            // truncated, and the rollover below empties the file in place.
            // This sink is the file's only writer, so seeking to the end
            // before each write is equivalent.
            let file = OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&target.path)
                .map_err(|e| format!("cannot open {}: {e}", target.path.display()))?;
            target.file = Some(file);
        }
        let file = target.file.as_mut().expect("opened above");
        let size = file.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
        if size + record.len() as u64 >= MAX_BYTES {
            file.set_len(0).map_err(|e| e.to_string())?;
            file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        }
        file.write_all(record.as_bytes()).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn at(ms: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 2, 3, 4, 5).unwrap()
            + chrono::Duration::milliseconds(ms as i64)
    }

    #[test]
    fn record_layout_matches_the_python_formatter() {
        let record = format_record(at(7), "[SENT] hi");
        assert_eq!(
            record,
            format!("2026-10-02 03:04:05,007 - log - INFO - [SENT] hi{EOL}")
        );
    }

    #[test]
    fn milliseconds_are_zero_padded_to_three_digits() {
        assert!(format_record(at(0), "").contains("03:04:05,000 - "));
        assert!(format_record(at(999), "").contains("03:04:05,999 - "));
    }

    #[test]
    fn newlines_inside_a_message_use_the_platform_line_end() {
        let record = format_record(at(0), "a\nb");
        assert!(record.contains(&format!("a{EOL}b{EOL}")), "{record:?}");
    }
}
