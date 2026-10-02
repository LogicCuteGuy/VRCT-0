//! Capturing a microphone or a speaker's loopback through WASAPI (cpal) as 16 kHz mono PCM16.
//!
//! cpal's WASAPI streams must stay on the thread that built them, so `Capture` runs one
//! thread per stream that owns it until `stop` (or drop). Each device buffer is converted
//! (`samples`), then normalised (`normalize`), and the result goes to the caller's sink on
//! cpal's audio thread: the sink should hand the bytes on, not work on them.

use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use super::devices::LOOPBACK_SUFFIX;
use super::normalize::Pcm16MonoNormalizer;
use super::samples::RawFormat;
use super::wasapi::name_of;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Microphone,
    /// What a playback device is playing (WASAPI loopback); the name carries `LOOPBACK_SUFFIX`.
    Speaker,
}

pub struct Capture {
    stop: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    /// Opens the device called `name` (as `DeviceList` shows it) and starts delivering audio.
    /// `on_error` hears stream failures after start, such as the device being unplugged;
    /// the capture is then finished and should be dropped.
    pub fn start(
        source: Source,
        name: &str,
        sink: impl FnMut(&[u8]) + Send + 'static,
        on_error: impl Fn(String) + Send + Sync + 'static,
    ) -> Result<Capture, String> {
        let name = name.to_string();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let thread = std::thread::Builder::new()
            .name("vrct-capture".into())
            .spawn(move || {
                let stream = match open(source, &name, sink, Arc::new(on_error)) {
                    Ok(stream) => {
                        let _ = ready_tx.send(Ok(()));
                        stream
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                // Returns when stop() is called or the Capture is dropped.
                let _ = stop_rx.recv();
                drop(stream);
            })
            .map_err(|e| format!("cannot start the capture thread: {e}"))?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Capture { stop: Some(stop_tx), thread: Some(thread) }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(_) => Err("the capture thread ended before it was ready".to_string()),
        }
    }

    pub fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop();
    }
}

fn open(
    source: Source,
    name: &str,
    mut sink: impl FnMut(&[u8]) + Send + 'static,
    on_error: Arc<dyn Fn(String) + Send + Sync>,
) -> Result<cpal::Stream, String> {
    let host = cpal::default_host();
    let (device, supported) = match source {
        Source::Microphone => {
            let device = host
                .input_devices()
                .map_err(|e| format!("cannot list microphones: {e}"))?
                .find(|device| name_of(device).as_deref() == Some(name))
                .ok_or_else(|| format!("no microphone named {name:?}"))?;
            let supported = device.default_input_config().map_err(|e| format!("{name}: {e}"))?;
            (device, supported)
        }
        Source::Speaker => {
            let playback = name.strip_suffix(LOOPBACK_SUFFIX).unwrap_or(name);
            let device = host
                .output_devices()
                .map_err(|e| format!("cannot list playback devices: {e}"))?
                .find(|device| name_of(device).as_deref() == Some(playback))
                .ok_or_else(|| format!("no playback device named {playback:?}"))?;
            let supported = device.default_output_config().map_err(|e| format!("{name}: {e}"))?;
            (device, supported)
        }
    };

    let format = raw_format(supported.sample_format())
        .ok_or_else(|| format!("{name}: sample format {:?} is not supported", supported.sample_format()))?;
    let config = supported.config();
    let mut normalizer = Pcm16MonoNormalizer::new(config.sample_rate, 2, config.channels as usize);

    let data_errors = on_error.clone();
    // Building an input stream on a render device is what turns on loopback.
    let stream = device
        .build_input_stream_raw(
            &config,
            supported.sample_format(),
            move |data, _| match normalizer.process(&format.to_pcm16(data.bytes())) {
                Ok(pcm) if !pcm.is_empty() => sink(&pcm),
                Ok(_) => {}
                Err(error) => data_errors(error),
            },
            move |error| on_error(error.to_string()),
            None,
        )
        .map_err(|e| format!("{name}: {e}"))?;
    stream.play().map_err(|e| format!("{name}: {e}"))?;
    Ok(stream)
}

fn raw_format(format: cpal::SampleFormat) -> Option<RawFormat> {
    use cpal::SampleFormat as F;
    Some(match format {
        F::U8 => RawFormat::U8,
        F::I8 => RawFormat::I8,
        F::I16 => RawFormat::I16,
        F::U16 => RawFormat::U16,
        F::I24 => RawFormat::I24,
        F::I32 => RawFormat::I32,
        F::F32 => RawFormat::F32,
        F::F64 => RawFormat::F64,
        _ => return None,
    })
}
