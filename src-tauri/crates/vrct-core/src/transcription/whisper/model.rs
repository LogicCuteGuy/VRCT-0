//! A Whisper model run by CTranslate2: the part of `faster_whisper.WhisperModel.transcribe` that
//! VRCT uses (beam search 5 at temperature 0, no timestamps, `transcribe`, previous text as the
//! prompt of the next 30 s window), with the same prompt, token suppression and bookkeeping.
//!
//! With one temperature, faster-whisper's fall-back loop ends with the one result it has, so none
//! is run here. Word timestamps, VAD filtering, hotwords and the other options it offers are not
//! used by VRCT and not ported. CPU only.

use std::path::Path;

use ct2rs::sys::{ComputeType, Config, Device, StorageView, Whisper, WhisperOptions};
use tokenizers::Tokenizer;

use super::features::{FeatureExtractor, LogMel, NB_MAX_FRAMES};
use super::provider::{Info, Options, Segment, Transcribe};

/// The `Tokenizer`'s language codes; a multilingual model refuses any other.
pub const LANGUAGE_CODES: [&str; 100] = [
    "af", "am", "ar", "as", "az", "ba", "be", "bg", "bn", "bo", "br", "bs", "ca", "cs", "cy", "da", "de", "el", "en",
    "es", "et", "eu", "fa", "fi", "fo", "fr", "gl", "gu", "ha", "haw", "he", "hi", "hr", "ht", "hu", "hy", "id", "is",
    "it", "ja", "jw", "ka", "kk", "km", "kn", "ko", "la", "lb", "ln", "lo", "lt", "lv", "mg", "mi", "mk", "ml", "mn",
    "mr", "ms", "mt", "my", "ne", "nl", "nn", "no", "oc", "pa", "pl", "ps", "pt", "ro", "ru", "sa", "sd", "si", "sk",
    "sl", "sn", "so", "sq", "sr", "su", "sv", "sw", "ta", "te", "tg", "th", "tk", "tl", "tr", "tt", "uk", "ur", "uz", "vi",
    "yi", "yo", "zh", "yue",
];

const MAX_LENGTH: usize = 448;
/// `max_initial_timestamp / time_precision`: 1.0 s in steps of 0.02 s.
const MAX_INITIAL_TIMESTAMP_INDEX: usize = 50;
const BEAM_SIZE: usize = 5;

/// The special tokens the prompt and the suppression list are made of.
#[derive(Debug, Clone, Copy)]
struct Ids {
    sot: u32,
    eot: u32,
    transcribe: u32,
    translate: u32,
    sot_prev: u32,
    sot_lm: u32,
    no_speech: u32,
    no_timestamps: u32,
}

/// faster-whisper's decoding loop on a CTranslate2 model.
///
/// A thread that has called into CTranslate2 must not simply return: its thread-local ruy thread pool
/// is joined from Windows' loader-locked thread-exit callback while the pool's own threads wait for that
/// lock, and the thread never finishes. Keep such a thread parked for the life of the process (the
/// process exit then ends it), or exit the process from it as the tests do.
pub struct WhisperModel {
    whisper: Whisper,
    tokenizer: Tokenizer,
    extractor: FeatureExtractor,
    multilingual: bool,
    ids: Ids,
    suppress: Vec<i32>,
}

fn compute_type(name: &str) -> Result<ComputeType, String> {
    Ok(match name {
        "auto" | "int8" => ComputeType::INT8,
        "default" => ComputeType::DEFAULT,
        "float32" => ComputeType::FLOAT32,
        "int8_float32" => ComputeType::INT8_FLOAT32,
        other => return Err(format!("compute type {other:?} is not supported by this build (CPU only)")),
    })
}

impl WhisperModel {
    /// Loads the model in `dir` (`model.bin`, `config.json`, `tokenizer.json`, the vocabulary).
    /// `compute_type` `auto` is `int8`, which is what VRCT's choice of the best CPU type came to.
    pub fn load(dir: &Path, device: &str, device_index: i32, compute_type_name: &str, cpu_threads: usize) -> Result<Self, String> {
        if device != "cpu" {
            return Err(format!("device {device:?} is not supported by this build (CPU only)"));
        }
        let config = Config {
            device: Device::CPU,
            compute_type: compute_type(compute_type_name)?,
            device_indices: vec![device_index],
            num_threads_per_replica: cpu_threads,
            ..Config::default()
        };
        let whisper = Whisper::new(dir, config).map_err(|e| format!("cannot load the model: {e}"))?;
        let tokenizer = Tokenizer::from_file(dir.join("tokenizer.json")).map_err(|e| format!("cannot read tokenizer.json: {e}"))?;

        let id = |token: &str| tokenizer.token_to_id(token).ok_or_else(|| format!("the tokenizer has no {token}"));
        let ids = Ids {
            sot: id("<|startoftranscript|>")?,
            eot: id("<|endoftext|>")?,
            transcribe: id("<|transcribe|>")?,
            translate: id("<|translate|>")?,
            sot_prev: id("<|startofprev|>")?,
            sot_lm: id("<|startoflm|>")?,
            no_speech: tokenizer.token_to_id("<|nospeech|>").or_else(|| tokenizer.token_to_id("<|nocaptions|>")).ok_or("the tokenizer has no <|nospeech|>")?,
            no_timestamps: id("<|notimestamps|>")?,
        };
        let multilingual = whisper.is_multilingual();
        let extractor = FeatureExtractor::new(whisper.n_mels());
        let mut model = WhisperModel { whisper, tokenizer, extractor, multilingual, ids, suppress: Vec::new() };
        model.suppress = model.suppressed_tokens();
        Ok(model)
    }

    pub fn is_multilingual(&self) -> bool {
        self.multilingual
    }

    /// The token ids that are never produced: the symbols and annotations of
    /// `Tokenizer.non_speech_tokens`, and the special tokens that only belong in a prompt. Sorted.
    pub fn suppressed_tokens(&self) -> Vec<i32> {
        let mut tokens: Vec<i32> = self.non_speech_tokens().into_iter().map(|t| t as i32).collect();
        let ids = &self.ids;
        tokens.extend([ids.transcribe, ids.translate, ids.sot, ids.sot_prev, ids.sot_lm, ids.no_speech].map(|t| t as i32));
        tokens.sort_unstable();
        tokens.dedup();
        tokens
    }

    fn encode_text(&self, text: &str) -> Vec<u32> {
        self.tokenizer.encode(text, false).map(|e| e.get_ids().to_vec()).unwrap_or_default()
    }

    /// `Tokenizer.non_speech_tokens`: symbols that would start speaker tags, sound descriptions and
    /// the like, without the punctuation that ordinary text needs.
    fn non_speech_tokens(&self) -> Vec<u32> {
        let mut symbols: Vec<String> = "\"#()*+/:;<=>@[\\]^_`{|}~「」『』".chars().map(String::from).collect();
        symbols.extend(
            "<< >> <<< >>> -- --- -( -[ (' (\" (( )) ((( ))) [[ ]] {{ }} ♪♪ ♪♪♪".split_whitespace().map(str::to_string),
        );
        let miscellaneous: Vec<String> = "♩♪♫♬♭♮♯".chars().map(String::from).collect();

        let mut result: Vec<u32> = Vec::new();
        for text in [" -", " '"] {
            if let Some(first) = self.encode_text(text).first() {
                result.push(*first);
            }
        }
        let is_misc = |symbol: &String| miscellaneous.contains(symbol);
        for symbol in symbols.iter().chain(miscellaneous.iter()) {
            for tokens in [self.encode_text(symbol), self.encode_text(&format!(" {symbol}"))] {
                if let Some(first) = tokens.first() {
                    if tokens.len() == 1 || is_misc(symbol) {
                        result.push(*first);
                    }
                }
            }
        }
        result.sort_unstable();
        result.dedup();
        result
    }

    fn token_string(&self, id: u32) -> Result<String, String> {
        self.tokenizer.id_to_token(id).ok_or_else(|| format!("token {id} is not in the vocabulary"))
    }

    /// The tokenizer's text for `tokens` (special tokens at or above `<|endoftext|>` are dropped first).
    fn decode(&self, tokens: &[usize]) -> Result<String, String> {
        let text_tokens: Vec<u32> = tokens.iter().map(|t| *t as u32).filter(|t| *t < self.ids.eot).collect();
        self.tokenizer.decode(&text_tokens, true).map_err(|e| format!("cannot decode tokens: {e}"))
    }

    /// The language code and its probability for the first window of the audio.
    fn detect_language(&self, mel: &LogMel) -> Result<(String, f64), String> {
        let window = mel.slice(0, NB_MAX_FRAMES).pad_or_trim(NB_MAX_FRAMES);
        let encoded = self.encode(&window)?;
        let results = self.whisper.detect_language(&encoded).map_err(|e| e.to_string())?;
        let best = results.first().and_then(|r| r.first()).ok_or("language detection returned nothing")?;
        // "<|en|>" to "en"
        let code = best.language.strip_prefix("<|").and_then(|c| c.strip_suffix("|>")).unwrap_or(&best.language);
        Ok((code.to_string(), f64::from(best.probability)))
    }

    /// Runs the encoder on one window of exactly `NB_MAX_FRAMES` frames.
    fn encode(&self, window: &LogMel) -> Result<StorageView<'static>, String> {
        debug_assert_eq!(window.frames, NB_MAX_FRAMES);
        let mut data = window.data.clone();
        let features = StorageView::new(&[1, window.n_mels, window.frames], &mut data, Device::CPU).map_err(|e| e.to_string())?;
        self.whisper.encode(&features, false).map_err(|e| format!("the encoder failed: {e}"))
    }

    /// `get_prompt` with `without_timestamps`: previous text, then start, language, task, no timestamps.
    fn prompt_ids(&self, previous: &[usize], language: Option<u32>) -> Vec<u32> {
        let mut ids: Vec<u32> = Vec::new();
        if !previous.is_empty() {
            ids.push(self.ids.sot_prev);
            let keep = MAX_LENGTH / 2 - 1;
            ids.extend(previous[previous.len().saturating_sub(keep)..].iter().map(|t| *t as u32));
        }
        ids.push(self.ids.sot);
        if self.multilingual {
            if let Some(language) = language {
                ids.push(language);
            }
            ids.push(self.ids.transcribe);
        }
        ids.push(self.ids.no_timestamps);
        ids
    }

    /// The prompt for `language` (a code such as `en`) after `previous` tokens, as token ids.
    pub fn prompt_for(&self, previous: &[usize], language: &str) -> Result<Vec<u32>, String> {
        let token = self.tokenizer.token_to_id(&format!("<|{language}|>")).ok_or_else(|| format!("the tokenizer has no token for {language:?}"))?;
        Ok(self.prompt_ids(previous, Some(token)))
    }

    fn prompt(&self, previous: &[usize], language: Option<u32>) -> Result<Vec<String>, String> {
        self.prompt_ids(previous, language).into_iter().map(|id| self.token_string(id)).collect()
    }
}

impl Transcribe for WhisperModel {
    fn transcribe(&self, samples: &[f32], language: Option<&str>, options: &Options) -> Result<(Vec<Segment>, Info), String> {
        let mel = self.extractor.compute(samples);

        let (language, language_probability) = match language {
            None if !self.multilingual => ("en".to_string(), 1.0),
            None => self.detect_language(&mel)?,
            Some(code) => (if self.multilingual { code.to_string() } else { "en".to_string() }, 1.0),
        };
        let language_token = if self.multilingual {
            if !LANGUAGE_CODES.contains(&language.as_str()) {
                return Err(format!("{language:?} is not a valid language code"));
            }
            Some(self.tokenizer.token_to_id(&format!("<|{language}|>")).ok_or_else(|| format!("the tokenizer has no token for {language:?}"))?)
        } else {
            None
        };

        let generate_options = WhisperOptions {
            beam_size: BEAM_SIZE,
            patience: 1.0,
            length_penalty: 1.0,
            repetition_penalty: 1.0,
            no_repeat_ngram_size: options.no_repeat_ngram_size as usize,
            max_length: MAX_LENGTH,
            return_scores: true,
            return_no_speech_prob: true,
            max_initial_timestamp_index: MAX_INITIAL_TIMESTAMP_INDEX,
            suppress_blank: true,
            suppress_tokens: self.suppress.clone(),
            ..WhisperOptions::default()
        };

        // The last frame is the one the padding added; the windows cover the audio before it.
        let content_frames = mel.frames.saturating_sub(1);
        let mut segments = Vec::new();
        let mut all_tokens: Vec<usize> = Vec::new();
        let mut seek = 0usize;
        while seek < content_frames {
            let segment_size = NB_MAX_FRAMES.min(content_frames - seek);
            let window = mel.slice(seek, segment_size).pad_or_trim(NB_MAX_FRAMES);
            let encoded = self.encode(&window)?;

            let prompt = self.prompt(&all_tokens, language_token)?;
            let result = self
                .whisper
                .generate(&encoded, &[prompt], &generate_options)
                .map_err(|e| e.to_string())?
                .into_iter()
                .next()
                .ok_or("the decoder returned nothing")?;
            let tokens = result.sequences_ids.first().cloned().unwrap_or_default();
            let seq_len = tokens.len() as f64;
            // Recover the average log probability from the beam's score: score = sum / seq_len^length_penalty.
            let cumulative = f64::from(result.scores.first().copied().unwrap_or(0.0)) * seq_len;
            let avg_logprob = cumulative / (seq_len + 1.0);
            let no_speech_prob = f64::from(result.no_speech_prob);

            // No speech, and not even a confident guess: skip the window.
            let confident = avg_logprob > options.avg_logprob;
            if no_speech_prob > options.no_speech_prob && !confident {
                seek += segment_size;
                continue;
            }

            let text = self.decode(&tokens)?;
            seek += segment_size;
            if text.trim().is_empty() {
                continue;
            }
            all_tokens.extend_from_slice(&tokens);
            segments.push(Segment { text, avg_logprob, no_speech_prob });
        }

        Ok((segments, Info { language, language_probability }))
    }
}
