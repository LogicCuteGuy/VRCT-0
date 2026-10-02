//! Translation engines. Python's `Translator` picks the engine and resolves
//! the language codes; the LLM-backed engines (OpenAI, Gemini, Ollama, ...)
//! are performed here, reached through the `translate.llm` RPC (see `rpc`);
//! so is DeepL (`translate.deepl`, `translate.deepl.check`), and the LLM key
//! checks and model lists (`llm.auth_check`, `llm.models`).

pub mod catalog;
pub mod deepl;
mod http;
pub mod llm;
pub mod prompt;
