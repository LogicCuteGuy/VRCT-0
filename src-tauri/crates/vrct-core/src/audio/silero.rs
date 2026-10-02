//! `SileroFrameProbability`: the speech probability of one 512-sample frame.
//!
//! The same Silero VAD v5 network the Python backend gets from faster-whisper
//! 1.1.1 (an encoder and a decoder as two ONNX files, in `assets/silero/`),
//! run through ONNX Runtime with one thread, as Python does. The model files
//! are compiled in; the ONNX Runtime library itself is loaded at run time
//! (`ort`'s `load-dynamic`), so the build downloads nothing. `OnnxRuntime::locate`
//! says where it looks.
//!
//! Per frame: the last 64 samples of the previous frame (zeros at the start) go
//! in front of the 512 new ones, the encoder turns the 576 samples into 128
//! features, the decoder (an LSTM with a `[2, 1, 128]` state) turns those into
//! the probability and the next state.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use ort::session::Session;
use ort::value::Tensor;

use super::vad::FrameProbability;
use super::FRAME_SAMPLES;

const ENCODER: &[u8] = include_bytes!("../../assets/silero/silero_encoder_v5.onnx");
const DECODER: &[u8] = include_bytes!("../../assets/silero/silero_decoder_v5.onnx");

const CONTEXT: usize = 64;
const FEATURES: usize = 128;
const STATE: usize = 2 * FEATURES;

const LIBRARY: &str = if cfg!(windows) {
    "onnxruntime.dll"
} else if cfg!(target_os = "macos") {
    "libonnxruntime.dylib"
} else {
    "libonnxruntime.so"
};

/// The ONNX Runtime shared library. `ort` can load only one per process, so the first
/// path that loads wins and later calls just report that result.
pub struct OnnxRuntime;

impl OnnxRuntime {
    /// `ORT_DYLIB_PATH` if set, else the library next to the executable.
    pub fn locate() -> Option<PathBuf> {
        if let Some(path) = std::env::var_os("ORT_DYLIB_PATH").map(PathBuf::from) {
            return path.is_file().then_some(path);
        }
        let beside = std::env::current_exe().ok()?.parent()?.join(LIBRARY);
        beside.is_file().then_some(beside)
    }

    pub fn load(path: &Path) -> Result<(), String> {
        static LOADED: OnceLock<Result<(), String>> = OnceLock::new();
        LOADED
            .get_or_init(|| {
                let builder = ort::init_from(path).map_err(|e| format!("cannot load {}: {e}", path.display()))?;
                if builder.commit() {
                    Ok(())
                } else {
                    Err("ONNX Runtime was already initialised".to_string())
                }
            })
            .clone()
    }
}

pub struct SileroFrameProbability {
    encoder: Session,
    decoder: Session,
    state: [f32; STATE],
    context: [f32; CONTEXT],
}

impl SileroFrameProbability {
    /// Loads the runtime from `OnnxRuntime::locate()` and builds both sessions.
    pub fn new() -> Result<Self, String> {
        let library = OnnxRuntime::locate().ok_or_else(|| {
            format!("{LIBRARY} not found: put it next to the executable or set ORT_DYLIB_PATH")
        })?;
        Self::with_library(&library)
    }

    pub fn with_library(library: &Path) -> Result<Self, String> {
        OnnxRuntime::load(library)?;
        let session = |bytes: &[u8]| -> Result<Session, ort::Error> {
            let mut builder = Session::builder()?;
            builder = builder.with_intra_threads(1)?;
            builder = builder.with_inter_threads(1)?;
            builder.commit_from_memory(bytes)
        };
        let session = |bytes: &[u8]| session(bytes).map_err(|e| format!("cannot build the Silero session: {e}"));
        Ok(Self {
            encoder: session(ENCODER)?,
            decoder: session(DECODER)?,
            state: [0.0; STATE],
            context: [0.0; CONTEXT],
        })
    }
}

impl FrameProbability for SileroFrameProbability {
    fn probability(&mut self, frame: &[f32]) -> Result<f32, String> {
        if frame.len() != FRAME_SAMPLES {
            return Err(format!("expected a frame of {FRAME_SAMPLES} samples, got {}", frame.len()));
        }
        let fail = |what: &str| {
            let what = what.to_string();
            move |e: ort::Error| format!("Silero {what}: {e}")
        };

        let mut input = Vec::with_capacity(CONTEXT + FRAME_SAMPLES);
        input.extend_from_slice(&self.context);
        input.extend_from_slice(frame);
        let input = Tensor::from_array(([1usize, CONTEXT + FRAME_SAMPLES], input)).map_err(fail("input"))?;
        let encoded = self.encoder.run(ort::inputs!["input" => input]).map_err(fail("encoder"))?;
        let (_, features) = encoded[0].try_extract_tensor::<f32>().map_err(fail("encoder output"))?;
        if features.len() != FEATURES {
            return Err(format!("Silero encoder gave {} values, expected {FEATURES}", features.len()));
        }

        let features = Tensor::from_array(([1usize, FEATURES], features.to_vec())).map_err(fail("features"))?;
        let state = Tensor::from_array(([2usize, 1, FEATURES], self.state.to_vec())).map_err(fail("state"))?;
        let decoded =
            self.decoder.run(ort::inputs!["input" => features, "state" => state]).map_err(fail("decoder"))?;
        let (_, probability) = decoded[0].try_extract_tensor::<f32>().map_err(fail("decoder output"))?;
        let (_, next_state) = decoded[1].try_extract_tensor::<f32>().map_err(fail("decoder state"))?;
        let probability = *probability.first().ok_or("Silero decoder gave no probability")?;
        if next_state.len() != STATE {
            return Err(format!("Silero decoder state has {} values, expected {STATE}", next_state.len()));
        }
        self.state.copy_from_slice(next_state);
        self.context.copy_from_slice(&frame[FRAME_SAMPLES - CONTEXT..]);
        Ok(probability)
    }

    fn reset(&mut self) {
        self.state.fill(0.0);
        self.context.fill(0.0);
    }
}
