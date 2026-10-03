//! Translation engines. Python's `Translator` picks the engine and resolves
//! the language codes; the LLM-backed engines (OpenAI, Gemini, Ollama, ...)
//! are performed here, reached through the `translate.llm` RPC (see `rpc`);
//! so is DeepL (`translate.deepl`, `translate.deepl.check`), and the LLM key
//! checks and model lists (`llm.auth_check`, `llm.models`). `text` is the whole
//! of one translation, language names included (`translate.text`). With the
//! `ct2` feature, `ct2` loads and runs the local CTranslate2 models
//! (`ct2.load`, `ct2.translate`). `flow` is what the model does around the engines: target languages
//! side by side, and the local model when an engine fails.

pub mod catalog;
#[cfg(feature = "ct2")]
pub mod ct2;
pub mod deepl;
pub mod flow;
pub(crate) mod http;
pub mod languages;
pub mod llm;
pub mod native;
pub mod web;
pub mod prompt;
pub mod text;
