//! Production wiring for the native pipeline. Constructing it does not capture.
use std::sync::Arc;

use vrct_core::router::ResponseSink;
use vrct_core::runtime::Runtime;
use vrct_core::settings::Settings;
use vrct_core::sinks::Sinks;
use vrct_core::translation::native::{HttpRemote, LocalModel, NativeTranslator};

#[cfg(feature = "ct2")]
struct SharedLocal {
    settings: Arc<Settings>,
    engine: Arc<vrct_core::translation::ct2::Engine>,
}

#[cfg(feature = "ct2")]
impl LocalModel for SharedLocal {
    fn loaded(&self) -> bool {
        self.settings
            .get_str("CTRANSLATE2_WEIGHT_TYPE")
            .is_some_and(|weight| self.engine.is_loaded(&weight))
    }
    fn translate(
        &self,
        message: &str,
        source: &str,
        target: &str,
        weight_type: &str,
    ) -> Result<String, String> {
        self.engine
            .translate(&vrct_core::translation::ct2::TranslateRequest {
                message: message.to_string(),
                source_language: source.to_string(),
                target_language: target.to_string(),
                weight_type: weight_type.to_string(),
                max_decoding_length: None,
            })
    }
}

pub fn create(
    settings: Arc<Settings>,
    sinks: Arc<Sinks>,
    sink: Arc<dyn ResponseSink>,
    #[cfg(feature = "ct2")] engine: Arc<vrct_core::translation::ct2::Engine>,
) -> Arc<Runtime> {
    use vrct_core::transcription::native::{FileWhisper, NativeBackend};
    let handle = tauri::async_runtime::handle().inner().clone();
    #[cfg(windows)]
    let platform: Arc<dyn vrct_core::transcription::native::Platform> =
        Arc::new(vrct_core::audio::raw::WasapiPlatform::locate());
    #[cfg(not(windows))]
    let platform: Arc<dyn vrct_core::transcription::native::Platform> = Arc::new(UnavailableAudio);
    let backend = Arc::new(NativeBackend::new(
        settings.clone(),
        platform,
        Arc::new(FileWhisper),
        handle.clone(),
    ));
    #[cfg(feature = "ct2")]
    let local = Some(Arc::new(SharedLocal {
        settings: settings.clone(),
        engine,
    }) as Arc<dyn LocalModel>);
    #[cfg(not(feature = "ct2"))]
    let local: Option<Arc<dyn LocalModel>> = None;
    let translator = Arc::new(NativeTranslator::new(
        Arc::new(HttpRemote::new(handle)),
        local,
    ));
    Runtime::new(settings, backend, translator, sinks, sink)
}

#[cfg(not(windows))]
struct UnavailableAudio;
#[cfg(not(windows))]
impl vrct_core::transcription::native::Platform for UnavailableAudio {
    fn devices(&self) -> vrct_core::audio::devices::DeviceList {
        Default::default()
    }
    fn energy_recorder(
        &self,
        _: vrct_core::transcription::session::Kind,
        _: &vrct_core::audio::devices::Device,
        _: vrct_core::transcription::recorder::EnergyParams,
    ) -> Result<Arc<dyn vrct_core::transcription::recorder::Recorder>, String> {
        Err("Native audio capture requires Windows WASAPI".into())
    }
    fn vad_recorder(
        &self,
        _: vrct_core::transcription::session::Kind,
        _: &vrct_core::audio::devices::Device,
        _: vrct_core::audio::vad::VadConfig,
    ) -> Result<Arc<dyn vrct_core::transcription::recorder::Recorder>, String> {
        Err("Native audio capture requires Windows WASAPI".into())
    }
}
