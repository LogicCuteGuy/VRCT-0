//! Capturing STT audio sources through WASAPI/ASIO as 16 kHz mono PCM16.
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
    /// Receiving STT: a recording input or a playback device's loopback.
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
        Self::start_on_host(source, super::devices::WASAPI_HOST, name, sink, on_error)
    }

    pub fn start_on_host(
        source: Source, host: &str, name: &str,
        sink: impl FnMut(&[u8]) + Send + 'static,
        on_error: impl Fn(String) + Send + Sync + 'static,
    ) -> Result<Capture, String> {
        let host = host.to_owned();
        let name = name.to_string();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let thread = std::thread::Builder::new()
            .name("vrct-capture".into())
            .spawn(move || {
                let _apartment = match super::wasapi::AudioApartment::initialize(&host) {
                    Ok(apartment) => apartment,
                    Err(error) => { let _ = ready_tx.send(Err(error)); return; }
                };
                let stream = match open(source, &host, &name, sink, Arc::new(on_error)) {
                    Ok(stream) => {
                        let _ = ready_tx.send(Ok(()));
                        stream
                    }
                    Err(error) => {
                        if host == super::devices::ASIO_HOST { super::wasapi::release_asio_device(); }
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                // Returns when stop() is called or the Capture is dropped.
                let _ = stop_rx.recv();
                drop(stream);
                if host == super::devices::ASIO_HOST { super::wasapi::release_asio_device(); }
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

/// The device called `name` (as `DeviceList` shows it) and the format it delivers.
pub(super) fn find_device(source: Source, host_name: &str, name: &str) -> Result<(Arc<cpal::Device>, cpal::SupportedStreamConfig), String> {
    if host_name == super::devices::ASIO_HOST {
        let device = super::wasapi::asio_device(name)?;
        let supported = device.default_input_config().map_err(|e| format!("{name}: ASIO input unavailable: {e}"))?;
        return Ok((device, supported));
    }
    if host_name != super::devices::WASAPI_HOST && !host_name.is_empty() {
        return Err(format!("unsupported capture host {host_name:?}"));
    }
    let host = cpal::default_host();
    match source {
        Source::Microphone => {
            let device = host
                .input_devices()
                .map_err(|e| format!("cannot list microphones: {e}"))?
                .find(|device| name_of(device).as_deref() == Some(name))
                .ok_or_else(|| format!("no microphone named {name:?}"))?;
            let supported = device.default_input_config().map_err(|e| format!("{name}: {e}"))?;
            Ok((Arc::new(device), supported))
        }
        Source::Speaker => {
            if !name.ends_with(LOOPBACK_SUFFIX) {
                if let Some(device) = host.input_devices()
                    .map_err(|e| format!("cannot list recording inputs: {e}"))?
                    .find(|device| name_of(device).as_deref() == Some(name))
                {
                    let supported = device.default_input_config().map_err(|e| format!("{name}: {e}"))?;
                    return Ok((Arc::new(device), supported));
                }
            }
            let playback = name.strip_suffix(LOOPBACK_SUFFIX).unwrap_or(name);
            let device = host
                .output_devices()
                .map_err(|e| format!("cannot list playback devices: {e}"))?
                .find(|device| name_of(device).as_deref() == Some(playback))
                .ok_or_else(|| format!("no playback device named {playback:?}"))?;
            let supported = device.default_output_config().map_err(|e| format!("{name}: {e}"))?;
            Ok((Arc::new(device), supported))
        }
    }
}

fn open(
    source: Source,
    host: &str,
    name: &str,
    mut sink: impl FnMut(&[u8]) + Send + 'static,
    on_error: Arc<dyn Fn(String) + Send + Sync>,
) -> Result<cpal::Stream, String> {
    let (device, supported) = find_device(source, host, name)?;

    let format = raw_format(supported.sample_format())
        .ok_or_else(|| format!("{name}: sample format {:?} is not supported", supported.sample_format()))?;
    let config = supported.config();
    let mut normalizer = Pcm16MonoNormalizer::new(config.sample_rate, 2, config.channels as usize);

    let data_errors = on_error.clone();
    let driver_lease = device.clone();
    // Building an input stream on a render device is what turns on loopback.
    let stream = device
        .build_input_stream_raw(
            &config,
            supported.sample_format(),
            move |data, _| { let _keep_driver_alive = &driver_lease; match normalizer.process(&format.to_pcm16(data.bytes())) {
                Ok(pcm) if !pcm.is_empty() => sink(&pcm),
                Ok(_) => {}
                Err(error) => data_errors(error),
            } },
            move |error| on_error(error.to_string()),
            None,
        )
        .map_err(|e| format!("{name}: {e}"))?;
    stream.play().map_err(|e| format!("{name}: {e}"))?;
    Ok(stream)
}

pub(super) fn raw_format(format: cpal::SampleFormat) -> Option<RawFormat> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::devices::WASAPI_HOST;

    #[test]
    fn speaker_stt_opens_recording_inputs_and_playback_loopbacks() {
        let list = super::super::wasapi::list_devices().unwrap();
        for input in &list.mics {
            let (device, format) = find_device(Source::Speaker, WASAPI_HOST, &input.name).unwrap();
            let input_format = device.default_input_config().unwrap();
            assert_eq!(name_of(device.as_ref()).as_deref(), Some(input.name.as_str()));
            assert_eq!(format.channels(), input_format.channels());
            assert_eq!(format.sample_rate(), input_format.sample_rate());
            assert_eq!(format.sample_format(), input_format.sample_format());
        }
        for playback in list.speakers.iter().filter(|device| device.name.ends_with(LOOPBACK_SUFFIX)) {
            let (device, format) = find_device(Source::Speaker, WASAPI_HOST, &playback.name).unwrap();
            assert_eq!(name_of(device.as_ref()).as_deref(), playback.name.strip_suffix(LOOPBACK_SUFFIX));
            assert_eq!(format.sample_rate(), device.default_output_config().unwrap().sample_rate());
        }
    }

    #[test]
    #[ignore = "Briefly receives VBMatrix Out 1 STT input buffers; counts bytes only, without saving audio"]
    fn speaker_stt_receives_vb_matrix_recording_input() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::{Duration, Instant};
        let list = super::super::wasapi::list_devices().unwrap();
        let input = list.mics.iter().find(|input| input.name.starts_with("VBMatrix Out 1 (")).expect("VBMatrix Out 1 must be installed");
        let count = Arc::new(AtomicUsize::new(0));
        let received = count.clone();
        let errors = Arc::new(std::sync::Mutex::new(Vec::new()));
        let failures = errors.clone();
        let mut capture = Capture::start_on_host(Source::Speaker, WASAPI_HOST, &input.name,
            move |pcm| { received.fetch_add(pcm.len(), Ordering::Relaxed); },
            move |error| { failures.lock().unwrap().push(error); }).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while count.load(Ordering::Relaxed) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        capture.stop();
        assert!(errors.lock().unwrap().is_empty());
        assert!(count.load(Ordering::Relaxed) > 0, "recording endpoint must feed receiving STT");
        eprintln!("Speaker STT received PCM buffers from {}", input.name);
    }
}
