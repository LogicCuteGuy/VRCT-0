//! The NLLB tokenizer, as `NllbTokenizerFast` behaves for the steps
//! `Translator.translateCTranslate2` takes.
//!
//! The Hugging Face NLLB repos ship a `tokenizer.json` that the `tokenizers`
//! library runs (normalizer, Metaspace pre-tokenizer, Unigram model, Metaspace
//! decoder). Python's fast tokenizer adds `[src_lang_code] ... [</s>]` through a
//! post-processor template; here the same two tokens are added by hand around
//! the plain encoding, which is the same list. Tokens the vocabulary lacks go
//! through an id back to `<unk>`, and the decoded text gets the same
//! `clean_up_tokenization` as the M2M100 side when the config asks for it
//! (default on, like transformers 4.x).
//!
//! The target prefix is the language code as given: Python does not check it.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::m2m100::clean_up_tokenization;

const TOKENIZER_FILE: &str = "tokenizer.json";
const CONFIG_FILE: &str = "tokenizer_config.json";
const UNK: &str = "<unk>";
const EOS: &str = "</s>";

pub struct Tokenizer {
    inner: tokenizers::Tokenizer,
    clean_up: bool,
}

impl Tokenizer {
    pub fn open(dir: &Path) -> Result<Self, String> {
        let inner = tokenizers::Tokenizer::from_file(dir.join(TOKENIZER_FILE))
            .map_err(|e| format!("cannot load {TOKENIZER_FILE}: {e}"))?;
        let clean_up = fs::read_to_string(dir.join(CONFIG_FILE))
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|config| {
                config
                    .get("clean_up_tokenization_spaces")
                    .and_then(Value::as_bool)
            })
            .unwrap_or(true);
        Ok(Self { inner, clean_up })
    }

    /// Same places as the M2M100 tokenizer: `dir`, or a Hugging Face cache snapshot under it.
    pub fn find(dir: &Path) -> Option<PathBuf> {
        super::find_files(dir, &[TOKENIZER_FILE])
    }

    /// `convert_ids_to_tokens(encode(text))` with `src_lang = source`.
    pub fn source_tokens(&self, text: &str, source: &str) -> Result<Vec<String>, String> {
        let encoding = self
            .inner
            .encode(text, false)
            .map_err(|e| format!("cannot tokenise: {e}"))?;
        let mut tokens = vec![source.to_string()];
        tokens.extend(encoding.get_tokens().iter().cloned());
        tokens.push(EOS.to_string());
        Ok(tokens)
    }

    pub fn target_prefix(&self, target: &str) -> Result<String, String> {
        Ok(target.to_string())
    }

    /// `decode(convert_tokens_to_ids(tokens))`.
    pub fn decode(&self, tokens: &[String]) -> Result<String, String> {
        let unk = self
            .inner
            .token_to_id(UNK)
            .ok_or_else(|| format!("the vocabulary has no {UNK}"))?;
        let ids: Vec<u32> = tokens
            .iter()
            .map(|token| self.inner.token_to_id(token).unwrap_or(unk))
            .collect();
        let text = self
            .inner
            .decode(&ids, false)
            .map_err(|e| format!("cannot decode: {e}"))?;
        Ok(if self.clean_up {
            clean_up_tokenization(&text)
        } else {
            text
        })
    }
}
