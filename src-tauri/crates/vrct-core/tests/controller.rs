//! Full native route/config assembly with no audio capture or cloud requests.
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use vrct_core::{
    audio::devices::Device,
    auth::{Auth, Catalog, Models},
    config::ConfigReplica,
    controller::{Controller, Effects},
    native::{NativeController, Services},
    protocol::Response,
    router::{ResponseSink, Router},
    runtime::Runtime,
    settings::{system::production_env, Devices, Settings},
    sinks::Sinks,
    transcription::{
        phrases::Format,
        recorder::Recorder,
        session::{Backend, Kind, Transcriber},
    },
    translation::native::{NativeTranslator, Remote},
};

#[tokio::test]
async fn rejected_custom_url_preserves_last_working_url_and_credential() {
    struct Reject;
    impl Catalog for Reject {
        fn fetch(
            &self,
            _: String,
            _: Option<String>,
            _: Option<String>,
        ) -> futures_util::future::BoxFuture<'static, Result<Models, String>> {
            Box::pin(async { Err("invalid server".into()) })
        }
    }
    let (controller, settings, _, _) = controller();
    let mut keys = settings.get("AUTH_KEYS").unwrap();
    keys["OpenAI_Compatible"] = json!("local-key");
    settings.set("AUTH_KEYS", keys).unwrap();
    let old = settings.get("OPENAI_COMPATIBLE_URL").unwrap();
    let auth = Auth::new(settings.clone(), controller, Arc::new(Reject));
    let reply = auth
        .change_url(
            "openai_compatible",
            "OpenAI_Compatible",
            "OPENAI_COMPATIBLE_URL",
            json!("http://127.0.0.1:1/v1"),
            old.clone(),
        )
        .await;
    assert_eq!(reply.0, 400);
    assert_eq!(
        reply.1["error_code"],
        "CONNECTION_OPENAI_COMPATIBLE_URL_INVALID"
    );
    assert_eq!(settings.get("OPENAI_COMPATIBLE_URL").unwrap(), old);
    assert_eq!(
        auth.getter("/get/data/openai_compatible_auth_key").unwrap(),
        "local-key"
    );
}
#[test]
fn changing_deepgram_model_refreshes_language_choices_and_active_slots() {
    let (controller, settings, _, _) = controller();
    settings
        .set(
            "SELECTABLE_DEEPGRAM_MODEL_LIST",
            json!(["english-model", "japanese-model"]),
        )
        .unwrap();
    settings
        .set(
            "DEEPGRAM_MODEL_LANGUAGES",
            json!({"english-model":["en"],"japanese-model":["ja"]}),
        )
        .unwrap();
    settings
        .set("SELECTED_DEEPGRAM_MODEL", json!("english-model"))
        .unwrap();
    let mut status = settings
        .get("SELECTABLE_TRANSCRIPTION_ENGINE_STATUS")
        .unwrap();
    status["Deepgram"] = json!(true);
    settings
        .set("SELECTABLE_TRANSCRIPTION_ENGINE_STATUS", status)
        .unwrap();
    assert_eq!(
        controller
            .answer("/set/data/selected_transcription_engine", json!("Deepgram"))
            .0,
        200
    );
    assert!(controller
        .languages()
        .as_array()
        .unwrap()
        .iter()
        .all(|v| v["language"] == "English"));
    assert_eq!(
        controller
            .answer("/set/data/selected_deepgram_model", json!("japanese-model"))
            .0,
        200
    );
    assert!(controller
        .languages()
        .as_array()
        .unwrap()
        .iter()
        .all(|v| v["language"] == "Japanese"));
    for (_, slots) in settings
        .get("SELECTED_YOUR_LANGUAGES")
        .unwrap()
        .as_object()
        .unwrap()
    {
        assert_eq!(slots["1"]["language"], "Japanese");
    }
}

#[derive(Default)]
struct Events(Mutex<Vec<Response>>);
impl ResponseSink for Events {
    fn emit(&self, r: Response) {
        self.0.lock().unwrap().push(r);
    }
}
struct DevicesFake;
impl Devices for DevicesFake {
    fn mic_hosts(&self) -> Vec<String> {
        vec!["Windows WASAPI".into(), "ASIO".into()]
    }
    fn mic_device_names(&self, host: &str) -> Vec<String> {
        if host == "ASIO" { vec!["VB-Matrix VASIO-8".into()] }
        else { vec!["Mic A".into(), "Mic B".into()] }
    }
    fn speaker_device_names(&self) -> Vec<String> {
        vec!["Speaker A [Loopback]".into()]
    }
    fn speaker_hosts(&self) -> Vec<String> { self.mic_hosts() }
    fn speaker_device_names_for_host(&self, host: &str) -> Vec<String> {
        match host {
            "ASIO" => vec!["VB-Matrix VASIO-8".into()],
            "Windows WASAPI" => self.speaker_device_names(),
            _ => Vec::new(),
        }
    }
    fn default_mic(&self) -> Option<(String, String)> {
        Some(("Windows WASAPI".into(), "Mic A".into()))
    }
    fn default_speaker(&self) -> Option<String> {
        Some("Speaker A [Loopback]".into())
    }
}
#[derive(Default)]
struct Effect(AtomicBool);
impl Effects for Effect {
    fn changed(&self, _: &str) -> Result<(), String> {
        if self.0.load(Ordering::SeqCst) {
            Err("occupied resource".into())
        } else {
            Ok(())
        }
    }
}
fn settings() -> Arc<Settings> {
    let dir = std::env::temp_dir().join(format!(
        "vrct-controller-{}-{}",
        std::process::id(),
        getrandom::u64().unwrap()
    ));
    let mut env = production_env("3.5.1-beta.1", dir);
    env.devices = Arc::new(DevicesFake);
    Arc::new(Settings::open(env).0)
}
fn controller() -> (Arc<Controller>, Arc<Settings>, Arc<Events>, Arc<Effect>) {
    let settings = settings();
    let events = Arc::new(Events::default());
    let effects = Arc::new(Effect::default());
    (
        Controller::new(
            settings.clone(),
            events.clone(),
            Arc::new(DevicesFake),
            effects.clone(),
        ),
        settings,
        events,
        effects,
    )
}
struct NoCapture;

#[test]
fn both_audio_hosts_select_devices_from_the_selected_host_and_reject_stale_names() {
    let (controller, settings, events, _) = controller();
    assert_eq!(controller.answer("/get/data/selectable_speaker_host_list", Value::Null), (200, json!(["Windows WASAPI", "ASIO"])));
    for kind in ["mic", "speaker"] {
        assert_eq!(controller.answer(&format!("/set/data/selected_{kind}_host"), json!("ASIO")).0, 200);
        assert_eq!(settings.get(&format!("SELECTED_{}_HOST", kind.to_uppercase())), Some(json!("ASIO")));
        assert_eq!(settings.get(&format!("SELECTED_{}_DEVICE", kind.to_uppercase())), Some(json!("VB-Matrix VASIO-8")));
        assert_eq!(controller.answer(&format!("/get/data/selectable_{kind}_device_list"), Value::Null), (200, json!(["VB-Matrix VASIO-8"])));
        assert_eq!(controller.answer(&format!("/set/data/selected_{kind}_device"), json!("Mic A")).0, 400);
        assert_eq!(controller.answer(&format!("/set/data/selected_{kind}_host"), json!("missing host")).0, 400);
    }
    assert!(events.0.lock().unwrap().iter().any(|r| r.endpoint == "/run/selected_speaker_device"));
    settings.flush().unwrap();
}

#[test]
fn failed_speaker_host_change_restores_both_settings_without_publishing_stale_devices() {
    let (controller, settings, events, effects) = controller();
    effects.0.store(true, Ordering::SeqCst);
    assert_eq!(controller.answer("/set/data/selected_speaker_host", json!("ASIO")).0, 400);
    assert_eq!(settings.get("SELECTED_SPEAKER_HOST"), Some(json!("Windows WASAPI")));
    assert_eq!(settings.get("SELECTED_SPEAKER_DEVICE"), Some(json!("Speaker A [Loopback]")));
    assert!(events.0.lock().unwrap().is_empty());
}
impl Backend for NoCapture {
    fn selected_device(&self, _: Kind) -> Option<Device> {
        None
    }
    fn open_recorder(&self, _: Kind, _: &Device) -> Result<Arc<dyn Recorder>, String> {
        panic!("route construction must not open audio")
    }
    fn create_transcriber(&self, _: Kind, _: Format) -> Result<Box<dyn Transcriber>, String> {
        panic!("route construction must not load speech")
    }
}
struct NoRemote;
impl Remote for NoRemote {
    fn deepl(&self, _: &str, _: &str, _: &str, _: &str) -> Result<String, String> {
        panic!("route construction must not contact a provider")
    }
    fn llm(&self, _: vrct_core::translation::llm::Request) -> Result<String, String> {
        panic!("route construction must not contact a provider")
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_legacy_route_has_a_native_owner_and_every_initialization_getter_answers() {
    let settings = settings();
    let events = Arc::new(Events::default());
    let sinks = Arc::new(Sinks::new(Arc::new(ConfigReplica::over(settings.clone()))));
    let runtime = Runtime::new(
        settings.clone(),
        Arc::new(NoCapture),
        Arc::new(NativeTranslator::new(Arc::new(NoRemote), None)),
        sinks.clone(),
        events.clone(),
    );
    let services = Services::new(
        settings,
        runtime,
        sinks,
        events.clone(),
        std::path::PathBuf::from("missing-test-resources"),
        #[cfg(feature = "ct2")]
        Arc::new(vrct_core::translation::ct2::Engine::default()),
    );
    let native = NativeController::new(services, Arc::new(DevicesFake));
    let router = native.register(Router::new(events));
    let app_routes = [
        "/run/shutdown",
        "/run/open_filepath_logs",
        "/run/open_filepath_config_file",
        "/run/feed_watchdog",
        "/get/data/available_releases",
        "/run/update_software",
        "/run/update_cuda_software",
    ];
    let missing: Vec<_> = vrct_core::controller::contract()
        .iter()
        .filter(|r| !router.is_owned(&r.endpoint) && !app_routes.contains(&r.endpoint.as_str()))
        .map(|r| r.endpoint.clone())
        .collect();
    assert!(missing.is_empty(), "missing native endpoints: {missing:?}");
    let snapshot = native.snapshot().unwrap();
    assert!(snapshot.as_object().unwrap().len() > 110);
    assert!(snapshot["/get/data/selectable_mic_device_list"].is_array());
    assert_eq!(snapshot["/get/data/selected_speaker_host"], json!("Windows WASAPI"));
    assert_eq!(snapshot["/get/data/selectable_speaker_host_list"], json!(["Windows WASAPI", "ASIO"]));
    for endpoint in vrct_core::controller::NATIVE_ENDPOINTS {
        assert!(router.is_owned(endpoint), "missing native endpoint: {endpoint}");
    }
    assert!(snapshot["/get/data/openai_auth_key"].is_null());
    native.services.shutdown();
    assert!(native.initialize().await.is_err());
    assert!(native
        .services
        .download("translation", "NLLB-200")
        .await
        .is_err());
}
#[test]
fn failed_service_change_rolls_back_config() {
    let (controller, settings, _, effect) = controller();
    let old = settings.get("WEBSOCKET_PORT").unwrap();
    effect.0.store(true, Ordering::SeqCst);
    let reply = controller.answer("/set/data/websocket_port", json!(23456));
    assert_eq!(reply.0, 400);
    assert_eq!(reply.1["data"], old);
    assert_eq!(settings.get("WEBSOCKET_PORT").unwrap(), old);
}

#[test]
fn asio_panel_rejects_invalid_roles_and_non_asio_hosts_without_loading_a_driver() {
    let (controller, _, _, _) = controller();
    for role in [json!(null), json!("output"), json!("mic"), json!("speaker")] {
        assert_eq!(controller.answer("/run/open_asio_control_panel", role).0, 400);
    }
}
#[test]
fn model_selection_requires_an_authenticated_model_catalog() {
    let (controller, settings, _, _) = controller();
    assert_eq!(
        controller
            .answer("/set/data/selected_openai_model", json!("invented"))
            .0,
        400
    );
    settings
        .set("SELECTABLE_OPENAI_MODEL_LIST", json!(["gpt-known"]))
        .unwrap();
    assert_eq!(
        controller.answer("/set/data/selected_openai_model", json!("gpt-known")),
        (200, json!("gpt-known"))
    );
    assert_eq!(
        controller
            .answer("/set/data/selected_openai_model", json!("missing"))
            .1["error_code"],
        "MODEL_OPENAI_INVALID"
    );
}
#[derive(Default)]
struct CatalogFake(Mutex<Vec<String>>);
impl Catalog for CatalogFake {
    fn fetch(
        &self,
        engine: String,
        _: Option<String>,
        _: Option<String>,
    ) -> futures_util::future::BoxFuture<'static, Result<Models, String>> {
        self.0.lock().unwrap().push(engine);
        Box::pin(async {
            Ok(Models {
                names: Some(vec!["model-first".into(), "model-second".into()]),
                languages: Value::Null,
            })
        })
    }
}
#[tokio::test]
async fn auth_validation_does_not_send_invalid_keys_and_success_publishes_models() {
    let (controller, settings, events, _) = controller();
    let catalog = Arc::new(CatalogFake::default());
    let auth = Auth::new(settings.clone(), controller, catalog.clone());
    let invalid = auth
        .refresh("openai", "OpenAI_API", Some("sk-short".into()))
        .await;
    assert_eq!(invalid.1["error_code"], "AUTH_OPENAI_INVALID");
    assert!(catalog.0.lock().unwrap().is_empty());
    let key = format!("sk-{}", "x".repeat(164));
    assert_eq!(
        auth.refresh("openai", "OpenAI_API", Some(key.clone()))
            .await,
        (200, json!(key))
    );
    assert_eq!(
        settings.get("SELECTED_OPENAI_MODEL").unwrap(),
        "model-first"
    );
    assert!(events
        .0
        .lock()
        .unwrap()
        .iter()
        .any(|r| r.endpoint == "/run/selectable_openai_model_list"));
    assert_eq!(auth.getter("/get/data/openai_auth_key").unwrap(), key);
}
#[tokio::test]
async fn failed_obs_bind_preserves_existing_websocket_server() {
    let settings = settings();
    let sinks = Sinks::new(Arc::new(ConfigReplica::over(settings.clone())));
    let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let ws = reserved.local_addr().unwrap().port();
    drop(reserved);
    settings.set("WEBSOCKET_PORT", json!(ws)).unwrap();
    settings.set("WEBSOCKET_SERVER", json!(true)).unwrap();
    sinks.configure("WEBSOCKET_SERVER", &settings).unwrap();
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    settings
        .set(
            "OBS_BROWSER_SOURCE_PORT",
            json!(occupied.local_addr().unwrap().port()),
        )
        .unwrap();
    settings.set("OBS_BROWSER_SOURCE", json!(true)).unwrap();
    assert!(sinks.configure("OBS_BROWSER_SOURCE", &settings).is_err());
    assert!(sinks.websocket_alive());
    assert!(tokio::net::TcpStream::connect(("127.0.0.1", ws))
        .await
        .is_ok());
    sinks.shutdown();
}
