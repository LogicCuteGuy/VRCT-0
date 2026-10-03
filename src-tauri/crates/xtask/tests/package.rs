use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::json;
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let base = std::path::absolute(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/xtask-tests"),
        )
        .unwrap();
        let mut bytes = [0u8; 12];
        getrandom::fill(&mut bytes).unwrap();
        let path = base.join(hex::encode(bytes));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn write(&self, name: &str, bytes: &[u8]) {
        write(&self.0.join(name), bytes);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let base = std::path::absolute(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/xtask-tests"),
        )
        .unwrap();
        assert!(self.0.starts_with(base) && self.0.file_name().is_some());
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}
fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn pe_dll(machine: u16) -> Vec<u8> {
    let mut bytes = vec![0u8; 512];
    bytes[..2].copy_from_slice(b"MZ");
    bytes[60..64].copy_from_slice(&128u32.to_le_bytes());
    bytes[128..132].copy_from_slice(b"PE\0\0");
    bytes[132..134].copy_from_slice(&machine.to_le_bytes());
    bytes[150..152].copy_from_slice(&0x2000u16.to_le_bytes());
    bytes[152..154].copy_from_slice(&0x20bu16.to_le_bytes());
    bytes
}

fn vs_fixture(fixture: &Fixture) -> PathBuf {
    let directory = fixture.0.join("vs");
    for name in [
        "msvcp140.dll",
        "msvcp140_1.dll",
        "vcruntime140.dll",
        "vcruntime140_1.dll",
    ] {
        write(
            &directory
                .join("VC/Redist/MSVC/14.44.35211/x64/Microsoft.VC143.CRT")
                .join(name),
            &pe_dll(0x8664),
        );
    }
    write(
        &directory.join("Licenses/1033/Redist.txt"),
        b"Microsoft fixture redistribution list",
    );
    fixture.write(
        "src-tauri/resources/msvc/NOTICE.md",
        b"Microsoft CRT notice fixture",
    );
    directory
}

fn zip(path: &Path, files: &[(&str, &[u8])]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut writer = ZipWriter::new(File::create(path).unwrap());
    for (name, bytes) in files {
        writer
            .start_file(*name, SimpleFileOptions::default())
            .unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap();
}

fn resource_fixture() -> Fixture {
    let fixture = Fixture::new();
    fixture.write("package.json", br#"{"version":"3.5.1-beta.1"}"#);
    fixture.write(
        "src-tauri/tauri.conf.json",
        br#"{"version":"0.0.0","bundle":{"externalBin":[]}}"#,
    );
    fixture.write("LICENSE", b"application license");
    fixture.write("NOTICE.md", b"third party notices");
    fixture.write(
        "src-tauri/target/release/VRCT.exe",
        b"MZ fixture executable",
    );
    let directory = fixture.0.join("src-tauri/resources");
    let dll = b"fixture-ort-dll";
    let shared = b"fixture-provider-shared";
    let license = b"fixture-ort-license";
    let notices = b"fixture-third-party-notices";
    let runtime_archive = fixture
        .0
        .join("src-tauri/target/native-cache/onnxruntime-win-x64-1.26.0.zip");
    zip(
        &runtime_archive,
        &[
            ("runtime/lib/onnxruntime.dll", dll),
            ("runtime/lib/onnxruntime_providers_shared.dll", shared),
            ("runtime/LICENSE", license),
            ("runtime/ThirdPartyNotices.txt", notices),
        ],
    );
    let native = json!({
        "runtime": {
            "archive_url":"https://not-needed.invalid/runtime.zip", "archive_bytes":fs::metadata(&runtime_archive).unwrap().len(), "archive_sha256":digest(&fs::read(&runtime_archive).unwrap()),
            "entries":[
                {"entry":"runtime/lib/onnxruntime.dll", "file":"onnxruntime.dll", "bytes":dll.len(), "sha256":digest(dll)},
                {"entry":"runtime/lib/onnxruntime_providers_shared.dll", "file":"onnxruntime_providers_shared.dll", "bytes":shared.len(), "sha256":digest(shared)},
                {"entry":"runtime/LICENSE", "file":"LICENSE", "bytes":license.len(), "sha256":digest(license)},
                {"entry":"runtime/ThirdPartyNotices.txt", "file":"ThirdPartyNotices.txt", "bytes":notices.len(), "sha256":digest(notices)}
            ]
        },
        "files":[{"path":"openvr/openvr_api.dll", "sha256":digest(b"fixture-openvr")}]
    });
    write(
        &directory.join("native-manifest.json"),
        native.to_string().as_bytes(),
    );
    let dictionary = b"native-dictionary-fixture";
    let dictionary_archive = fixture
        .0
        .join("src-tauri/target/native-cache/sudachi-full.zip");
    zip(
        &dictionary_archive,
        &[("dictionary/system_full.dic", dictionary)],
    );
    let dictionary_manifest = json!({"archive_url":"https://not-needed.invalid/dictionary.zip", "archive_bytes":fs::metadata(&dictionary_archive).unwrap().len(), "archive_sha256":digest(&fs::read(dictionary_archive).unwrap()), "dictionary_entry":"dictionary/system_full.dic", "dictionary_bytes":dictionary.len(), "dictionary_sha256":digest(dictionary)});
    write(
        &directory.join("transliteration/manifest.json"),
        dictionary_manifest.to_string().as_bytes(),
    );
    for name in [
        "LEGAL",
        "LICENSE-2.0.txt",
        "sudachi.json",
        "char.def",
        "rewrite.def",
        "unk.def",
    ] {
        write(
            &directory.join("transliteration").join(name),
            b"fixture dictionary config/license",
        );
    }
    for font in ["JP", "KR", "SC", "TC"] {
        write(
            &directory.join(format!("fonts/NotoSans{font}-Regular.ttf")),
            b"fixture font",
        );
    }
    write(&directory.join("fonts/OFL.txt"), b"font license");
    write(&directory.join("openvr/openvr_api.dll"), b"fixture-openvr");
    write(&directory.join("openvr/LICENSE"), b"OpenVR license");
    write(&directory.join("ocr/LICENSE"), b"OCR license");
    let ocr = b"native ocr fixture";
    write(&directory.join("ocr/detector.onnx"), ocr);
    write(&directory.join("ocr/models.json"), json!({"models":[{"file":"detector.onnx", "url":"https://not-needed.invalid/detector.onnx", "sha256":digest(ocr)}]}).to_string().as_bytes());
    fixture.write(
        "src-tauri/crates/vrct-core/assets/silero/NOTICE.md",
        b"Silero license",
    );
    xtask::stage_msvc_from_visual_studio(&fixture.0, &vs_fixture(&fixture)).unwrap();
    fixture
}

#[test]
fn prepare_offline_verifies_extracts_and_stages_exact_native_resources() {
    let fixture = resource_fixture();
    xtask::prepare(&fixture.0, "release", true).unwrap();
    let staged = fixture.0.join("src-tauri/target/release");
    assert_eq!(
        fs::read(staged.join("onnxruntime.dll")).unwrap(),
        b"fixture-ort-dll"
    );
    assert_eq!(
        fs::read(staged.join("resources/transliteration/system.dic")).unwrap(),
        b"native-dictionary-fixture"
    );
    assert!(staged.join("resources/ocr/detector.onnx").is_file());
    assert!(staged.join("licenses/openvr-LICENSE").is_file());
    assert!(staged
        .join("licenses/onnxruntime-ThirdPartyNotices.txt")
        .is_file());
    assert!(staged.join("licenses/Silero-NOTICE.md").is_file());
    assert!(staged.join("vcruntime140_1.dll").is_file());
    assert!(staged.join("resources/msvc/manifest.json").is_file());
    assert!(staged.join("licenses/MSVC-Redist.txt").is_file());
    xtask::prepare(&fixture.0, "release", true).unwrap();
    assert!(xtask::prepare(&fixture.0, "invalid-profile", true).is_err());
}

#[test]
fn crt_staging_validates_all_source_dlls_before_replacing_prior_assets() {
    let fixture = resource_fixture();
    let source = fixture.0.join("vs");
    let crt = source.join("VC/Redist/MSVC/14.44.35211/x64/Microsoft.VC143.CRT");
    let cached = fixture.0.join("src-tauri/resources/msvc/msvcp140.dll");
    let original = fs::read(&cached).unwrap();
    write(&crt.join("vcruntime140_1.dll"), &pe_dll(0x14c));
    assert!(xtask::stage_msvc_from_visual_studio(&fixture.0, &source)
        .unwrap_err()
        .contains("x64"));
    assert_eq!(fs::read(&cached).unwrap(), original);
    fs::remove_file(crt.join("vcruntime140_1.dll")).unwrap();
    assert!(xtask::stage_msvc_from_visual_studio(&fixture.0, &source)
        .unwrap_err()
        .contains("absent"));
    assert_eq!(fs::read(&cached).unwrap(), original);
}

#[test]
fn corrupted_resource_never_replaces_existing_dll_and_offline_reports_failure() {
    let fixture = resource_fixture();
    xtask::prepare(&fixture.0, "release", true).unwrap();
    fixture.write("src-tauri/resources/openvr/openvr_api.dll", b"tampered DLL");
    let error = xtask::prepare(&fixture.0, "release", true).unwrap_err();
    assert!(error.contains("missing/corrupt"));
    assert_eq!(
        fs::read(fixture.0.join("src-tauri/target/release/openvr_api.dll")).unwrap(),
        b"fixture-openvr"
    );
    fixture.write(
        "src-tauri/resources/openvr/openvr_api.dll",
        b"fixture-openvr",
    );
    fixture.write("src-tauri/resources/ocr/detector.onnx", b"corrupt OCR");
    assert!(xtask::prepare(&fixture.0, "release", true)
        .unwrap_err()
        .contains("offline"));
}

#[test]
fn native_zip_has_installer_root_and_every_payload_hash_and_preserves_existing_output_on_failure() {
    let fixture = resource_fixture();
    xtask::prepare(&fixture.0, "release", true).unwrap();
    let output = fixture.0.join("VRCT.zip");
    xtask::package(&fixture.0, "release", &output).unwrap();
    xtask::verify_zip(&output).unwrap();
    let original = fs::read(&output).unwrap();
    let mut archive = ZipArchive::new(File::open(&output).unwrap()).unwrap();
    assert!(archive.by_name("VRCT.exe").is_ok());
    assert!(archive.by_name("onnxruntime.dll").is_ok());
    assert!(archive.by_name("native-package-manifest.json").is_ok());
    assert!(archive
        .by_name("resources/fonts/NotoSansTC-Regular.ttf")
        .is_ok());
    assert!(archive.by_name("VRCT-sidecar.exe").is_err());
    drop(archive);
    fixture.write(
        "src-tauri/target/release/resources/legacy.py",
        b"print('must not ship')",
    );
    assert!(xtask::package(&fixture.0, "release", &output)
        .unwrap_err()
        .contains("forbidden"));
    assert_eq!(fs::read(output).unwrap(), original);
}

#[test]
fn renamed_protected_detector_content_is_rejected() {
    let fixture = resource_fixture();
    xtask::prepare(&fixture.0, "release", true).unwrap();
    fixture.write(
        "src-python/models/ocr/onnx/chatbox_yolox_tiny.onnx",
        b"protected fixture",
    );
    fixture.write(
        "src-tauri/target/release/resources/renamed-detector.onnx",
        b"protected fixture",
    );
    assert!(
        xtask::package(&fixture.0, "release", &fixture.0.join("VRCT.zip"))
            .unwrap_err()
            .contains("protected detector content")
    );
}

#[test]
fn embedded_protected_detector_in_executable_preserves_existing_package() {
    let fixture = resource_fixture();
    xtask::prepare(&fixture.0, "release", true).unwrap();
    let output = fixture.0.join("VRCT.zip");
    xtask::package(&fixture.0, "release", &output).unwrap();
    let original = fs::read(&output).unwrap();
    let mut state = 0x3141_5926u32;
    let model: Vec<u8> = (0..262_243)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect();
    fixture.write("src-python/models/ocr/onnx/chatbox_yolox_tiny.onnx", &model);
    // The model's prefix crosses the scanner's 128 KiB read boundary.
    let mut executable = vec![0u8; 128 * 1024 - 63];
    executable[..2].copy_from_slice(b"MZ");
    executable.extend_from_slice(&model);
    executable.extend_from_slice(b"executable suffix");
    fixture.write("src-tauri/target/release/VRCT.exe", &executable);
    assert!(xtask::package(&fixture.0, "release", &output)
        .unwrap_err()
        .contains("protected detector is embedded"));
    assert_eq!(fs::read(&output).unwrap(), original);

    // A matching prefix without the complete protected object isn't forbidden.
    let middle = 128 * 1024 + 100;
    executable[middle] ^= 1;
    fixture.write("src-tauri/target/release/VRCT.exe", &executable);
    xtask::package(&fixture.0, "release", &output).unwrap();
}

#[test]
fn version_sync_preserves_config_fields_and_never_edits_python() {
    let fixture = resource_fixture();
    fixture.write("src-python/config.py", b"do not modify Python");
    xtask::sync_version(&fixture.0).unwrap();
    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.0.join("src-tauri/tauri.conf.json")).unwrap())
            .unwrap();
    assert_eq!(config["version"], "3.5.1-beta.1");
    assert_eq!(config["bundle"]["externalBin"], json!([]));
    assert_eq!(
        fs::read(fixture.0.join("src-python/config.py")).unwrap(),
        b"do not modify Python"
    );
}

#[test]
fn verifier_rejects_payload_corruption_and_unlisted_legacy_files() {
    use std::io::Read;
    let fixture = resource_fixture();
    xtask::prepare(&fixture.0, "release", true).unwrap();
    let original = fixture.0.join("VRCT.zip");
    xtask::package(&fixture.0, "release", &original).unwrap();
    let mut archive = ZipArchive::new(File::open(&original).unwrap()).unwrap();
    let mut files = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).unwrap();
        let name = entry.name().to_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        files.push((name, bytes));
    }
    let corrupt = fixture.0.join("corrupt.zip");
    let mut altered = files.clone();
    let (_, dll) = altered
        .iter_mut()
        .find(|(name, _)| name == "onnxruntime.dll")
        .unwrap();
    dll[0] ^= 1;
    zip(
        &corrupt,
        &altered
            .iter()
            .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
            .collect::<Vec<_>>(),
    );
    assert!(xtask::verify_zip(&corrupt)
        .unwrap_err()
        .contains("checksum/length mismatch"));

    let legacy = fixture.0.join("legacy.zip");
    files.push(("resources/legacy.py".into(), b"must not ship".to_vec()));
    zip(
        &legacy,
        &files
            .iter()
            .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
            .collect::<Vec<_>>(),
    );
    assert!(xtask::verify_zip(&legacy)
        .unwrap_err()
        .contains("forbidden"));
}

#[test]
fn verifier_rejects_wrong_architecture_even_when_crt_and_package_hashes_are_updated() {
    use std::io::Read;
    let fixture = resource_fixture();
    let original = fixture.0.join("VRCT.zip");
    xtask::package(&fixture.0, "release", &original).unwrap();
    let mut archive = ZipArchive::new(File::open(&original).unwrap()).unwrap();
    let mut files = std::collections::BTreeMap::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).unwrap();
        let name = entry.name().to_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        files.insert(name, bytes);
    }
    let wrong = pe_dll(0x14c);
    files.insert("msvcp140.dll".into(), wrong.clone());
    let mut crt: serde_json::Value =
        serde_json::from_slice(&files["resources/msvc/manifest.json"]).unwrap();
    crt["files"]["msvcp140.dll"] = json!({"bytes":wrong.len(),"sha256":digest(&wrong)});
    let crt_bytes = serde_json::to_vec(&crt).unwrap();
    files.insert("resources/msvc/manifest.json".into(), crt_bytes.clone());
    let mut package: serde_json::Value =
        serde_json::from_slice(&files["native-package-manifest.json"]).unwrap();
    package["files"]["msvcp140.dll"] = json!({"bytes":wrong.len(),"sha256":digest(&wrong)});
    package["files"]["resources/msvc/manifest.json"] =
        json!({"bytes":crt_bytes.len(),"sha256":digest(&crt_bytes)});
    files.insert(
        "native-package-manifest.json".into(),
        serde_json::to_vec(&package).unwrap(),
    );
    let output = fixture.0.join("wrong-architecture.zip");
    zip(
        &output,
        &files
            .iter()
            .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
            .collect::<Vec<_>>(),
    );
    assert!(xtask::verify_zip(&output).unwrap_err().contains("x64"));
}
