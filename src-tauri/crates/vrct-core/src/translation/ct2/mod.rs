//! Local translation with CTranslate2 (M2M100 models), run inside this process.
//!
//! Python still downloads and verifies the weights; Rust loads them from the same
//! place: `<path>/weights/ctranslate2/<weight type>/` for the model and, under
//! its `tokenizer/` folder, the files `AutoTokenizer.from_pretrained(..,
//! cache_dir=..)` left behind. A failed load leaves nothing loaded, and the
//! Python side then loads the model itself as before.
//!
//! Only the CPU build is supported, and only M2M100 for now; NLLB stays in
//! Python until it has its own tokenizer here.

pub mod m2m100;

use std::path::PathBuf;
use std::sync::Mutex;

use ct2rs::sys::{ComputeType, Config, Device, TranslationOptions, Translator};
use serde::Deserialize;

/// Weight types this build runs; the directory is named after the type.
const WEIGHT_TYPES: [&str; 2] = ["m2m100_418M-ct2-int8", "m2m100_1.2B-ct2-int8"];

#[derive(Debug, Clone, Deserialize)]
pub struct LoadRequest {
    /// VRCT's local data root (`config.PATH_LOCAL`).
    pub path: PathBuf,
    pub weight_type: String,
    #[serde(default = "default_device")]
    pub device: String,
    #[serde(default)]
    pub device_index: i32,
    #[serde(default = "default_compute_type")]
    pub compute_type: String,
}

fn default_device() -> String {
    "cpu".into()
}

fn default_compute_type() -> String {
    "auto".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct TranslateRequest {
    pub message: String,
    /// M2M100 language codes (`ja`, `en`, ...), already resolved from display names.
    pub source_language: String,
    pub target_language: String,
    pub weight_type: String,
    /// Library default (256) when absent; only tests set it.
    #[serde(default)]
    pub max_decoding_length: Option<usize>,
}

struct Loaded {
    weight_type: String,
    translator: Translator,
    tokenizer: m2m100::Tokenizer,
}

/// The one loaded model. Loading and translating take the same lock, like the
/// `RLock` in Python, so a translation never sees a half-switched model.
#[derive(Default)]
pub struct Engine {
    loaded: Mutex<Option<Loaded>>,
}

impl Engine {
    pub fn load(&self, request: &LoadRequest) -> Result<(), String> {
        let mut loaded = self.loaded.lock().map_err(|_| "model lock poisoned".to_string())?;
        *loaded = None;

        if !WEIGHT_TYPES.contains(&request.weight_type.as_str()) {
            return Err(format!("{} is not run by this build", request.weight_type));
        }
        let config = config(request)?;
        let directory = request.path.join("weights").join("ctranslate2").join(&request.weight_type);
        let tokenizer_dir = m2m100::Tokenizer::find(&directory.join("tokenizer"))
            .ok_or_else(|| format!("no tokenizer files under {}", directory.join("tokenizer").display()))?;
        let tokenizer = m2m100::Tokenizer::open(&tokenizer_dir)?;
        let translator = Translator::new(&directory, &config).map_err(|e| format!("cannot load the model: {e}"))?;
        *loaded = Some(Loaded { weight_type: request.weight_type.clone(), translator, tokenizer });
        Ok(())
    }

    /// `Translator.translateCTranslate2`, with a failure as an error instead of `False`.
    pub fn translate(&self, request: &TranslateRequest) -> Result<String, String> {
        let loaded = self.loaded.lock().map_err(|_| "model lock poisoned".to_string())?;
        let Some(loaded) = loaded.as_ref() else {
            return Err("no model is loaded".into());
        };
        if loaded.weight_type != request.weight_type {
            return Err(format!("{} is loaded, not {}", loaded.weight_type, request.weight_type));
        }

        let source = loaded.tokenizer.source_tokens(&request.message, &request.source_language)?;
        let prefix = loaded.tokenizer.target_prefix(&request.target_language)?;
        let mut options = TranslationOptions::default();
        if let Some(length) = request.max_decoding_length {
            options.max_decoding_length = length;
        }
        let results = loaded
            .translator
            .translate_batch_with_target_prefix(&[source], &[vec![prefix]], &options, None)
            .map_err(|e| format!("translation failed: {e}"))?;
        let hypothesis = results
            .into_iter()
            .next()
            .and_then(|result| result.hypotheses.into_iter().next())
            .ok_or_else(|| "no hypothesis returned".to_string())?;
        // The first token is the target-language prefix itself.
        loaded.tokenizer.decode(hypothesis.get(1..).unwrap_or_default())
    }

    pub fn is_loaded(&self, weight_type: &str) -> bool {
        self.loaded.lock().is_ok_and(|loaded| loaded.as_ref().is_some_and(|model| model.weight_type == weight_type))
    }
}

fn config(request: &LoadRequest) -> Result<Config, String> {
    if request.device != "cpu" {
        return Err(format!("device {:?} is not supported by this build (CPU only)", request.device));
    }
    Ok(Config {
        device: Device::CPU,
        compute_type: compute_type(&request.compute_type)?,
        device_indices: vec![request.device_index],
        // Python used inter_threads=1, intra_threads=4.
        num_threads_per_replica: 4,
        ..Config::default()
    })
}

fn compute_type(name: &str) -> Result<ComputeType, String> {
    Ok(match name {
        "auto" => ComputeType::AUTO,
        "default" => ComputeType::DEFAULT,
        "float32" => ComputeType::FLOAT32,
        "int8" => ComputeType::INT8,
        "int8_float32" => ComputeType::INT8_FLOAT32,
        "int8_float16" => ComputeType::INT8_FLOAT16,
        "int8_bfloat16" => ComputeType::INT8_BFLOAT16,
        "int16" => ComputeType::INT16,
        "float16" => ComputeType::FLOAT16,
        "bfloat16" => ComputeType::BFLOAT16,
        other => return Err(format!("unknown compute type {other:?}")),
    })
}
