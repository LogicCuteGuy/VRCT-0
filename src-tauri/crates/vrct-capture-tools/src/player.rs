use crate::{base_directory, console, run_id, Random};
use clap::{Parser, ValueEnum};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File},
    io::{BufWriter, Write},
    net::UdpSocket,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    time::{Duration, Instant},
};
use unicode_general_category::{get_general_category, GeneralCategory};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Length {
    Tiny,
    Short,
    Medium,
    Long,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Sample {
    pub id: String,
    pub language: String,
    pub text: String,
    pub tags: Vec<String>,
}
impl Sample {
    pub fn units(&self) -> usize {
        self.text.encode_utf16().count()
    }
    pub fn length(&self) -> Length {
        match self.units() {
            0..=5 => Length::Tiny,
            6..=30 => Length::Short,
            31..=80 => Length::Medium,
            _ => Length::Long,
        }
    }
}
#[derive(Deserialize)]
struct Row {
    text: String,
    #[serde(default)]
    tags: Vec<String>,
}
pub fn valid_language(language: &str) -> bool {
    if language == "mixed" {
        return true;
    }
    let (base, script) = language
        .split_once('-')
        .map_or((language, None), |(a, b)| (a, Some(b)));
    (2..=3).contains(&base.len())
        && base.bytes().all(|b| b.is_ascii_lowercase())
        && script.is_none_or(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphabetic()))
}
pub fn load_samples(folder: &Path) -> Result<Vec<Sample>, String> {
    let mut files: Vec<_> = fs::read_dir(folder)
        .map_err(|e| format!("No sample JSON files found in {}: {e}", folder.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    files.retain(|path| {
        path.extension().is_some_and(|extension| {
            if cfg!(windows) {
                extension.to_string_lossy().eq_ignore_ascii_case("json")
            } else {
                extension == "json"
            }
        })
    });
    #[cfg(windows)]
    files.sort_by_key(|path| path.as_os_str().to_string_lossy().to_lowercase());
    #[cfg(not(windows))]
    files.sort();
    if files.is_empty() {
        return Err(format!(
            "No sample JSON files found in {}",
            folder.display()
        ));
    }
    let mut languages = HashSet::new();
    let mut samples = Vec::new();
    for path in files {
        let raw = fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let data: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(raw.trim_start_matches('\u{feff}'))
                .map_err(|e| format!("{}: {e}", path.display()))?;
        for (language, rows) in data {
            if !valid_language(&language) || !languages.insert(language.clone()) {
                return Err(format!("Invalid or repeated language {language:?}"));
            }
            let rows: Vec<Row> =
                serde_json::from_value(rows).map_err(|e| format!("{language}: {e}"))?;
            if rows.is_empty() {
                return Err(format!("{language} needs a nonempty list"));
            }
            for (index, row) in rows.into_iter().enumerate() {
                let sample = Sample {
                    id: format!("{language}_{:03}", index + 1),
                    language: language.clone(),
                    text: row.text,
                    tags: row.tags,
                };
                if sample.text.trim().is_empty()
                    || sample.text.chars().any(|c| {
                        c != '\n'
                            && matches!(
                                get_general_category(c),
                                GeneralCategory::Control
                                    | GeneralCategory::Format
                                    | GeneralCategory::PrivateUse
                                    | GeneralCategory::Surrogate
                                    | GeneralCategory::Unassigned
                            )
                    })
                {
                    return Err(format!(
                        "{}: blank text or unsupported control character",
                        sample.id
                    ));
                }
                if sample.units() > 144 || sample.text.bytes().filter(|b| *b == b'\n').count() >= 9
                {
                    return Err(format!(
                        "{}: exceeds 144 UTF-16 units or 9 explicit lines",
                        sample.id
                    ));
                }
                samples.push(sample);
            }
        }
    }
    Ok(samples)
}
pub fn pad_string(packet: &mut Vec<u8>, text: &str) {
    packet.extend_from_slice(text.as_bytes());
    packet.push(0);
    while !packet.len().is_multiple_of(4) {
        packet.push(0);
    }
}
pub fn message_bytes(text: &str) -> Vec<u8> {
    let mut packet = Vec::new();
    pad_string(&mut packet, "/chatbox/input");
    pad_string(&mut packet, ",sTF");
    pad_string(&mut packet, text);
    packet
}
/// Generate the two human/machine catalogues formerly written by the Python
/// packaging builder. Explicit export replaces only these named outputs.
pub fn export_catalogue(samples: &[Sample], directory: &Path) -> Result<(), String> {
    let newline = if cfg!(windows) { "\r\n" } else { "\n" };
    let mut jsonl = String::new();
    let mut text = String::new();
    for (index, sample) in samples.iter().enumerate() {
        let length = format!("{:?}", sample.length()).to_lowercase();
        let mut record = serde_json::to_value(sample).map_err(|e| e.to_string())?;
        record.as_object_mut().unwrap().extend(
            serde_json::json!({
                "utf16_units":sample.units(), "length":length,
                "explicit_lines":sample.text.matches('\n').count()+1
            })
            .as_object()
            .unwrap()
            .clone(),
        );
        jsonl.push_str(&record.to_string());
        jsonl.push_str(newline);
        if index != 0 {
            text.push_str(newline);
        }
        text.push_str(&format!(
            "[{} / {length} / {} UTF-16 units]{newline}",
            sample.id,
            sample.units()
        ));
        text.push_str(&sample.text.replace('\n', newline));
        text.push_str(newline);
    }
    let mut text_bytes = vec![0xef, 0xbb, 0xbf];
    text_bytes.extend_from_slice(text.as_bytes());
    fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let identity = run_id()?;
    for (name, bytes) in [
        ("samples.jsonl", jsonl.as_bytes()),
        ("samples.txt", text_bytes.as_slice()),
    ] {
        let temporary = directory.join(format!(".{identity}-{name}.part"));
        let output = directory.join(name);
        let mut owned = false;
        let result = (|| -> std::io::Result<()> {
            let mut file = File::options()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            owned = true;
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            replace_catalogue(&temporary, &output)
        })();
        if result.is_err() && owned {
            let _ = fs::remove_file(&temporary);
        }
        result.map_err(|e| format!("Cannot export {}: {e}", output.display()))?;
    }
    Ok(())
}
fn replace_catalogue(source: &Path, output: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        };
        let source: Vec<u16> = source.as_os_str().encode_wide().chain([0]).collect();
        let output: Vec<u16> = output.as_os_str().encode_wide().chain([0]).collect();
        if unsafe {
            MoveFileExW(
                source.as_ptr(),
                output.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    fs::rename(source, output)
}
/// Time is supplied at due()/completed() boundaries so slow sends cannot create
/// catch-up bursts. Command handling always happens before due().
pub struct Playback {
    pub order: Vec<Sample>,
    pub index: usize,
    pub count: usize,
    pub cycle: usize,
    pub paused: bool,
    pub stopped: bool,
    pub deadline: Duration,
    interval: Duration,
    once: bool,
    max_messages: usize,
    ordered: bool,
    random: Random,
}
impl Playback {
    pub fn new(
        samples: Vec<Sample>,
        interval: Duration,
        once: bool,
        max_messages: usize,
        ordered: bool,
        seed: Option<&str>,
    ) -> Result<Self, String> {
        if samples.is_empty() {
            return Err("No samples match selected filters".into());
        }
        let mut state = Self {
            order: samples,
            index: 0,
            count: 0,
            cycle: 1,
            paused: false,
            stopped: false,
            deadline: Duration::ZERO,
            interval,
            once,
            max_messages,
            ordered,
            random: Random::new(seed)?,
        };
        if !ordered {
            state.random.shuffle(&mut state.order);
        }
        Ok(state)
    }
    pub fn command(&mut self, key: char, now: Duration) -> Option<String> {
        match key {
            'q' => self.stopped = true,
            'p' => {
                self.paused = !self.paused;
                self.deadline = now.saturating_add(self.interval);
                return Some(
                    if self.paused {
                        "[paused] P to resume"
                    } else {
                        "[resumed]"
                    }
                    .into(),
                );
            }
            'r' => {
                return Some(format!(
                    "[status] count={}, cycle={}, paused={}",
                    self.count, self.cycle, self.paused
                ))
            }
            _ => {}
        }
        None
    }
    pub fn due(&self, now: Duration) -> Option<&Sample> {
        (!self.stopped && !self.paused && now >= self.deadline).then(|| &self.order[self.index])
    }
    pub fn completed(&mut self, now: Duration) {
        self.count += 1;
        self.index += 1;
        self.deadline = now.saturating_add(self.interval);
        if self.max_messages > 0 && self.count >= self.max_messages {
            self.stopped = true;
        }
        if self.index == self.order.len() {
            if self.once {
                self.stopped = true;
            } else {
                self.index = 0;
                self.cycle += 1;
                if !self.ordered {
                    self.random.shuffle(&mut self.order);
                }
            }
        }
    }
}
#[derive(Parser, Debug)]
#[command(
    about = "Play multilingual samples into the local VRChat chatbox. UDP submission does not confirm display."
)]
pub struct Args {
    #[arg(long)]
    pub samples: Option<PathBuf>,
    #[arg(
        long,
        help = "Write samples.jsonl and UTF-8 BOM samples.txt; never send OSC"
    )]
    pub export_catalogue: Option<PathBuf>,
    #[arg(long, default_value_t = 9000)]
    pub port: u16,
    #[arg(long, default_value_t = 6.0)]
    pub interval: f64,
    #[arg(long)]
    pub languages: Option<String>,
    #[arg(long, value_enum)]
    pub length: Option<Length>,
    #[arg(long)]
    pub ordered: bool,
    #[arg(long, allow_hyphen_values = true, value_parser=crate::parse_seed)]
    pub seed: Option<String>,
    #[arg(long)]
    pub once: bool,
    #[arg(long, default_value_t = 0)]
    pub max_messages: usize,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub list: bool,
    #[arg(long)]
    pub start: bool,
    #[arg(long)]
    pub log_dir: Option<PathBuf>,
    #[arg(long, default_value_t = 1.0)]
    pub timeout: f64,
}
impl Args {
    pub fn validate(&self) -> Result<(), String> {
        if self.port == 0 {
            return Err("--port must be 1..65535".into());
        }
        if !self.interval.is_finite() || self.interval < 3.0 {
            return Err("--interval must be finite and at least 3 seconds".into());
        }
        Duration::try_from_secs_f64(self.interval)
            .map_err(|e| format!("--interval is out of range: {e}"))?;
        if !self.timeout.is_finite() || self.timeout <= 0.0 || self.timeout > 3600.0 {
            return Err("--timeout must be finite, positive, at most 3600 seconds".into());
        }
        Ok(())
    }
}
pub fn select(args: &Args) -> Result<Vec<Sample>, String> {
    args.validate()?;
    let bundled = base_directory().join("chatbox_samples");
    let folder = args.samples.clone().unwrap_or_else(|| {
        #[cfg(debug_assertions)]
        if !bundled.is_dir() {
            return Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tools/chatbox_samples");
        }
        bundled
    });
    let mut samples = load_samples(&folder)?;
    if let Some(languages) = &args.languages {
        let wanted: HashSet<_> = languages.split(',').map(str::trim).collect();
        let present: HashSet<_> = samples.iter().map(|s| s.language.as_str()).collect();
        let mut unknown: Vec<_> = wanted.difference(&present).copied().collect();
        unknown.sort();
        if !unknown.is_empty() {
            return Err(format!("Unknown languages: {}", unknown.join(", ")));
        }
        samples.retain(|s| wanted.contains(s.language.as_str()));
    }
    if let Some(length) = args.length {
        samples.retain(|s| s.length() == length);
    }
    if samples.is_empty() {
        return Err("No samples match selected filters".into());
    }
    Ok(samples)
}
pub fn run(args: Args) -> Result<(), String> {
    let samples = select(&args)?;
    let mut languages = BTreeMap::new();
    let mut lengths = BTreeMap::new();
    for sample in &samples {
        *languages.entry(&sample.language).or_insert(0usize) += 1;
        *lengths
            .entry(format!("{:?}", sample.length()).to_lowercase())
            .or_insert(0usize) += 1;
    }
    println!(
        "Samples: {}, languages: {languages:?}\nLengths (UTF-16 units): {lengths:?}",
        samples.len()
    );
    if let Some(directory) = &args.export_catalogue {
        export_catalogue(&samples, directory)?;
        println!(
            "Exported {} samples to {}",
            samples.len(),
            directory.display()
        );
        return Ok(());
    }
    if args.list {
        return Ok(());
    }
    let stop = console::interrupt_flag()?;
    if !args.dry_run && !args.start && !console::confirm_start(&stop)? {
        println!("Cancelled; nothing sent.");
        return Ok(());
    }
    println!(
        "{}, interval={}s\nP=pause/resume  Q=quit  R=status (console focused)",
        if args.dry_run {
            "DRY RUN (no network)".into()
        } else {
            format!("OSC -> 127.0.0.1:{}", args.port)
        },
        args.interval
    );
    let (socket, mut log) = if args.dry_run {
        (None, None)
    } else {
        let directory = args
            .log_dir
            .unwrap_or_else(|| base_directory().join("sent_logs"));
        fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        let path = directory.join(format!("{}.jsonl", run_id()?));
        let file = File::options()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| e.to_string())?;
        let socket = UdpSocket::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
        socket
            .set_write_timeout(Some(Duration::from_secs_f64(args.timeout)))
            .map_err(|e| e.to_string())?;
        println!("Submission log: {}", path.display());
        (Some(socket), Some(BufWriter::new(file)))
    };
    let mut player = Playback::new(
        samples,
        Duration::from_secs_f64(args.interval),
        args.once,
        args.max_messages,
        args.ordered,
        args.seed.as_deref(),
    )?;
    let clock = Instant::now();
    while !player.stopped && !stop.load(Ordering::Acquire) {
        if let Some(key) = console::read_key() {
            if let Some(message) = player.command(key, clock.elapsed()) {
                println!("{message}");
            }
        }
        if stop.load(Ordering::Acquire) {
            break;
        }
        if let Some(sample) = player.due(clock.elapsed()) {
            if let Some(socket) = &socket {
                let packet = message_bytes(&sample.text);
                let target = format!("127.0.0.1:{}", args.port);
                if socket
                    .send_to(&packet, &target)
                    .map_err(|e| e.to_string())?
                    != packet.len()
                {
                    return Err("Incomplete UDP datagram".into());
                }
                let record = serde_json::json!({"submitted_at_utc":chrono::Utc::now().to_rfc3339(),"event":"udp_submitted","id":sample.id,"language":sample.language,"text":sample.text,"utf16_units":sample.units(),"explicit_lines":sample.text.matches('\n').count()+1,"target":target,"tags":sample.tags});
                let writer = log.as_mut().unwrap();
                writeln!(writer, "{record}")
                    .and_then(|_| writer.flush())
                    .map_err(|e| e.to_string())?;
            }
            println!(
                "[{}] {} {:?} {}/144: {}",
                if args.dry_run { "preview" } else { "submitted" },
                sample.id,
                sample.length(),
                sample.units(),
                sample.text.replace('\n', " \\n ")
            );
            player.completed(clock.elapsed());
        }
        if !player.stopped {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    println!(
        "done: {}={}",
        if args.dry_run {
            "previewed"
        } else {
            "submitted"
        },
        player.count
    );
    Ok(())
}
