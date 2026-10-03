//! Resource preparation and installer-compatible native release ZIPs.
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

type Result<T> = std::result::Result<T, String>;

mod msvc;
pub use msvc::stage_from_visual_studio as stage_msvc_from_visual_studio;

#[derive(Deserialize)]
struct NativeManifest {
    runtime: Archive,
    files: Vec<PinnedFile>,
}
#[derive(Deserialize)]
struct Archive {
    archive_url: String,
    archive_bytes: u64,
    archive_sha256: String,
    entries: Vec<Entry>,
}
#[derive(Deserialize)]
struct Entry {
    entry: String,
    file: String,
    bytes: u64,
    sha256: String,
}
#[derive(Deserialize)]
struct PinnedFile {
    path: String,
    sha256: String,
}
#[derive(Deserialize)]
struct Dictionary {
    archive_url: String,
    archive_bytes: u64,
    archive_sha256: String,
    dictionary_entry: String,
    dictionary_bytes: u64,
    dictionary_sha256: String,
}
#[derive(Deserialize)]
struct OcrManifest {
    models: Vec<OcrFile>,
}
#[derive(Deserialize)]
struct OcrFile {
    file: String,
    url: String,
    sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PackageFile {
    pub bytes: u64,
    pub sha256: String,
}
#[derive(Debug, Serialize, Deserialize)]
struct PackageManifest {
    format: u32,
    version: String,
    files: BTreeMap<String, PackageFile>,
}

#[derive(Deserialize)]
struct ProtectedFingerprint {
    bytes: u64,
    sha256: String,
    prefix_hex: String,
}

fn protected_fingerprint(root: Option<&Path>) -> Result<ProtectedFingerprint> {
    if let Some(path) = root
        .map(|root| root.join("src-python/models/ocr/onnx/chatbox_yolox_tiny.onnx"))
        .filter(|path| path.is_file())
    {
        let content = file_checksum(&path)?;
        let mut file = File::open(path).map_err(|e| e.to_string())?;
        let mut prefix = vec![0u8; content.bytes.min(128) as usize];
        file.read_exact(&mut prefix).map_err(|e| e.to_string())?;
        return Ok(ProtectedFingerprint {
            bytes: content.bytes,
            sha256: content.sha256,
            prefix_hex: hex::encode(prefix),
        });
    }
    serde_json::from_str(include_str!("../protected-model-fingerprint.json"))
        .map_err(|e| e.to_string())
}

/// Stream an executable looking for the complete protected object, including
/// objects embedded inside `.rdata`. The 128-byte prefix locates candidates;
/// the complete object's SHA-256 confirms them. The fingerprint isn't a model.
fn contains_protected(mut reader: impl Read, fingerprint: &ProtectedFingerprint) -> Result<bool> {
    let prefix = hex::decode(&fingerprint.prefix_hex)
        .map_err(|_| "invalid protected content fingerprint")?;
    if prefix.is_empty() || prefix.len() as u64 > fingerprint.bytes {
        return Err("invalid protected content fingerprint".into());
    }
    struct Candidate {
        start: u64,
        remaining: u64,
        hash: Sha256,
    }
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut tail = Vec::new();
    let mut processed = 0u64;
    let mut chunk = [0u8; 128 * 1024];
    loop {
        let count = reader.read(&mut chunk).map_err(|e| e.to_string())?;
        if count == 0 {
            return Ok(false);
        }
        let buffer = [tail.as_slice(), &chunk[..count]].concat();
        for (offset, bytes) in buffer.windows(prefix.len()).enumerate() {
            if bytes != prefix {
                continue;
            }
            let start = processed - tail.len() as u64 + offset as u64;
            let mut hash = Sha256::new();
            let previous = tail.len().saturating_sub(offset);
            if previous > 0 {
                hash.update(&buffer[offset..tail.len()]);
            }
            candidates.push(Candidate {
                start,
                remaining: fingerprint.bytes - previous as u64,
                hash,
            });
            if candidates.len() > 64 {
                return Err("too many protected model signatures in payload".into());
            }
        }
        for candidate in &mut candidates {
            let offset = candidate.start.saturating_sub(processed) as usize;
            if offset >= count {
                continue;
            }
            let bytes = (count - offset).min(candidate.remaining as usize);
            candidate.hash.update(&chunk[offset..offset + bytes]);
            candidate.remaining -= bytes as u64;
            if candidate.remaining == 0
                && hex::encode(candidate.hash.clone().finalize()) == fingerprint.sha256
            {
                return Ok(true);
            }
        }
        candidates.retain(|candidate| candidate.remaining > 0);
        tail = buffer[buffer.len().saturating_sub(prefix.len() - 1)..].to_vec();
        processed += count as u64;
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

fn checksum(mut reader: impl Read) -> Result<PackageFile> {
    let mut hash = Sha256::new();
    let mut bytes = 0;
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let count = reader.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        bytes += count as u64;
        hash.update(&buffer[..count]);
    }
    Ok(PackageFile {
        bytes,
        sha256: hex::encode(hash.finalize()),
    })
}

fn file_checksum(path: &Path) -> Result<PackageFile> {
    checksum(File::open(path).map_err(|e| format!("{}: {e}", path.display()))?)
}

fn verified(path: &Path, hash: &str, size: Option<u64>) -> bool {
    file_checksum(path)
        .is_ok_and(|got| got.sha256 == hash && size.is_none_or(|size| got.bytes == size))
}

fn profile_path(root: &Path, profile: &str) -> Result<PathBuf> {
    if !matches!(profile, "debug" | "release") {
        return Err("profile must be debug or release".into());
    }
    Ok(root.join("src-tauri/target").join(profile))
}

fn random() -> Result<String> {
    let mut bytes = [0; 12];
    getrandom::fill(&mut bytes).map_err(|e| e.to_string())?;
    Ok(hex::encode(bytes))
}

struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn publish_file(source: &Path, destination: &Path) -> Result<()> {
    if destination.exists() {
        let backup = destination.with_extension(format!("{}.previous", random()?));
        fs::rename(destination, &backup)
            .map_err(|e| format!("could not preserve {}: {e}", destination.display()))?;
        if let Err(error) = fs::rename(source, destination) {
            fs::rename(&backup, destination).map_err(|rollback| {
                format!("publication failed: {error}; rollback failed: {rollback}")
            })?;
            return Err(format!("publication failed: {error}"));
        }
        fs::remove_file(&backup).map_err(|e| format!("could not remove resource backup: {e}"))?;
    } else {
        fs::rename(source, destination).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn download(
    url: &str,
    destination: &Path,
    hash: &str,
    size: Option<u64>,
    offline: bool,
) -> Result<()> {
    if verified(destination, hash, size) {
        return Ok(());
    }
    if offline {
        return Err(format!(
            "offline: missing or corrupt {}",
            destination.display()
        ));
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(1800))
        .build()
        .map_err(|e| e.to_string())?;
    let temporary = Temporary(destination.with_extension(format!("{}.partial", random()?)));
    let mut last = String::new();
    for attempt in 0..3 {
        let result = (|| {
            let mut response = client
                .get(url)
                .send()
                .map_err(|e| format!("download failed: {}", e.without_url()))?;
            let status = response.status();
            if !status.is_success() {
                return Err(format!("resource download HTTP {}", status.as_u16()));
            }
            let mut file = File::create(&temporary.0).map_err(|e| e.to_string())?;
            let mut digest = Sha256::new();
            let mut length = 0;
            let mut buffer = [0u8; 128 * 1024];
            loop {
                let count = response.read(&mut buffer).map_err(|e| e.to_string())?;
                if count == 0 {
                    break;
                }
                length += count as u64;
                if size.is_some_and(|size| length > size) {
                    return Err("resource exceeds expected size".into());
                }
                file.write_all(&buffer[..count])
                    .map_err(|e| e.to_string())?;
                digest.update(&buffer[..count]);
            }
            if hex::encode(digest.finalize()) != hash || size.is_some_and(|size| length != size) {
                return Err("resource checksum/size mismatch".into());
            }
            file.sync_all().map_err(|e| e.to_string())?;
            drop(file);
            publish_file(&temporary.0, destination)
        })();
        match result {
            Ok(()) => {
                println!("Prepared {}", destination.display());
                return Ok(());
            }
            Err(error) => {
                let permanent = error
                    .strip_prefix("resource download HTTP ")
                    .and_then(|status| status.parse::<u16>().ok())
                    .is_some_and(|status| {
                        (400..500).contains(&status) && !matches!(status, 408 | 429)
                    });
                if permanent {
                    return Err(error);
                }
                last = error;
            }
        }
        if attempt < 2 {
            std::thread::sleep(Duration::from_millis(200 * (attempt + 1)));
        }
    }
    Err(last)
}

fn extract(
    archive: &Path,
    entry_name: &str,
    destination: &Path,
    hash: &str,
    bytes: u64,
) -> Result<()> {
    if verified(destination, hash, Some(bytes)) {
        return Ok(());
    }
    let mut archive = ZipArchive::new(File::open(archive).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let mut entry = archive
        .by_name(entry_name)
        .map_err(|e| format!("missing native resource entry {entry_name}: {e}"))?;
    if entry.size() != bytes {
        return Err("archive resource has wrong length".into());
    }
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let temporary = Temporary(destination.with_extension(format!("{}.partial", random()?)));
    let mut output = File::create(&temporary.0).map_err(|e| e.to_string())?;
    std::io::copy(&mut entry, &mut output).map_err(|e| e.to_string())?;
    output.sync_all().map_err(|e| e.to_string())?;
    drop(output);
    if !verified(&temporary.0, hash, Some(bytes)) {
        return Err("archive resource checksum mismatch".into());
    }
    publish_file(&temporary.0, destination)
}

fn validate_relative(path: &str) -> Result<()> {
    if path.is_empty()
        || path.contains(['\\', ':'])
        || path.starts_with('/')
        || path.split('/').any(|part| {
            let stem = part
                .split('.')
                .next()
                .unwrap_or_default()
                .to_ascii_uppercase();
            part.is_empty()
                || matches!(part, "." | "..")
                || part.ends_with(['.', ' '])
                || part
                    .chars()
                    .any(|character| character.is_control() || "<>\"|?*".contains(character))
                || matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                || (stem.len() == 4
                    && (stem.starts_with("COM") || stem.starts_with("LPT"))
                    && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        })
        || !Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(format!("unsafe resource path {path:?}"));
    }
    Ok(())
}

pub fn prepare(root: &Path, profile: &str, offline: bool) -> Result<()> {
    let output = profile_path(root, profile)?;
    let resources = root.join("src-tauri/resources");
    let native: NativeManifest = read_json(&resources.join("native-manifest.json"))?;
    for file in &native.files {
        validate_relative(&file.path)?;
        if !verified(&resources.join(&file.path), &file.sha256, None) {
            return Err(format!(
                "bundled resource is missing/corrupt: {}",
                file.path
            ));
        }
    }
    let cache = root.join("src-tauri/target/native-cache");
    fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
    let runtime_archive = cache.join("onnxruntime-win-x64-1.26.0.zip");
    let runtime_directory = resources.join("onnxruntime");
    if !native.runtime.entries.iter().all(|entry| {
        verified(
            &runtime_directory.join(&entry.file),
            &entry.sha256,
            Some(entry.bytes),
        )
    }) {
        download(
            &native.runtime.archive_url,
            &runtime_archive,
            &native.runtime.archive_sha256,
            Some(native.runtime.archive_bytes),
            offline,
        )?;
        for entry in &native.runtime.entries {
            validate_relative(&entry.file)?;
            extract(
                &runtime_archive,
                &entry.entry,
                &runtime_directory.join(&entry.file),
                &entry.sha256,
                entry.bytes,
            )?;
        }
    }
    let dictionary: Dictionary = read_json(&resources.join("transliteration/manifest.json"))?;
    let dictionary_path = resources.join("transliteration/system.dic");
    if !verified(
        &dictionary_path,
        &dictionary.dictionary_sha256,
        Some(dictionary.dictionary_bytes),
    ) {
        let archive = cache.join("sudachi-full.zip");
        download(
            &dictionary.archive_url,
            &archive,
            &dictionary.archive_sha256,
            Some(dictionary.archive_bytes),
            offline,
        )?;
        extract(
            &archive,
            &dictionary.dictionary_entry,
            &dictionary_path,
            &dictionary.dictionary_sha256,
            dictionary.dictionary_bytes,
        )?;
    }
    let ocr: OcrManifest = read_json(&resources.join("ocr/models.json"))?;
    for model in &ocr.models {
        validate_relative(&model.file)?;
        download(
            &model.url,
            &resources.join("ocr").join(&model.file),
            &model.sha256,
            None,
            offline,
        )?;
    }
    fs::create_dir_all(&output).map_err(|e| e.to_string())?;
    msvc::prepare(root, &output)?;
    for name in ["onnxruntime.dll", "onnxruntime_providers_shared.dll"] {
        copy_file(&runtime_directory.join(name), &output.join(name))?;
    }
    copy_file(
        &resources.join("openvr/openvr_api.dll"),
        &output.join("openvr_api.dll"),
    )?;
    for (source, target) in [
        ("openvr/LICENSE", "openvr-LICENSE"),
        ("onnxruntime/LICENSE", "onnxruntime-LICENSE"),
        (
            "onnxruntime/ThirdPartyNotices.txt",
            "onnxruntime-ThirdPartyNotices.txt",
        ),
        ("fonts/OFL.txt", "NotoSans-OFL.txt"),
        ("ocr/LICENSE", "RapidOCR-LICENSE"),
        ("transliteration/LICENSE-2.0.txt", "Sudachi-LICENSE.txt"),
        ("transliteration/LEGAL", "Sudachi-LEGAL"),
    ] {
        copy_file(
            &resources.join(source),
            &output.join("licenses").join(target),
        )?;
    }
    copy_file(
        &root.join("src-tauri/crates/vrct-core/assets/silero/NOTICE.md"),
        &output.join("licenses/Silero-NOTICE.md"),
    )?;
    for name in ["transliteration", "fonts", "ocr"] {
        copy_tree(&resources.join(name), &output.join("resources").join(name))?;
    }
    copy_file(
        &resources.join("native-manifest.json"),
        &output.join("resources/native-manifest.json"),
    )?;
    println!("Native resources verified and staged for {profile}");
    Ok(())
}

fn copy_file(source: &Path, destination: &Path) -> Result<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::copy(source, destination).map_err(|e| format!("{}: {e}", source.display()))?;
    Ok(())
}

fn walk(directory: &Path, prefix: &str, out: &mut BTreeMap<String, PathBuf>) -> Result<()> {
    for entry in fs::read_dir(directory).map_err(|e| format!("{}: {e}", directory.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_symlink() {
            return Err("symlinks are not accepted in packaged resources".into());
        }
        let filename = entry
            .file_name()
            .into_string()
            .map_err(|_| "resource filename is not UTF-8")?;
        let name = if prefix.is_empty() {
            filename
        } else {
            format!("{prefix}/{filename}")
        };
        if kind.is_dir() {
            walk(&entry.path(), &name, out)?;
        } else if kind.is_file() {
            if name.ends_with(".ps1") {
                continue;
            }
            validate_relative(&name)?;
            out.insert(name, entry.path());
        }
    }
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    let mut files = BTreeMap::new();
    walk(source, "", &mut files)?;
    for (name, path) in files {
        copy_file(&path, &destination.join(name))?;
    }
    Ok(())
}

pub fn sync_version(root: &Path) -> Result<()> {
    let package: serde_json::Value = read_json(&root.join("package.json"))?;
    let version = package["version"]
        .as_str()
        .ok_or("package.json has no version")?;
    let config_path = root.join("src-tauri/tauri.conf.json");
    let mut config: serde_json::Value = read_json(&config_path)?;
    if config["version"] == version {
        return Ok(());
    }
    config["version"] = serde_json::Value::String(version.into());
    let bytes = format!(
        "{}\n",
        serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?
    );
    let temporary = Temporary(config_path.with_extension(format!("{}.partial", random()?)));
    fs::write(&temporary.0, bytes).map_err(|e| e.to_string())?;
    publish_file(&temporary.0, &config_path)
}

fn forbidden_name(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    name == "chatbox_yolox_tiny.onnx"
        || name == "vrct-sidecar.exe"
        || name.ends_with(".py")
        || name.ends_with(".pyc")
        || name.ends_with(".pyd")
        || (name.starts_with("python") && name.ends_with(".dll"))
}

fn required_files() -> &'static [&'static str] {
    &[
        "VRCT.exe",
        "openvr_api.dll",
        "onnxruntime.dll",
        "onnxruntime_providers_shared.dll",
        "msvcp140.dll",
        "msvcp140_1.dll",
        "vcruntime140.dll",
        "vcruntime140_1.dll",
        "resources/msvc/manifest.json",
        "licenses/MSVC-NOTICE.md",
        "licenses/MSVC-Redist.txt",
        "licenses/openvr-LICENSE",
        "licenses/onnxruntime-LICENSE",
        "licenses/onnxruntime-ThirdPartyNotices.txt",
        "licenses/NotoSans-OFL.txt",
        "licenses/RapidOCR-LICENSE",
        "licenses/Sudachi-LICENSE.txt",
        "licenses/Sudachi-LEGAL",
        "licenses/Silero-NOTICE.md",
        "resources/native-manifest.json",
        "resources/transliteration/system.dic",
        "resources/transliteration/manifest.json",
        "resources/fonts/NotoSansJP-Regular.ttf",
        "resources/ocr/models.json",
        "LICENSE",
        "NOTICE.md",
    ]
}

pub fn package(root: &Path, profile: &str, output: &Path) -> Result<PathBuf> {
    // Revalidate authoritative source pins before collecting the staged tree.
    // Offline mode makes packaging deterministic and catches missing resources.
    prepare(root, profile, true)?;
    let directory = profile_path(root, profile)?;
    let config: serde_json::Value = read_json(&root.join("package.json"))?;
    let version = config["version"]
        .as_str()
        .ok_or("package.json has no version")?;
    let mut files = BTreeMap::new();
    for name in [
        "VRCT.exe",
        "openvr_api.dll",
        "onnxruntime.dll",
        "onnxruntime_providers_shared.dll",
    ]
    .into_iter()
    .chain(msvc::DLLS)
    {
        files.insert(name.into(), directory.join(name));
    }
    walk(&directory.join("resources"), "resources", &mut files)?;
    walk(&directory.join("licenses"), "licenses", &mut files)?;
    for name in ["LICENSE", "NOTICE.md"] {
        files.insert(name.into(), root.join(name));
    }
    for name in required_files() {
        if !files.contains_key(*name) {
            return Err(format!(
                "native package is missing {name}; run prepare first"
            ));
        }
    }
    let protected = protected_fingerprint(Some(root))?;
    let mut hashes = BTreeMap::new();
    for (name, path) in &files {
        if forbidden_name(name) {
            return Err(format!("forbidden legacy/protected resource: {name}"));
        }
        let hash = file_checksum(path)?;
        if protected.bytes == hash.bytes && protected.sha256 == hash.sha256 {
            return Err(format!("protected detector content in {name}"));
        }
        if name.to_ascii_lowercase().ends_with(".exe")
            && contains_protected(File::open(path).map_err(|e| e.to_string())?, &protected)?
        {
            return Err(format!("protected detector is embedded in {name}"));
        }
        hashes.insert(name.clone(), hash);
    }
    let manifest = PackageManifest {
        format: 1,
        version: version.into(),
        files: hashes,
    };
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let temporary = Temporary(output.with_extension(format!("{}.partial", random()?)));
    let mut zip = ZipWriter::new(File::create(&temporary.0).map_err(|e| e.to_string())?);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .large_file(true);
    for (name, path) in files {
        zip.start_file(name, options).map_err(|e| e.to_string())?;
        std::io::copy(&mut File::open(path).map_err(|e| e.to_string())?, &mut zip)
            .map_err(|e| e.to_string())?;
    }
    zip.start_file("native-package-manifest.json", options)
        .map_err(|e| e.to_string())?;
    zip.write_all(
        serde_json::to_string_pretty(&manifest)
            .map_err(|e| e.to_string())?
            .as_bytes(),
    )
    .map_err(|e| e.to_string())?;
    let file = zip.finish().map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    verify_zip(&temporary.0)?;
    publish_file(&temporary.0, output)?;
    Ok(output.to_owned())
}

pub fn verify_zip(path: &Path) -> Result<()> {
    let mut zip =
        ZipArchive::new(File::open(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let manifest: PackageManifest = {
        let entry = zip
            .by_name("native-package-manifest.json")
            .map_err(|_| "ZIP lacks native integrity manifest")?;
        if entry.size() > 4 * 1024 * 1024 {
            return Err("package manifest exceeds size limit".into());
        }
        serde_json::from_reader(entry).map_err(|e| format!("invalid package manifest: {e}"))?
    };
    if manifest.format != 1 || manifest.version.is_empty() {
        return Err("unsupported native package manifest".into());
    }
    for name in required_files() {
        if !manifest.files.contains_key(*name) {
            return Err(format!("ZIP is missing native resource {name}"));
        }
    }
    let fingerprint = protected_fingerprint(None)?;
    let mut seen = HashSet::new();
    for index in 0..zip.len() {
        let entry = zip.by_index(index).map_err(|e| e.to_string())?;
        let name = entry.name().to_owned();
        validate_relative(&name)?;
        if !seen.insert(name.to_ascii_lowercase()) {
            return Err(format!("duplicate ZIP entry {name}"));
        }
        if forbidden_name(&name) || name.starts_with("_internal/") {
            return Err(format!("forbidden legacy/protected ZIP entry {name}"));
        }
        if name == "native-package-manifest.json" {
            continue;
        }
        let expected = manifest
            .files
            .get(&name)
            .ok_or_else(|| format!("unlisted ZIP entry {name}"))?;
        if expected.bytes == fingerprint.bytes && expected.sha256 == fingerprint.sha256 {
            return Err(format!("protected detector content in ZIP payload {name}"));
        }
        if checksum(entry)? != *expected {
            return Err(format!("ZIP checksum/length mismatch for {name}"));
        }
    }
    if seen.len() != manifest.files.len() + 1 {
        return Err("ZIP is missing one or more manifest entries".into());
    }
    verify_resource_pins(&mut zip, &manifest.files)?;
    for name in manifest
        .files
        .keys()
        .filter(|name| name.to_ascii_lowercase().ends_with(".exe"))
    {
        let entry = zip.by_name(name).map_err(|e| e.to_string())?;
        if contains_protected(entry, &fingerprint)? {
            return Err(format!(
                "protected detector is embedded in ZIP payload {name}"
            ));
        }
    }
    Ok(())
}

fn archive_json<T: serde::de::DeserializeOwned>(
    zip: &mut ZipArchive<File>,
    name: &str,
) -> Result<T> {
    let entry = zip
        .by_name(name)
        .map_err(|_| format!("ZIP is missing {name}"))?;
    if entry.size() > 4 * 1024 * 1024 {
        return Err("resource manifest exceeds size limit".into());
    }
    serde_json::from_reader(entry).map_err(|e| format!("invalid resource manifest {name}: {e}"))
}

fn verify_resource_pins(
    zip: &mut ZipArchive<File>,
    files: &BTreeMap<String, PackageFile>,
) -> Result<()> {
    let native: NativeManifest = archive_json(zip, "resources/native-manifest.json")?;
    let check = |name: &str, hash: &str, size: Option<u64>| -> Result<()> {
        if !files
            .get(name)
            .is_some_and(|file| file.sha256 == hash && size.is_none_or(|size| file.bytes == size))
        {
            return Err(format!("ZIP resource differs from provider pin: {name}"));
        }
        Ok(())
    };
    for file in &native.files {
        validate_relative(&file.path)?;
        let name = if file.path == "openvr/openvr_api.dll" {
            "openvr_api.dll".into()
        } else {
            format!("resources/{}", file.path)
        };
        check(&name, &file.sha256, None)?;
    }
    for entry in &native.runtime.entries {
        let name = match entry.file.as_str() {
            "LICENSE" => "licenses/onnxruntime-LICENSE",
            "ThirdPartyNotices.txt" => "licenses/onnxruntime-ThirdPartyNotices.txt",
            "onnxruntime.dll" => "onnxruntime.dll",
            "onnxruntime_providers_shared.dll" => "onnxruntime_providers_shared.dll",
            _ => return Err("unexpected runtime file in resource manifest".into()),
        };
        check(name, &entry.sha256, Some(entry.bytes))?;
    }
    let dictionary: Dictionary = archive_json(zip, "resources/transliteration/manifest.json")?;
    check(
        "resources/transliteration/system.dic",
        &dictionary.dictionary_sha256,
        Some(dictionary.dictionary_bytes),
    )?;
    let ocr: OcrManifest = archive_json(zip, "resources/ocr/models.json")?;
    for model in &ocr.models {
        validate_relative(&model.file)?;
        check(
            &format!("resources/ocr/{}", model.file),
            &model.sha256,
            None,
        )?;
    }
    let crt: msvc::Manifest = archive_json(zip, "resources/msvc/manifest.json")?;
    msvc::validate_manifest(&crt)?;
    for (name, file) in &crt.files {
        check(name, &file.sha256, Some(file.bytes))?;
        let mut entry = zip.by_name(name).map_err(|e| e.to_string())?;
        if entry.size() > 16 * 1024 * 1024 {
            return Err("MSVC runtime DLL exceeds size limit".into());
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        msvc::validate_dll(&bytes)?;
    }
    check(
        "licenses/MSVC-Redist.txt",
        &crt.redist.sha256,
        Some(crt.redist.bytes),
    )?;
    check(
        "licenses/MSVC-NOTICE.md",
        &crt.notice.sha256,
        Some(crt.notice.bytes),
    )?;
    Ok(())
}

#[cfg(test)]
mod protected_scan_tests {
    use super::*;

    struct ShortReads<'a> {
        bytes: &'a [u8],
        maximum: usize,
    }
    impl Read for ShortReads<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let count = buffer.len().min(self.maximum).min(self.bytes.len());
            buffer[..count].copy_from_slice(&self.bytes[..count]);
            self.bytes = &self.bytes[count..];
            Ok(count)
        }
    }

    fn fingerprint(model: &[u8]) -> ProtectedFingerprint {
        ProtectedFingerprint {
            bytes: model.len() as u64,
            sha256: hex::encode(Sha256::digest(model)),
            prefix_hex: hex::encode(&model[..model.len().min(128)]),
        }
    }

    #[test]
    fn detects_complete_object_with_short_reads_and_multiple_candidates() {
        let model: Vec<u8> = (0..1003).map(|i| (i % 251) as u8).collect();
        let signature = fingerprint(&model);
        let mut payload = b"MZ prefix".to_vec();
        let mut decoy = model.clone();
        decoy[500] ^= 1;
        payload.extend_from_slice(&decoy);
        payload.extend_from_slice(&model);
        payload.extend_from_slice(b"suffix");
        for maximum in [1, 7, 127, 128, 129, 100_000] {
            assert!(contains_protected(
                ShortReads {
                    bytes: &payload,
                    maximum
                },
                &signature
            )
            .unwrap());
        }
    }

    #[test]
    fn rejects_incomplete_or_changed_objects_and_invalid_fingerprints() {
        let mut model: Vec<u8> = (0..1003).map(|i| (i % 251) as u8).collect();
        let mut signature = fingerprint(&model);
        assert!(!contains_protected(&model[..1002], &signature).unwrap());
        assert!(!contains_protected(&[][..], &signature).unwrap());
        model[500] ^= 1;
        assert!(!contains_protected(model.as_slice(), &signature).unwrap());
        signature.prefix_hex = "".into();
        assert!(contains_protected(model.as_slice(), &signature).is_err());
    }

    #[test]
    fn windows_paths_reject_devices_aliases_and_traversal() {
        for path in [
            "",
            "../escape",
            "resources/../file",
            "C:/file",
            "resources\\file",
            "/file",
            "CON.onnx",
            "dir/LPT9.dat",
            "dir/NUL",
            "file.",
            "file ",
            "dir/file\0",
            "file?",
            "dir//file",
        ] {
            assert!(validate_relative(path).is_err(), "accepted {path:?}");
        }
        assert!(validate_relative("resources/ocr/ch_rec.onnx").is_ok());
        assert!(validate_relative("resources/fonts/NotoSansJP-Regular.ttf").is_ok());
    }
}
