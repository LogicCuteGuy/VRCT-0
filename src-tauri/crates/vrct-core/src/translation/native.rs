//! The engines, in this process: Python's `Translator.translate` for the engines Rust runs.
//!
//! [`NativeTranslator`] is what the flow (`translation::flow`) talks to. It keeps what Python's `Translator`
//! kept: the DeepL key, the authenticated client of each LLM engine (key, address, model, and the last
//! conversation history it was given), and the local model. A call does what `translate` does:
//!
//! * the same language on both sides is the message itself;
//! * a language the engine has no code for is "unsupported" (not a failure), as is an engine without a table;
//! * an engine nobody authenticated, or a call that fails, is a failure;
//! * otherwise the engine is asked with the codes it knows.
//!
//! The network and the model are behind [`Remote`] and [`LocalModel`], so all of this runs in tests without
//! either. Google, Bing and Papago (the `translators` web library in Python) are not ported yet: they fail,
//! which sends the flow to the local model.
//!
//! `tests/adapter.rs` replays scenarios recorded from the real `Translator`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::Value;
use tokio::runtime::Handle;

use super::flow::{Reply, Request, Translator};
use super::languages::language_codes;
use super::llm;

/// The engines that talk to a service. `translate.text` asks the same question of both kinds.
pub trait Remote: Send + Sync {
    /// DeepL, with `source` and `target` already DeepL codes.
    fn deepl(&self, auth_key: &str, text: &str, source: &str, target: &str) -> Result<String, String>;
    /// An LLM engine; the request carries the codes in `input_lang` and `output_lang`.
    fn llm(&self, request: llm::Request) -> Result<String, String>;
    fn web(&self, _request: super::web::Request) -> Result<String, String> {
        Err("web translation transport is not configured".into())
    }
}

/// The real thing: HTTP on a tokio runtime. Calls block the calling thread, so they must not be made from
/// inside the runtime (the pipeline's worker threads are fine).
pub struct HttpRemote {
    runtime: Handle,
}

impl HttpRemote {
    pub fn new(runtime: Handle) -> Self {
        Self { runtime }
    }
}

impl Remote for HttpRemote {
    fn deepl(&self, auth_key: &str, text: &str, source: &str, target: &str) -> Result<String, String> {
        self.runtime.block_on(super::deepl::translate(super::deepl::Request {
            auth_key: auth_key.to_string(),
            text: text.to_string(),
            source_lang: Some(source.to_string()),
            target_lang: target.to_string(),
            server_url: None,
        }))
    }

    fn llm(&self, request: llm::Request) -> Result<String, String> {
        self.runtime.block_on(llm::translate(request))
    }

    fn web(&self, request: super::web::Request) -> Result<String, String> {
        self.runtime.block_on(super::web::translate(request))
    }
}

/// The local CTranslate2 model.
pub trait LocalModel: Send + Sync {
    fn loaded(&self) -> bool;
    /// `translateCTranslate2`: `source` and `target` are the model's language codes.
    fn translate(&self, message: &str, source: &str, target: &str, weight_type: &str) -> Result<String, String>;
}

/// An authenticated LLM engine.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EngineClient {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub model: String,
}

struct Client {
    settings: EngineClient,
    /// What `setContextHistory` last stored: an empty history does not reset it.
    history: Vec<Value>,
}

type Log = Arc<dyn Fn(&str) + Send + Sync>;

pub struct NativeTranslator {
    remote: Arc<dyn Remote>,
    local: Option<Arc<dyn LocalModel>>,
    deepl_key: Mutex<Option<String>>,
    clients: Mutex<HashMap<String, Client>>,
    log: Option<Log>,
}

/// The engines `translators` served in Python.
const WEB_ENGINES: &[&str] = &["Google", "Bing", "Papago"];

impl NativeTranslator {
    pub fn new(remote: Arc<dyn Remote>, local: Option<Arc<dyn LocalModel>>) -> Self {
        Self { remote, local, deepl_key: Mutex::new(None), clients: Mutex::new(HashMap::new()), log: None }
    }

    /// Where failures are written (`errorLogging`).
    pub fn with_log(mut self, log: Log) -> Self {
        self.log = Some(log);
        self
    }

    fn note(&self, text: &str) {
        if let Some(log) = &self.log {
            log(text);
        }
    }

    fn clients(&self) -> MutexGuard<'_, HashMap<String, Client>> {
        self.clients.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The DeepL key that passed its check; none forgets it.
    pub fn set_deepl_key(&self, key: Option<String>) {
        *self.deepl_key.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = key;
    }

    /// Makes `engine` available (its key was accepted). A new client starts without history.
    pub fn set_client(&self, engine: &str, settings: EngineClient) {
        self.clients().insert(engine.to_string(), Client { settings, history: Vec::new() });
    }

    /// Forgets the engine's client (its key was refused).
    pub fn remove_client(&self, engine: &str) {
        self.clients().remove(engine);
    }

    /// Changes the model of an authenticated engine; false when there is none.
    pub fn set_model(&self, engine: &str, model: &str) -> bool {
        match self.clients().get_mut(engine) {
            Some(client) => {
                client.settings.model = model.to_string();
                true
            }
            None => false,
        }
    }

    fn failed(&self, engine: &str, error: &str) -> Reply {
        self.note(&format!("{engine} failed: {error}"));
        Reply::Failed
    }

    fn answer(&self, engine: &str, outcome: Result<String, String>) -> Reply {
        match outcome {
            Ok(text) => Reply::Text(text),
            Err(error) => self.failed(engine, &error),
        }
    }
}

impl Translator for NativeTranslator {
    fn translate(&self, request: &Request<'_>) -> Reply {
        if request.source_language == request.target_language {
            return Reply::Text(request.message.to_string());
        }
        let engine = request.engine;
        let Ok((source, target)) = language_codes(
            engine,
            request.weight_type,
            request.target_country.unwrap_or_default(),
            request.source_language.unwrap_or_default(),
            request.target_language.unwrap_or_default(),
        ) else {
            return Reply::Unsupported;
        };

        match engine {
            "DeepL_API" => {
                let key = self.deepl_key.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
                match key {
                    Some(key) => self.answer(engine, self.remote.deepl(&key, request.message, &source, &target)),
                    None => Reply::Failed,
                }
            }
            "CTranslate2" => match &self.local {
                Some(local) => self.answer(engine, local.translate(request.message, &source, &target, request.weight_type)),
                None => Reply::Failed,
            },
            name if llm_engine(name) => {
                let call = {
                    let mut clients = self.clients();
                    let Some(client) = clients.get_mut(name) else { return Reply::Failed };
                    if let Some(history) = request.history.filter(|history| !history.is_empty()) {
                        client.history = history.to_vec();
                    }
                    llm::Request {
                        engine: name.to_string(),
                        base_url: client.settings.base_url.clone(),
                        api_key: client.settings.api_key.clone(),
                        model: client.settings.model.clone(),
                        text: request.message.to_string(),
                        input_lang: source,
                        output_lang: target,
                        history: client.history.clone(),
                    }
                };
                self.answer(engine, self.remote.llm(call))
            }
            name if WEB_ENGINES.contains(&name) => self.answer(name, self.remote.web(super::web::Request {
                engine: name.into(), text: request.message.into(), source, target, base_url: None,
            })),
            // An engine with a language table and no branch: Python's `result` stayed "".
            _ => Reply::Text(String::new()),
        }
    }

    fn ctranslate2_loaded(&self) -> bool {
        self.local.as_ref().is_some_and(|local| local.loaded())
    }

    fn report_failure(&self) {
        self.note("the local translation model failed too; the original text is used");
    }
}

/// The LLM engines (DeepL is matched before this is asked).
fn llm_engine(engine: &str) -> bool {
    super::text::handles(engine)
}

/// The local model run by this process (`ct2` feature): the engine plus which weights it was asked to load.
#[cfg(feature = "ct2")]
pub struct Ct2Local {
    engine: Arc<super::ct2::Engine>,
    weight_type: Mutex<Option<String>>,
}

#[cfg(feature = "ct2")]
impl Ct2Local {
    pub fn new(engine: Arc<super::ct2::Engine>) -> Self {
        Self { engine, weight_type: Mutex::new(None) }
    }

    /// `changeCTranslate2Model`: loads the weights; a failure leaves nothing loaded.
    pub fn load(&self, request: &super::ct2::LoadRequest) -> Result<(), String> {
        let mut weight_type = self.weight_type.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        *weight_type = None;
        self.engine.load(request)?;
        *weight_type = Some(request.weight_type.clone());
        Ok(())
    }
}

#[cfg(feature = "ct2")]
impl LocalModel for Ct2Local {
    fn loaded(&self) -> bool {
        let weight_type = self.weight_type.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        weight_type.as_deref().is_some_and(|weight_type| self.engine.is_loaded(weight_type))
    }

    fn translate(&self, message: &str, source: &str, target: &str, weight_type: &str) -> Result<String, String> {
        self.engine.translate(&super::ct2::TranslateRequest {
            message: message.to_string(),
            source_language: source.to_string(),
            target_language: target.to_string(),
            weight_type: weight_type.to_string(),
            max_decoding_length: None,
        })
    }
}
