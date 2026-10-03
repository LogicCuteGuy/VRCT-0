//! Small fixture files prove the production downloader without loading weights.
mod common;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use sha1::Sha1;
use sha2::{Digest, Sha256};
use vrct_core::models::{manifests, FileSpec, Manager, Manifest};
use vrct_core::protocol::Response;
use vrct_core::router::ResponseSink;

const CONFIG: &str = "{\"model_type\":\"fixture\"}\n";
const MODEL: &str = "fixture-model-binary-123";
const REVISION: &str = "1234567890123456789012345678901234567890";

#[derive(Default)]
struct Sink(Mutex<Vec<Response>>);
impl ResponseSink for Sink {
    fn emit(&self, response: Response) {
        self.0.lock().unwrap().push(response);
    }
}

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let mut nonce = [0; 12];
        getrandom::fill(&mut nonce).unwrap();
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/model-tests");
        let root = std::path::absolute(base).unwrap().join(hex::encode(nonce));
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let base = std::path::absolute(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/model-tests"),
        )
        .unwrap();
        assert!(self.0.starts_with(base) && self.0.file_name().is_some());
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn spec(path: &str, body: &str, algorithm: &str) -> FileSpec {
    let digest = if algorithm == "git-sha1" {
        let bytes = [format!("blob {}\0", body.len()).as_bytes(), body.as_bytes()].concat();
        hex::encode(Sha1::digest(bytes))
    } else {
        hex::encode(Sha256::digest(body.as_bytes()))
    };
    FileSpec {
        path: path.into(),
        repo: "fixture/model".into(),
        revision: REVISION.into(),
        file: path.into(),
        size: body.len() as u64,
        algorithm: algorithm.into(),
        digest,
    }
}

fn manifest() -> Manifest {
    Manifest {
        kind: "whisper".into(),
        weight: "tiny".into(),
        files: vec![
            spec("config.json", CONFIG, "git-sha1"),
            spec("model.bin", MODEL, "sha256"),
        ],
    }
}

fn make_manager(root: &Directory, sink: &Arc<Sink>, base: &str) -> Arc<Manager> {
    Arc::new(Manager::with_source(&root.0, sink.clone(), base, vec![manifest()]).unwrap())
}

fn no_staging(root: &Directory) {
    let parent = root.0.join("weights/whisper");
    if parent.exists() {
        assert!(std::fs::read_dir(parent)
            .unwrap()
            .flatten()
            .all(|entry| !entry.file_name().to_string_lossy().starts_with(".vrct-")));
    }
}

#[test]
fn production_manifests_pin_all_legacy_models_and_native_tokenizers() {
    let manifests = manifests();
    assert_eq!(
        manifests.iter().filter(|m| m.kind == "ctranslate2").count(),
        5
    );
    assert_eq!(manifests.iter().filter(|m| m.kind == "whisper").count(), 9);
    let directory = Directory::new();
    let sink = Arc::new(Sink::default());
    Manager::with_source(
        &directory.0,
        sink,
        "https://huggingface.co",
        manifests.clone(),
    )
    .unwrap();
    for manifest in &manifests {
        assert!(manifest.files.iter().any(|file| file.path == "config.json"));
        assert!(manifest
            .files
            .iter()
            .any(|file| file.path == "model.bin" && file.algorithm == "sha256"));
        assert!(manifest
            .files
            .iter()
            .all(|file| file.revision.len() == 40 && file.size > 0));
        if manifest.kind == "ctranslate2" {
            let expected = if manifest.weight.starts_with("m2m100") {
                "tokenizer/sentencepiece.bpe.model"
            } else {
                "tokenizer/tokenizer.json"
            };
            assert!(manifest.files.iter().any(|file| file.path == expected));
            assert!(manifest
                .files
                .iter()
                .any(|file| file.path == "tokenizer/tokenizer_config.json"));
        } else {
            assert!(manifest
                .files
                .iter()
                .any(|file| file.path == "tokenizer.json"));
        }
    }
    let turbo = manifests
        .iter()
        .find(|manifest| manifest.weight == "large-v3-turbo-int8")
        .unwrap();
    assert!(turbo
        .files
        .iter()
        .any(|file| file.repo == "Zoont/faster-whisper-large-v3-turbo-int8-ct2"));
    assert!(!turbo.files.iter().any(|file| file.path == "vocabulary.txt"));
}

#[tokio::test]
async fn downloads_verify_publish_and_emit_monotonic_completion() {
    let server = common::mock(vec![(200, CONFIG.into()), (200, MODEL.into())]).await;
    let directory = Directory::new();
    let sink = Arc::new(Sink::default());
    let manager = make_manager(&directory, &sink, &server.base());
    assert!(!manager.available("whisper", "tiny"));
    let path = manager.download("whisper", "tiny").await.unwrap();
    assert_eq!(
        std::fs::read_to_string(path.join("model.bin")).unwrap(),
        MODEL
    );
    assert!(manager.available("whisper", "tiny"));
    assert!(manager.available_async("whisper", "tiny").await);
    assert_eq!(manager.all_status("whisper"), json!({"tiny": true}));
    assert_eq!(manager.all_status("unknown"), json!({}));
    no_staging(&directory);
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0]
        .request_line
        .contains(&format!("/fixture/model/resolve/{REVISION}/config.json")));
    assert!(requests[1]
        .request_line
        .contains(&format!("/fixture/model/resolve/{REVISION}/model.bin")));
    {
        let responses = sink.0.lock().unwrap();
        let progresses: Vec<_> = responses
            .iter()
            .filter(|response| response.endpoint == "/run/download_progress_whisper_weight")
            .map(|response| response.result["progress"].as_f64().unwrap())
            .collect();
        assert_eq!(progresses.first(), Some(&0.0));
        assert_eq!(progresses.last(), Some(&1.0));
        assert!(progresses.windows(2).all(|pair| pair[0] <= pair[1]));
        assert_eq!(
            responses.last().unwrap().endpoint,
            "/run/downloaded_whisper_weight"
        );
        assert_eq!(responses.last().unwrap().result, "tiny");
    }
    manager.download("whisper", "tiny").await.unwrap();
    assert_eq!(
        server.requests().len(),
        2,
        "verified installed models require no network"
    );
}

#[tokio::test]
async fn integrity_failure_removes_staging_and_preserves_previous_directory() {
    let server = common::mock(vec![(200, CONFIG.into()), (200, "X".repeat(MODEL.len()))]).await;
    let directory = Directory::new();
    let sink = Arc::new(Sink::default());
    let manager = make_manager(&directory, &sink, &server.base());
    let previous = manager.model_path("whisper", "tiny").unwrap();
    std::fs::create_dir_all(&previous).unwrap();
    std::fs::write(previous.join("model.bin"), "old model stays").unwrap();
    std::fs::write(previous.join("user-note.txt"), "preserve on failure").unwrap();
    assert!(manager
        .download("whisper", "tiny")
        .await
        .unwrap_err()
        .contains("integrity mismatch"));
    assert_eq!(
        std::fs::read_to_string(previous.join("model.bin")).unwrap(),
        "old model stays"
    );
    assert_eq!(
        std::fs::read_to_string(previous.join("user-note.txt")).unwrap(),
        "preserve on failure"
    );
    assert!(!manager.available("whisper", "tiny"));
    no_staging(&directory);
    assert_eq!(
        server.requests().len(),
        4,
        "initial config plus three model attempts"
    );
    assert!(!sink
        .0
        .lock()
        .unwrap()
        .iter()
        .any(|response| response.endpoint.contains("downloaded_")));
}

#[tokio::test]
async fn checksum_and_truncated_files_retry_but_404_does_not() {
    let server = common::mock(vec![
        (503, "unavailable".into()),
        (200, CONFIG.into()),
        (200, "short".into()),
        (200, MODEL.into()),
    ])
    .await;
    let directory = Directory::new();
    let sink = Arc::new(Sink::default());
    let manager = make_manager(&directory, &sink, &server.base());
    manager.download("whisper", "tiny").await.unwrap();
    assert!(manager.available("whisper", "tiny"));
    assert_eq!(server.requests().len(), 4);
    no_staging(&directory);

    let server = common::mock(vec![(404, "not found".into())]).await;
    let directory = Directory::new();
    let manager = make_manager(&directory, &sink, &server.base());
    assert_eq!(
        manager.download("whisper", "tiny").await.unwrap_err(),
        "model download HTTP 404"
    );
    assert_eq!(server.requests().len(), 1);
    no_staging(&directory);
}

#[tokio::test]
async fn availability_cache_invalidates_when_files_change() {
    let server = common::mock(vec![(200, CONFIG.into()), (200, MODEL.into())]).await;
    let directory = Directory::new();
    let sink = Arc::new(Sink::default());
    let manager = make_manager(&directory, &sink, &server.base());
    let path = manager.download("whisper", "tiny").await.unwrap();
    assert!(manager.available("whisper", "tiny"));
    std::fs::write(path.join("model.bin"), "truncated").unwrap();
    assert!(!manager.available("whisper", "tiny"));
    assert!(!manager.available_async("whisper", "tiny").await);
    assert_eq!(manager.all_status("whisper"), json!({"tiny": false}));
}

#[tokio::test]
async fn successful_staged_replacement_cleans_previous_directory_and_rechecks_equal_size_corruption(
) {
    let server = common::mock(vec![(200, CONFIG.into()), (200, MODEL.into())]).await;
    let directory = Directory::new();
    let sink = Arc::new(Sink::default());
    let manager = make_manager(&directory, &sink, &server.base());
    let previous = manager.model_path("whisper", "tiny").unwrap();
    std::fs::create_dir_all(&previous).unwrap();
    std::fs::write(previous.join("model.bin"), "old partial").unwrap();
    let path = manager.download("whisper", "tiny").await.unwrap();
    assert_eq!(
        std::fs::read_to_string(path.join("model.bin")).unwrap(),
        MODEL
    );
    no_staging(&directory);
    assert!(manager.available("whisper", "tiny"));
    let modified = std::fs::metadata(path.join("model.bin"))
        .unwrap()
        .modified()
        .unwrap();
    std::fs::write(path.join("model.bin"), "X".repeat(MODEL.len())).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(path.join("model.bin"))
        .unwrap()
        .set_modified(modified + Duration::from_secs(1))
        .unwrap();
    assert!(
        !manager.available("whisper", "tiny"),
        "equal-length corruption invalidates the metadata cache and is rehashed"
    );
}

async fn stalled_server() -> (
    String,
    tokio::sync::oneshot::Receiver<()>,
    tokio::task::JoinHandle<()>,
) {
    use tokio::io::AsyncReadExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buffer = [0u8; 1024];
        assert!(stream.read(&mut buffer).await.unwrap() > 0);
        let _ = sender.send(());
        std::future::pending::<()>().await;
    });
    (base, receiver, task)
}

#[tokio::test]
async fn cancellation_interrupts_stalled_http_cleans_up_and_future_downloads_work() {
    let (base, connected, server_task) = stalled_server().await;
    let directory = Directory::new();
    let sink = Arc::new(Sink::default());
    let manager = make_manager(&directory, &sink, &base);
    let downloader = Arc::clone(&manager);
    let task = tokio::spawn(async move { downloader.download("whisper", "tiny").await });
    tokio::time::timeout(Duration::from_secs(2), connected)
        .await
        .unwrap()
        .unwrap();
    manager.cancel();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err(),
        "model download cancelled"
    );
    no_staging(&directory);
    assert!(!manager.available("whisper", "tiny"));
    server_task.abort();

    // A generation change cancels existing work, not a permanent stop flag.
    let server = common::mock(vec![(200, CONFIG.into()), (200, MODEL.into())]).await;
    let manager = make_manager(&directory, &sink, &server.base());
    manager.cancel();
    manager.download("whisper", "tiny").await.unwrap();
}

#[tokio::test]
async fn aborting_download_future_also_removes_staging() {
    let (base, connected, server_task) = stalled_server().await;
    let directory = Directory::new();
    let sink = Arc::new(Sink::default());
    let manager = make_manager(&directory, &sink, &base);
    let downloader = Arc::clone(&manager);
    let task = tokio::spawn(async move { downloader.download("whisper", "tiny").await });
    tokio::time::timeout(Duration::from_secs(2), connected)
        .await
        .unwrap()
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    no_staging(&directory);
    assert!(!manager.model_path("whisper", "tiny").unwrap().exists());
    server_task.abort();
}

#[tokio::test]
async fn operation_guard_serializes_downloads_and_loads_and_cancel_releases_queue() {
    let server = common::mock(vec![(200, CONFIG.into()), (200, MODEL.into())]).await;
    let directory = Directory::new();
    let sink = Arc::new(Sink::default());
    let manager = make_manager(&directory, &sink, &server.base());
    let guard = manager.operation_guard().await;
    let downloader = Arc::clone(&manager);
    let task = tokio::spawn(async move { downloader.download("whisper", "tiny").await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(server.requests().is_empty());
    manager.cancel();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err(),
        "model download cancelled"
    );
    drop(guard);
    let (first, second) = tokio::join!(
        manager.download("whisper", "tiny"),
        manager.download("whisper", "tiny")
    );
    assert_eq!(first.unwrap(), second.unwrap());
    assert_eq!(
        server.requests().len(),
        2,
        "second caller sees the completed install"
    );
}

#[test]
fn untrusted_paths_and_unknown_models_are_rejected_before_filesystem_or_network() {
    let directory = Directory::new();
    let sink = Arc::new(Sink::default());
    for path in [
        "../escape",
        "/escape",
        "C:/escape",
        "tokenizer/../../escape",
        "..\\escape",
        "CON",
        "model.bin.",
        "config.json ",
    ] {
        let mut manifest = manifest();
        manifest.files[0].path = path.into();
        assert!(Manager::with_source(
            &directory.0,
            sink.clone(),
            "http://127.0.0.1:1",
            vec![manifest]
        )
        .is_err());
    }
    let manager = make_manager(&directory, &sink, "http://127.0.0.1:1");
    assert!(manager.model_path("whisper", "../escape").is_err());
    assert!(!manager.available("whisper", "unknown"));
    assert!(std::fs::read_dir(&directory.0).unwrap().next().is_none());
}

#[test]
fn prior_python_tokenizer_cache_is_readable_even_with_an_incomplete_cache_repository() {
    let directory = Directory::new();
    let sink = Arc::new(Sink::default());
    let mut manifest = manifest();
    manifest.files.push(spec(
        "tokenizer/tokenizer.json",
        "{\"fixture\":true}",
        "git-sha1",
    ));
    let manager =
        Manager::with_source(&directory.0, sink, "http://127.0.0.1:1", vec![manifest]).unwrap();
    let path = manager.model_path("whisper", "tiny").unwrap();
    std::fs::create_dir_all(path.join("tokenizer/models--incomplete")).unwrap();
    let snapshot = path
        .join("tokenizer/models--fixture--tokenizer/snapshots")
        .join(REVISION);
    std::fs::create_dir_all(&snapshot).unwrap();
    std::fs::write(path.join("config.json"), CONFIG).unwrap();
    std::fs::write(path.join("model.bin"), MODEL).unwrap();
    std::fs::write(snapshot.join("tokenizer.json"), "{\"fixture\":true}").unwrap();
    assert!(manager.available("whisper", "tiny"));
}
