//! Verified, cancellable Hugging Face model downloads without Python.
//!
//! The bundled manifest pins every model/tokenizer artifact to a commit. LFS
//! objects use SHA-256; regular Git files use the Git blob SHA-1. Downloads are
//! staged beside their destination, and published only after all hashes pass.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use reqwest::{Client, Url};
use serde::Deserialize;
use serde_json::{json, Value};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::sync::{watch, OwnedMutexGuard};

use crate::protocol::Response;
use crate::router::ResponseSink;

#[derive(Clone, Debug, Deserialize)]
pub struct FileSpec {
    /// Relative path within the installed model, possibly under `tokenizer/`.
    pub path: String,
    pub repo: String,
    pub revision: String,
    pub file: String,
    pub size: u64,
    pub algorithm: String,
    pub digest: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Manifest {
    pub kind: String,
    pub weight: String,
    pub files: Vec<FileSpec>,
}

pub fn manifests() -> Vec<Manifest> {
    serde_json::from_str(include_str!("models/manifest.json"))
        .expect("bundled model manifests are valid JSON")
}

#[derive(Clone, PartialEq, Eq)]
struct Stamp {
    size: u64,
    modified: SystemTime,
}

pub struct Manager {
    root: PathBuf,
    sink: Arc<dyn ResponseSink>,
    manifests: Vec<Manifest>,
    origin: Url,
    client: Client,
    gate: Arc<tokio::sync::Mutex<()>>,
    cancellation: watch::Sender<u64>,
    verified: Mutex<HashMap<PathBuf, Vec<Stamp>>>,
}

impl Manager {
    pub fn new(root: impl Into<PathBuf>, sink: Arc<dyn ResponseSink>) -> Self {
        Self::with_source(root, sink, "https://huggingface.co", manifests())
            .expect("production model manager configuration is valid")
    }

    /// Fixture/proxy seam: download URLs retain `/repo/resolve/revision/file`.
    pub fn with_source(
        root: impl Into<PathBuf>,
        sink: Arc<dyn ResponseSink>,
        origin: &str,
        manifests: Vec<Manifest>,
    ) -> Result<Self, String> {
        validate_manifests(&manifests)?;
        let root = std::path::absolute(root.into())
            .map_err(|error| format!("invalid model root: {error}"))?;
        let origin = Url::parse(origin).map_err(|_| "invalid model download origin")?;
        if !matches!(origin.scheme(), "http" | "https")
            || origin.host_str().is_none()
            || !origin.username().is_empty()
            || origin.password().is_some()
        {
            return Err("invalid model download origin".into());
        }
        let client = Client::builder()
            .user_agent("VRCT-native-model-downloader/1")
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(60))
            .build()
            .map_err(|_| "could not construct model download client")?;
        let (cancellation, _) = watch::channel(0);
        Ok(Self {
            root,
            sink,
            manifests,
            origin,
            client,
            gate: Arc::new(tokio::sync::Mutex::new(())),
            cancellation,
            verified: Mutex::new(HashMap::new()),
        })
    }

    fn manifest(&self, kind: &str, weight: &str) -> Result<&Manifest, String> {
        self.manifests
            .iter()
            .find(|manifest| manifest.kind == kind && manifest.weight == weight)
            .ok_or_else(|| format!("unknown {kind} model {weight}"))
    }

    pub fn model_path(&self, kind: &str, weight: &str) -> Result<PathBuf, String> {
        self.manifest(kind, weight)?;
        Ok(self.root.join("weights").join(kind).join(weight))
    }

    /// Hold this while loading/replacing a model so it cannot race publication.
    /// `download` acquires the same lock itself; do not wrap it in another guard.
    pub async fn operation_guard(&self) -> OwnedMutexGuard<()> {
        Arc::clone(&self.gate).lock_owned().await
    }

    /// Cancel current and queued downloads, without poisoning future downloads.
    pub fn cancel(&self) {
        self.cancellation
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }

    /// Integrity availability, not a claim that this machine can load the model.
    /// Hashing is cached until file sizes/modification times change. Call from a
    /// blocking worker for the first scan of multi-gigabyte installed weights.
    pub fn available(&self, kind: &str, weight: &str) -> bool {
        let Ok(manifest) = self.manifest(kind, weight) else {
            return false;
        };
        let Ok(directory) = self.model_path(kind, weight) else {
            return false;
        };
        let required: Vec<_> = manifest
            .files
            .iter()
            .filter(|file| !is_documentation(&file.path))
            .collect();
        let files: Option<Vec<_>> = required
            .iter()
            .map(|file| locate(&directory, &file.path))
            .collect();
        let Some(files) = files else {
            return false;
        };
        let stamps: Option<Vec<_>> = files.iter().map(|file| stamp(file).ok()).collect();
        let Some(stamps) = stamps else {
            return false;
        };
        if self.verified.lock().unwrap().get(&directory) == Some(&stamps) {
            return true;
        }
        if required
            .iter()
            .zip(&files)
            .any(|(spec, path)| verify_file(path, spec).is_err())
        {
            return false;
        }
        self.verified.lock().unwrap().insert(directory, stamps);
        true
    }

    pub fn all_status(&self, kind: &str) -> Value {
        Value::Object(
            self.manifests
                .iter()
                .filter(|manifest| manifest.kind == kind)
                .map(|manifest| {
                    (
                        manifest.weight.clone(),
                        json!(self.available(kind, &manifest.weight)),
                    )
                })
                .collect(),
        )
    }

    /// Async entry point for the first integrity scan; hashing runs off the
    /// async executor so a download/status check cannot freeze other workers.
    pub async fn available_async(&self, kind: &str, weight: &str) -> bool {
        let Ok(manifest) = self.manifest(kind, weight) else {
            return false;
        };
        let Ok(directory) = self.model_path(kind, weight) else {
            return false;
        };
        let required: Vec<_> = manifest
            .files
            .iter()
            .filter(|file| !is_documentation(&file.path))
            .cloned()
            .collect();
        let files: Option<Vec<_>> = required
            .iter()
            .map(|file| locate(&directory, &file.path))
            .collect();
        let Some(files) = files else {
            return false;
        };
        let stamps: Option<Vec<_>> = files.iter().map(|file| stamp(file).ok()).collect();
        let Some(stamps) = stamps else {
            return false;
        };
        if self.verified.lock().unwrap().get(&directory) == Some(&stamps) {
            return true;
        }
        let valid = tokio::task::spawn_blocking(move || {
            required
                .iter()
                .zip(&files)
                .all(|(spec, path)| verify_file(path, spec).is_ok())
        })
        .await
        .unwrap_or(false);
        if valid {
            self.verified.lock().unwrap().insert(directory, stamps);
        }
        valid
    }

    fn emit_progress(&self, kind: &str, weight: &str, progress: f64) {
        self.sink.emit(Response::new(
            200,
            format!("/run/download_progress_{kind}_weight"),
            json!({"weight_type": weight, "progress": progress}),
        ));
    }

    pub async fn download(&self, kind: &str, weight: &str) -> Result<PathBuf, String> {
        let manifest = self.manifest(kind, weight)?.clone();
        let mut cancellation = self.cancellation.subscribe();
        let generation = *cancellation.borrow_and_update();
        let _guard = tokio::select! {
            guard = self.operation_guard() => guard,
            _ = cancellation.changed() => return Err("model download cancelled".into()),
        };
        check_cancelled(&cancellation, generation)?;
        let target = self.model_path(kind, weight)?;
        let available = tokio::select! {
            available = self.available_async(kind, weight) => available,
            _ = cancellation.changed() => return Err("model download cancelled".into()),
        };
        check_cancelled(&cancellation, generation)?;
        if available {
            self.emit_progress(kind, weight, 1.0);
            self.sink.emit(Response::new(
                200,
                format!("/run/downloaded_{kind}_weight"),
                json!(weight),
            ));
            return Ok(target);
        }
        let parent = target.parent().ok_or("model path has no parent")?;
        ensure_no_symlinks(&self.root, parent)?;
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| format!("could not create model directory: {error}"))?;
        ensure_no_symlinks(&self.root, parent)?;
        let nonce = nonce()?;
        let stage = parent.join(format!(".vrct-{weight}-{nonce}.download"));
        tokio::fs::create_dir(&stage)
            .await
            .map_err(|error| format!("could not create model staging directory: {error}"))?;
        let mut cleanup = Stage {
            path: stage.clone(),
            active: true,
        };
        self.emit_progress(kind, weight, 0.0);
        let result = self
            .download_files(&manifest, &stage, &mut cancellation, generation)
            .await;
        // Cleanup is also guarded by Drop if the caller aborts the future.
        result?;
        check_cancelled(&cancellation, generation)?;
        ensure_no_symlinks(&self.root, &target)?;
        let backup = parent.join(format!(".vrct-{weight}-{nonce}.previous"));
        let had_target = target.exists();
        if had_target {
            std::fs::rename(&target, &backup).map_err(|error| {
                format!("could not preserve existing model before install: {error}")
            })?;
        }
        // Synchronous rename pair avoids an abort/cancellation point between
        // preserving the old directory and publishing the verified replacement.
        if let Err(error) = std::fs::rename(&stage, &target) {
            if had_target {
                std::fs::rename(&backup, &target).map_err(|rollback| format!("model publication failed: {error}; restoring previous model failed: {rollback}"))?;
            }
            return Err(format!("model publication failed: {error}"));
        }
        cleanup.active = false;
        if had_target {
            if backup.is_dir() {
                let _ = std::fs::remove_dir_all(&backup);
            } else {
                let _ = std::fs::remove_file(&backup);
            }
        }
        self.verified.lock().unwrap().remove(&target);
        self.emit_progress(kind, weight, 1.0);
        self.sink.emit(Response::new(
            200,
            format!("/run/downloaded_{kind}_weight"),
            json!(weight),
        ));
        Ok(target)
    }

    async fn download_files(
        &self,
        manifest: &Manifest,
        stage: &Path,
        cancellation: &mut watch::Receiver<u64>,
        generation: u64,
    ) -> Result<(), String> {
        let total = manifest
            .files
            .iter()
            .try_fold(0u64, |sum, file| sum.checked_add(file.size))
            .ok_or("model manifest size overflow")?;
        let mut completed = 0;
        let mut last_progress = 0.0;
        for spec in &manifest.files {
            check_cancelled(cancellation, generation)?;
            let target = stage.join(&spec.path);
            if let Some(parent) = target.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|error| format!("could not create staged model directory: {error}"))?;
            }
            let mut url = self.origin.clone();
            url.set_path(&format!(
                "/{}/resolve/{}/{}",
                spec.repo, spec.revision, spec.file
            ));
            let mut last_error = String::new();
            for attempt in 0..3 {
                let result = self
                    .download_file(&url, spec, &target, cancellation, generation, |received| {
                        let progress = (completed + received) as f64 / total.max(1) as f64;
                        // A retried file never makes the UI move backwards. Cap at
                        // 99% until every hash and the publication have succeeded.
                        let progress = progress.min(0.99);
                        if progress - last_progress >= 0.005 {
                            last_progress = progress;
                            self.emit_progress(&manifest.kind, &manifest.weight, progress);
                        }
                    })
                    .await;
                match result {
                    Ok(()) => {
                        last_error.clear();
                        break;
                    }
                    Err(DownloadError::Cancelled) => return Err("model download cancelled".into()),
                    Err(DownloadError::Permanent(error)) => return Err(error),
                    Err(DownloadError::Retry(error)) => last_error = error,
                }
                let _ = tokio::fs::remove_file(&target).await;
                if attempt < 2 {
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_millis(200 * (attempt + 1))) => {},
                        _ = cancellation.changed() => return Err("model download cancelled".into()),
                    }
                    check_cancelled(cancellation, generation)?;
                }
            }
            if !last_error.is_empty() {
                return Err(last_error);
            }
            completed += spec.size;
        }
        Ok(())
    }

    async fn download_file(
        &self,
        url: &Url,
        spec: &FileSpec,
        path: &Path,
        cancellation: &mut watch::Receiver<u64>,
        generation: u64,
        mut progress: impl FnMut(u64),
    ) -> Result<(), DownloadError> {
        check_cancelled(cancellation, generation).map_err(|_| DownloadError::Cancelled)?;
        let request = self.client.get(url.clone()).send();
        let mut response = tokio::select! {
            response = request => response.map_err(|error| DownloadError::Retry(format!("model download request failed: {}", error.without_url())))?,
            _ = cancellation.changed() => return Err(DownloadError::Cancelled),
        };
        let status = response.status();
        if !status.is_success() {
            let message = format!("model download HTTP {}", status.as_u16());
            return Err(
                if matches!(status.as_u16(), 408 | 429) || status.is_server_error() {
                    DownloadError::Retry(message)
                } else {
                    DownloadError::Permanent(message)
                },
            );
        }
        if response
            .content_length()
            .is_some_and(|size| size != spec.size)
        {
            return Err(DownloadError::Retry(format!(
                "model file size mismatch: {}",
                spec.path
            )));
        }
        let mut file = tokio::fs::File::create(path).await.map_err(|error| {
            DownloadError::Permanent(format!("could not create staged file: {error}"))
        })?;
        let mut hash = FileHash::new(spec).map_err(DownloadError::Permanent)?;
        let mut received = 0;
        loop {
            let chunk = tokio::select! {
                chunk = response.chunk() => chunk.map_err(|error| DownloadError::Retry(format!("model download stream failed: {}", error.without_url())))?,
                _ = cancellation.changed() => return Err(DownloadError::Cancelled),
            };
            let Some(chunk) = chunk else {
                break;
            };
            check_cancelled(cancellation, generation).map_err(|_| DownloadError::Cancelled)?;
            received += chunk.len() as u64;
            if received > spec.size {
                return Err(DownloadError::Retry(format!(
                    "model file size mismatch: {}",
                    spec.path
                )));
            }
            hash.update(&chunk);
            file.write_all(&chunk).await.map_err(|error| {
                DownloadError::Permanent(format!("could not write staged model file: {error}"))
            })?;
            progress(received);
        }
        if received != spec.size || hash.finish() != spec.digest {
            return Err(DownloadError::Retry(format!(
                "model file integrity mismatch: {}",
                spec.path
            )));
        }
        file.sync_all().await.map_err(|error| {
            DownloadError::Permanent(format!("could not flush staged model file: {error}"))
        })?;
        Ok(())
    }
}

enum DownloadError {
    Cancelled,
    Permanent(String),
    Retry(String),
}

struct Stage {
    path: PathBuf,
    active: bool,
}
impl Drop for Stage {
    fn drop(&mut self) {
        if self.active {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

enum FileHash {
    Sha256(Sha256),
    Git(Sha1),
}
impl FileHash {
    fn new(spec: &FileSpec) -> Result<Self, String> {
        match spec.algorithm.as_str() {
            "sha256" => Ok(Self::Sha256(Sha256::new())),
            "git-sha1" => {
                let mut hash = Sha1::new();
                hash.update(format!("blob {}\0", spec.size).as_bytes());
                Ok(Self::Git(hash))
            }
            _ => Err("unknown model integrity algorithm".into()),
        }
    }
    fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Sha256(hash) => hash.update(bytes),
            Self::Git(hash) => hash.update(bytes),
        }
    }
    fn finish(self) -> String {
        match self {
            Self::Sha256(hash) => hex::encode(hash.finalize()),
            Self::Git(hash) => hex::encode(hash.finalize()),
        }
    }
}

fn verify_file(path: &Path, spec: &FileSpec) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("model file unavailable: {error}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() != spec.size {
        return Err("model file size/type mismatch".into());
    }
    let mut file =
        std::fs::File::open(path).map_err(|error| format!("model file unavailable: {error}"))?;
    let mut hash = FileHash::new(spec)?;
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let received = file
            .read(&mut buffer)
            .map_err(|error| format!("could not verify model: {error}"))?;
        if received == 0 {
            break;
        }
        hash.update(&buffer[..received]);
    }
    if hash.finish() != spec.digest {
        return Err("model checksum mismatch".into());
    }
    Ok(())
}

fn stamp(path: &Path) -> std::io::Result<Stamp> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(std::io::Error::other("model path is not a regular file"));
    }
    Ok(Stamp {
        size: metadata.len(),
        modified: metadata.modified()?,
    })
}

fn is_documentation(path: &str) -> bool {
    path == "README.md"
        || path.ends_with("/README.md")
        || path == "LICENSE"
        || path.ends_with("/LICENSE")
}

fn locate(directory: &Path, relative: &str) -> Option<PathBuf> {
    let direct = directory.join(relative);
    if direct.is_file() {
        return Some(direct);
    }
    // Earlier Python installs placed AutoTokenizer files inside the HF cache.
    let name = relative.strip_prefix("tokenizer/")?;
    if name.contains('/') {
        return None;
    }
    for repo in std::fs::read_dir(directory.join("tokenizer"))
        .ok()?
        .flatten()
    {
        if !repo.file_name().to_string_lossy().starts_with("models--") {
            continue;
        }
        let Ok(snapshots) = std::fs::read_dir(repo.path().join("snapshots")) else {
            continue;
        };
        for snapshot in snapshots.flatten() {
            let candidate = snapshot.path().join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn check_cancelled(receiver: &watch::Receiver<u64>, generation: u64) -> Result<(), String> {
    if *receiver.borrow() == generation {
        Ok(())
    } else {
        Err("model download cancelled".into())
    }
}

fn nonce() -> Result<String, String> {
    let mut random = [0u8; 12];
    getrandom::fill(&mut random).map_err(|_| "could not create model staging ID")?;
    Ok(hex::encode(random))
}

fn valid_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', ':'])
        && !path.starts_with('/')
        && path.split('/').all(|part| {
            let name = part
                .split('.')
                .next()
                .unwrap_or_default()
                .to_ascii_uppercase();
            !part.is_empty()
                && !matches!(part, "." | "..")
                && !part.ends_with(['.', ' '])
                && !part
                    .chars()
                    .any(|c| c.is_control() || matches!(c, '<' | '>' | '"' | '|' | '?' | '*'))
                && !matches!(name.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                && !(name.len() == 4
                    && (name.starts_with("COM") || name.starts_with("LPT"))
                    && matches!(name.as_bytes()[3], b'1'..=b'9'))
        })
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn validate_manifests(manifests: &[Manifest]) -> Result<(), String> {
    let mut names = HashSet::new();
    for manifest in manifests {
        if !matches!(manifest.kind.as_str(), "ctranslate2" | "whisper")
            || !valid_relative(&manifest.weight)
            || manifest.weight.contains('/')
            || !names.insert((&manifest.kind, &manifest.weight))
            || manifest.files.is_empty()
        {
            return Err("invalid or duplicate model manifest".into());
        }
        let mut paths = HashSet::new();
        for file in &manifest.files {
            let length = match file.algorithm.as_str() {
                "sha256" => 64,
                "git-sha1" => 40,
                _ => return Err("unknown model checksum algorithm".into()),
            };
            if !valid_relative(&file.path)
                || !valid_relative(&file.file)
                || !valid_relative(&file.repo)
                || file.repo.split('/').count() != 2
                || file.revision.len() != 40
                || !file.revision.chars().all(|c| c.is_ascii_hexdigit())
                || file.size == 0
                || file.digest.len() != length
                || !file.digest.chars().all(|c| c.is_ascii_hexdigit())
                || !paths.insert(&file.path)
            {
                return Err("invalid model manifest file".into());
            }
        }
        if !manifest.files.iter().any(|file| file.path == "model.bin")
            || !manifest.files.iter().any(|file| file.path == "config.json")
        {
            return Err("model manifest lacks required files".into());
        }
    }
    Ok(())
}

fn ensure_no_symlinks(root: &Path, target: &Path) -> Result<(), String> {
    if !target.starts_with(root) {
        return Err("model path escapes root".into());
    }
    let mut path = PathBuf::new();
    for component in target.components() {
        path.push(component);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("model path contains a symlink".into())
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("could not inspect model path: {error}")),
        }
    }
    Ok(())
}
