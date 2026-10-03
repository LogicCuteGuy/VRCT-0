//! The M2M100 tokenizer, as `transformers.M2M100Tokenizer` behaves for the
//! steps `Translator.translateCTranslate2` takes.
//!
//! CTranslate2 works on token strings, so ids never appear here. What matters is
//! which strings go in and how the strings coming out become text:
//!
//! * source: `[__<src>__] + pieces + [</s>]`, where every SentencePiece piece the
//!   model's `vocab.json` lacks becomes `<unk>` (Python goes piece -> id -> piece);
//! * target prefix: `__<target>__`;
//! * output: the pieces after the prefix are joined by SentencePiece's own
//!   decoder, with `<s> </s> <unk> <pad>` kept as written, then whitespace-trimmed
//!   and run through `clean_up_tokenization` when the tokenizer config asks for it
//!   (it does by default in the `transformers` pinned by VRCT).
//!
//! Checked against `transformers` 5.5.4 (`tests/fixtures/regenerate_ct2_golden.py`);
//! VRCT pins 4.40.2. Text that itself contains a special token such as `</s>`
//! is split into that token by Python and tokenised as plain characters here.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use sentencepiece::SentencePieceProcessor;
use serde_json::Value;

/// Language codes the M2M100 models know (`FAIRSEQ_LANGUAGE_CODES["m2m100"]`).
const LANGUAGES: [&str; 100] = [
    "af", "am", "ar", "ast", "az", "ba", "be", "bg", "bn", "br", "bs", "ca", "ceb", "cs", "cy",
    "da", "de", "el", "en", "es", "et", "fa", "ff", "fi", "fr", "fy", "ga", "gd", "gl", "gu", "ha",
    "he", "hi", "hr", "ht", "hu", "hy", "id", "ig", "ilo", "is", "it", "ja", "jv", "ka", "kk",
    "km", "kn", "ko", "lb", "lg", "ln", "lo", "lt", "lv", "mg", "mk", "ml", "mn", "mr", "ms", "my",
    "ne", "nl", "no", "ns", "oc", "or", "pa", "pl", "ps", "pt", "ro", "ru", "sd", "si", "sk", "sl",
    "so", "sq", "sr", "ss", "su", "sv", "sw", "ta", "th", "tl", "tn", "tr", "uk", "ur", "uz", "vi",
    "wo", "xh", "yi", "yo", "zh", "zu",
];

const SPM_FILE: &str = "sentencepiece.bpe.model";
const VOCAB_FILE: &str = "vocab.json";
const CONFIG_FILE: &str = "tokenizer_config.json";
const UNK: &str = "<unk>";
const EOS: &str = "</s>";
const SPECIAL: [&str; 4] = ["<s>", EOS, UNK, "<pad>"];

pub struct Tokenizer {
    pieces: SentencePieceProcessor,
    vocabulary: HashSet<String>,
    clean_up: bool,
}

impl Tokenizer {
    /// Load from a directory holding `sentencepiece.bpe.model` and `vocab.json`.
    pub fn open(dir: &Path) -> Result<Self, String> {
        let pieces = SentencePieceProcessor::open(dir.join(SPM_FILE))
            .map_err(|e| format!("cannot load {SPM_FILE}: {e}"))?;
        let vocabulary = fs::read_to_string(dir.join(VOCAB_FILE))
            .map_err(|e| format!("cannot read {VOCAB_FILE}: {e}"))?;
        let vocabulary: Value =
            serde_json::from_str(&vocabulary).map_err(|e| format!("bad {VOCAB_FILE}: {e}"))?;
        let vocabulary = vocabulary
            .as_object()
            .ok_or_else(|| format!("{VOCAB_FILE} is not an object"))?
            .keys()
            .cloned()
            .collect();
        // No config file, or no such key: transformers 4.x cleans up by default.
        let clean_up = fs::read_to_string(dir.join(CONFIG_FILE))
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|config| {
                config
                    .get("clean_up_tokenization_spaces")
                    .and_then(Value::as_bool)
            })
            .unwrap_or(true);
        Ok(Self {
            pieces,
            vocabulary,
            clean_up,
        })
    }

    /// Where `AutoTokenizer.from_pretrained(repo, cache_dir=dir)` left the files:
    /// `dir` itself, or `dir/models--<org>--<name>/snapshots/<revision>`.
    pub fn find(dir: &Path) -> Option<PathBuf> {
        super::find_files(dir, &[SPM_FILE, VOCAB_FILE])
    }

    pub fn languages() -> &'static [&'static str] {
        &LANGUAGES
    }

    pub fn knows(language: &str) -> bool {
        LANGUAGES.contains(&language)
    }

    fn language_token(language: &str) -> Result<String, String> {
        if Self::knows(language) {
            Ok(format!("__{language}__"))
        } else {
            Err(format!("unknown language code {language:?}"))
        }
    }

    /// `convert_ids_to_tokens(encode(text))` with `src_lang = source`.
    pub fn source_tokens(&self, text: &str, source: &str) -> Result<Vec<String>, String> {
        let mut tokens = vec![Self::language_token(source)?];
        let pieces = self
            .pieces
            .encode(text)
            .map_err(|e| format!("cannot tokenise: {e}"))?;
        tokens.extend(pieces.into_iter().map(|piece| {
            if self.vocabulary.contains(&piece.piece) {
                piece.piece
            } else {
                UNK.to_string()
            }
        }));
        tokens.push(EOS.to_string());
        Ok(tokens)
    }

    /// `lang_code_to_token[target]`.
    pub fn target_prefix(&self, target: &str) -> Result<String, String> {
        Self::language_token(target)
    }

    /// `decode(convert_tokens_to_ids(tokens))`.
    pub fn decode(&self, tokens: &[String]) -> Result<String, String> {
        let known = |token: &String| self.vocabulary.contains(token) || is_language_token(token);
        let mut text = String::new();
        let mut run: Vec<&str> = Vec::new();
        let flush = |text: &mut String, run: &mut Vec<&str>| -> Result<(), String> {
            if !run.is_empty() {
                text.push_str(
                    &self
                        .pieces
                        .decode_pieces(run)
                        .map_err(|e| format!("cannot decode: {e}"))?,
                );
                run.clear();
            }
            Ok(())
        };
        for token in tokens {
            // A token the vocabulary lacks is the unknown token once it has been through an id.
            let token = if known(token) { token.as_str() } else { UNK };
            if SPECIAL.contains(&token) {
                flush(&mut text, &mut run)?;
                text.push_str(token);
            } else {
                run.push(token);
            }
        }
        flush(&mut text, &mut run)?;
        let text = text.trim();
        Ok(if self.clean_up {
            clean_up_tokenization(text)
        } else {
            text.to_string()
        })
    }
}

fn is_language_token(token: &str) -> bool {
    token
        .strip_prefix("__")
        .and_then(|rest| rest.strip_suffix("__"))
        .is_some_and(Tokenizer::knows)
}

/// `PreTrainedTokenizerBase.clean_up_tokenization`.
pub(super) fn clean_up_tokenization(text: &str) -> String {
    text.replace(" .", ".")
        .replace(" ?", "?")
        .replace(" !", "!")
        .replace(" ,", ",")
        .replace(" ' ", "'")
        .replace(" n't", "n't")
        .replace(" 'm", "'m")
        .replace(" 's", "'s")
        .replace(" 've", "'ve")
        .replace(" 're", "'re")
}
