//! Translation engines. Python's `Translator` picks the engine and resolves
//! the language codes; the LLM-backed engines (OpenAI, Gemini, Ollama, ...)
//! are performed here, reached through the `translate.llm` RPC (see `rpc`);
//! so is DeepL (`translate.deepl`, `translate.deepl.check`).

pub mod deepl;
mod http;
pub mod llm;
pub mod prompt;
