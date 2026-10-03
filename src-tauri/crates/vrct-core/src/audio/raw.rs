//! The machine side of a mic/speaker session on Windows: a device read in its own format for the
//! energy-threshold recorder, and the WASAPI capture plus Silero for the VAD recorder.
//!
//! The energy recorder works on what the device delivers: its sample rate, and for a speaker all of its
//! channels (PyAudio opened microphones with one channel and loopback devices with as many as they have).
//! [`RawSource`] collects what cpal's audio thread delivers and hands it out in `chunk`-sized reads, the way
//! a PortAudio stream's `read` does: a read waits until a whole chunk is there.

use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::traits::{DeviceTrait, StreamTrait};

use super::capture::{find_device, raw_format, Capture, Source};
use super::devices::{Device, DeviceList};
use super::silero::{OnnxRuntime, SileroFrameProbability};
use super::vad::{SegmentIds, VadConfig, VadSegmenter};
use super::wasapi::list_devices;
use crate::transcription::energy;
use crate::transcription::native::Platform;
use crate::transcription::recorder::{CaptureFactory, CaptureFailure, Capturing, EnergyParams, EnergyRecorder, PcmSink, Recorder, VadRecorder};
use crate::transcription::session::Kind;

/// Frames per read: `speech_recognition.Microphone.CHUNK`.
const CHUNK_FRAMES: usize = 1024;
/// What is kept when nobody reads (a paused recorder): older audio is let go rather than replayed on resume.
const KEEP: Duration = Duration::from_millis(500);

#[derive(Default)]
struct Buffer {
    bytes: VecDeque<u8>,
    failure: Option<String>,
}

struct Shared {
    buffer: Mutex<Buffer>,
    arrived: Condvar,
    closed: AtomicBool,
}

impl Shared {
    fn buffer(&self) -> MutexGuard<'_, Buffer> {
        self.buffer.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Adds audio, letting the oldest go when more than `keep` bytes are waiting. Whole frames only, so the
    /// channels never shift.
    fn push(&self, pcm: impl IntoIterator<Item = u8>, keep: usize, frame: usize) {
        let mut buffer = self.buffer();
        buffer.bytes.extend(pcm);
        let excess = buffer.bytes.len().saturating_sub(keep);
        if excess > 0 {
            buffer.bytes.drain(..excess.div_ceil(frame) * frame);
        }
        drop(buffer);
        self.arrived.notify_all();
    }

    fn fail(&self, error: String) {
        self.buffer().failure = Some(error);
        self.arrived.notify_all();
    }
}

/// Feeds a [`RawSource`] that has no device behind it.
pub struct Feed {
    shared: Arc<Shared>,
    keep: usize,
    frame: usize,
}

impl Feed {
    /// 16-bit interleaved audio, as a device would deliver it.
    pub fn push(&self, pcm: &[u8]) {
        self.shared.push(pcm.iter().copied(), self.keep, self.frame);
    }

    pub fn fail(&self, error: &str) {
        self.shared.fail(error.to_string());
    }
}

/// Makes a read that is waiting for audio return (the energy recorder's way of unblocking a stream).
#[derive(Clone)]
pub struct Closer(Arc<Shared>);

impl Closer {
    pub fn close(&self) {
        self.0.closed.store(true, Ordering::SeqCst);
        self.0.arrived.notify_all();
    }
}

pub struct RawSource {
    shared: Arc<Shared>,
    rate: u32,
    channels: u32,
    stream: Option<(Sender<()>, JoinHandle<()>)>,
}

impl RawSource {
    /// Opens the device called `name` and starts collecting. `mono` mixes the channels down to one (a
    /// microphone), otherwise they are delivered interleaved as the device has them.
    pub fn open(source: Source, name: &str, mono: bool) -> Result<(RawSource, Closer), String> {
        let shared = Arc::new(Shared { buffer: Mutex::new(Buffer::default()), arrived: Condvar::new(), closed: AtomicBool::new(false) });
        let name = name.to_string();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(u32, u32), String>>();

        let worker = Arc::clone(&shared);
        // cpal's streams must stay on the thread that built them.
        let thread = std::thread::Builder::new()
            .name("vrct-raw-capture".into())
            .spawn(move || {
                let stream = match build(source, &name, mono, &worker) {
                    Ok((stream, rate, channels)) => {
                        let _ = ready_tx.send(Ok((rate, channels)));
                        stream
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                let _ = stop_rx.recv();
                drop(stream);
            })
            .map_err(|e| format!("cannot start the capture thread: {e}"))?;
        let (rate, channels) = match ready_rx.recv() {
            Ok(Ok(format)) => format,
            Ok(Err(error)) => {
                let _ = thread.join();
                return Err(error);
            }
            Err(_) => return Err("the capture thread ended before it was ready".to_string()),
        };
        let closer = Closer(Arc::clone(&shared));
        Ok((RawSource { shared, rate, channels, stream: Some((stop_tx, thread)) }, closer))
    }

    /// A source with no device, fed by hand (for tests). It keeps `keep` bytes of unread audio.
    pub fn detached(rate: u32, channels: u32, keep: usize) -> (RawSource, Feed, Closer) {
        let shared = Arc::new(Shared { buffer: Mutex::new(Buffer::default()), arrived: Condvar::new(), closed: AtomicBool::new(false) });
        let feed = Feed { shared: Arc::clone(&shared), keep, frame: channels as usize * 2 };
        let closer = Closer(Arc::clone(&shared));
        (RawSource { shared, rate, channels, stream: None }, feed, closer)
    }

    /// Interleaved channels in each frame.
    pub fn channels(&self) -> u32 {
        self.channels
    }
}

fn build(source: Source, name: &str, mono: bool, shared: &Arc<Shared>) -> Result<(cpal::Stream, u32, u32), String> {
    let (device, supported) = find_device(source, name)?;
    let format = raw_format(supported.sample_format())
        .ok_or_else(|| format!("{name}: sample format {:?} is not supported", supported.sample_format()))?;
    let config = supported.config();
    let (rate, device_channels) = (config.sample_rate, u32::from(config.channels));
    let channels = if mono { 1 } else { device_channels };
    let keep = (rate as usize * channels as usize * 2 * KEEP.as_millis() as usize / 1000).max(CHUNK_FRAMES * channels as usize * 2);

    let (data_shared, error_shared) = (Arc::clone(shared), Arc::clone(shared));
    let frame = channels as usize * 2;
    let stream = device
        .build_input_stream_raw(
            &config,
            supported.sample_format(),
            move |data, _| {
                let mut pcm = format.to_pcm16(data.bytes());
                if mono && device_channels > 1 {
                    pcm = mix_down(&pcm, device_channels as usize);
                }
                data_shared.push(pcm, keep, frame);
            },
            move |error| error_shared.fail(error.to_string()),
            None,
        )
        .map_err(|e| format!("{name}: {e}"))?;
    stream.play().map_err(|e| format!("{name}: {e}"))?;
    Ok((stream, rate, channels))
}

/// One channel from several: the average of each frame.
pub fn mix_down(pcm: &[u8], channels: usize) -> Vec<u8> {
    pcm.chunks_exact(channels * 2)
        .flat_map(|frame| {
            let sum: i32 = frame.chunks_exact(2).map(|sample| i32::from(i16::from_le_bytes([sample[0], sample[1]]))).sum();
            ((sum / channels as i32) as i16).to_le_bytes()
        })
        .collect()
}

impl energy::Source for RawSource {
    fn chunk(&self) -> usize {
        CHUNK_FRAMES
    }

    fn sample_rate(&self) -> u32 {
        self.rate
    }

    fn sample_width(&self) -> u32 {
        2
    }

    fn available(&mut self) -> bool {
        !self.shared.buffer().bytes.is_empty()
    }

    fn read(&mut self) -> io::Result<Vec<u8>> {
        let wanted = CHUNK_FRAMES * self.channels as usize * 2;
        let mut buffer = self.shared.buffer();
        loop {
            if self.shared.closed.load(Ordering::SeqCst) {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "the stream was closed"));
            }
            // Sticky: a broken stream stays broken for whoever reads next.
            if let Some(failure) = &buffer.failure {
                return Err(io::Error::other(failure.clone()));
            }
            if buffer.bytes.len() >= wanted {
                return Ok(buffer.bytes.drain(..wanted).collect());
            }
            buffer = self.shared.arrived.wait_timeout(buffer, Duration::from_millis(50)).unwrap_or_else(|p| p.into_inner()).0;
        }
    }
}

impl Drop for RawSource {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::SeqCst);
        if let Some((stop, thread)) = self.stream.take() {
            let _ = stop.send(());
            let _ = thread.join();
        }
    }
}

// ---- the Silero recorder's capture ------------------------------------------------------------------------

impl Capturing for Capture {
    fn stop(&mut self) {
        Capture::stop(self);
    }
}

struct WasapiCapture {
    source: Source,
    name: String,
}

impl CaptureFactory for WasapiCapture {
    fn start(&self, sink: PcmSink, on_failure: CaptureFailure) -> Result<Box<dyn Capturing>, String> {
        let capture = Capture::start(self.source, &self.name, sink, on_failure)?;
        Ok(Box::new(capture))
    }
}

// ---- the platform -------------------------------------------------------------------------------------------

/// This machine: WASAPI for the devices, Silero on the ONNX Runtime found next to the app.
pub struct WasapiPlatform {
    onnx: Option<PathBuf>,
}

impl WasapiPlatform {
    pub fn locate() -> Self {
        WasapiPlatform { onnx: OnnxRuntime::locate() }
    }

    pub fn with_onnx_runtime(library: PathBuf) -> Self {
        WasapiPlatform { onnx: Some(library) }
    }
}

fn source_of(kind: Kind) -> Source {
    match kind {
        Kind::Mic => Source::Microphone,
        Kind::Speaker => Source::Speaker,
    }
}

impl Platform for WasapiPlatform {
    fn devices(&self) -> DeviceList {
        list_devices().unwrap_or_default()
    }

    fn energy_recorder(&self, kind: Kind, device: &Device, params: EnergyParams) -> Result<Arc<dyn Recorder>, String> {
        let (source, closer) = RawSource::open(source_of(kind), &device.name, kind == Kind::Mic)?;
        let channels = source.channels();
        let unblock = Box::new(move || closer.close());
        Ok(Arc::new(EnergyRecorder::new(kind.as_str(), Box::new(source), channels, params, unblock)))
    }

    fn vad_recorder(&self, kind: Kind, device: &Device, config: VadConfig) -> Result<Arc<dyn Recorder>, String> {
        let library = self.onnx.as_ref().ok_or("the ONNX Runtime library was not found")?;
        let engine = SileroFrameProbability::with_library(library)?;
        let segmenter = VadSegmenter::with_ids(engine, config, SegmentIds::global());
        let capture = Arc::new(WasapiCapture { source: source_of(kind), name: device.name.clone() });
        Ok(Arc::new(VadRecorder::new(kind.as_str(), capture, segmenter)))
    }
}
