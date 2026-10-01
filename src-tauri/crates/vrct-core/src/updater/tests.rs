//! End-to-end updater tests against a throwaway local HTTP server.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};

use super::*;

type Routes = Arc<Mutex<HashMap<String, Vec<(u16, Vec<u8>)>>>>;

/// Serves each path's queued responses in order, repeating the last one.
struct TestServer {
    base: String,
    routes: Routes,
}

impl TestServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let routes: Routes = Arc::default();
        let served = Arc::clone(&routes);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut buffer = [0u8; 4096];
                let read = stream.read(&mut buffer).unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]);
                let path = request
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/")
                    .split('?')
                    .next()
                    .unwrap_or("/")
                    .to_string();
                let (status, body) = {
                    let mut routes = served.lock().unwrap();
                    match routes.get_mut(&path) {
                        Some(queue) if queue.len() > 1 => queue.remove(0),
                        Some(queue) if queue.len() == 1 => queue[0].clone(),
                        _ => (404, b"not found".to_vec()),
                    }
                };
                let head = format!(
                    "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        Self { base, routes }
    }

    fn route(&self, path: &str, responses: Vec<(u16, Vec<u8>)>) {
        self.routes.lock().unwrap().insert(path.to_string(), responses);
    }

    fn source(&self) -> UpdateSource {
        UpdateSource {
            owner: "o".into(),
            repo: "r".into(),
            api_base: self.base.clone(),
        }
    }

    fn updater(&self, current: &str) -> Updater {
        Updater::new(self.source(), current, "3.4.3").unwrap()
    }
}

const INSTALLER_NAME: &str = "VRCT_3.6.0_x64-setup.exe";
const WIN: Target = Target { os: Os::Windows, arch: Arch::X64 };

fn installer_bytes() -> Vec<u8> {
    vec![0xAB; 2 * 1024 * 1024]
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn releases_json(base: &str, with_sidecar: bool) -> Vec<u8> {
    let mut assets = vec![serde_json::json!({
        "name": INSTALLER_NAME,
        "browser_download_url": format!("{base}/dl/{INSTALLER_NAME}"),
        "size": installer_bytes().len(),
    })];
    if with_sidecar {
        assets.push(serde_json::json!({
            "name": format!("{INSTALLER_NAME}.sha256"),
            "browser_download_url": format!("{base}/dl/{INSTALLER_NAME}.sha256"),
            "size": 64,
        }));
    }
    serde_json::json!([
        {"tag_name": "v3.6.0", "prerelease": false, "draft": false, "assets": assets},
        {"tag_name": "v3.5.1", "prerelease": false, "draft": false, "assets": []},
        {"tag_name": "v3.7.0-beta.1", "prerelease": true, "draft": false, "assets": []},
    ])
    .to_string()
    .into_bytes()
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vrct-updater-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn server_with(with_sidecar: bool) -> TestServer {
    let server = TestServer::start();
    server.route(
        "/repos/o/r/releases",
        vec![(200, releases_json(&server.base, with_sidecar))],
    );
    server.route(&format!("/dl/{INSTALLER_NAME}"), vec![(200, installer_bytes())]);
    server
}

#[tokio::test]
async fn check_reports_newer_release_per_channel() {
    let server = server_with(true);
    let updater = server.updater("3.5.1");

    let stable = updater.check(Channel::Stable).await.unwrap();
    assert!(stable.is_update_available);
    assert_eq!(stable.new_version.as_deref(), Some("3.6.0"));

    let beta = updater.check(Channel::Beta).await.unwrap();
    assert_eq!(beta.new_version.as_deref(), Some("3.7.0-beta.1"));

    let up_to_date = server.updater("3.6.0").check(Channel::Stable).await.unwrap();
    assert!(!up_to_date.is_update_available);
}

#[tokio::test]
async fn list_available_returns_supported_releases_newest_first() {
    let server = server_with(true);
    let list = server.updater("3.5.1").list_available().await.unwrap();
    let versions: Vec<_> = list.iter().map(|r| r.version.as_str()).collect();
    assert_eq!(versions, ["3.7.0-beta.1", "3.6.0", "3.5.1"]);
}

#[tokio::test]
async fn http_error_from_api_is_a_network_error() {
    let server = TestServer::start();
    server.route("/repos/o/r/releases", vec![(403, b"rate limited".to_vec())]);
    let error = server.updater("3.5.1").check(Channel::Stable).await.unwrap_err();
    assert!(matches!(error, UpdateError::Network(_)));
}

#[tokio::test]
async fn verified_install_downloads_and_checks_the_sidecar_digest() {
    let server = server_with(true);
    server.route(
        &format!("/dl/{INSTALLER_NAME}.sha256"),
        vec![(200, format!("{}  {INSTALLER_NAME}\n", digest(&installer_bytes())).into_bytes())],
    );
    let dir = temp_dir("ok");
    let mut last_progress = (0, None);

    let prepared = server
        .updater("3.5.1")
        .prepare_install_for(WIN, None, Channel::Stable, Edition::Cpu, &dir, |done, total| {
            last_progress = (done, total)
        })
        .await
        .unwrap();

    assert!(prepared.checksum_verified);
    assert_eq!(prepared.version.to_string(), "3.6.0");
    assert_eq!(std::fs::read(&prepared.path).unwrap(), installer_bytes());
    assert_eq!(last_progress.0, installer_bytes().len() as u64);
    assert_eq!(last_progress.1, Some(installer_bytes().len() as u64));
}

#[tokio::test]
async fn tampered_installer_is_rejected_and_not_left_on_disk() {
    let server = server_with(true);
    server.route(
        &format!("/dl/{INSTALLER_NAME}.sha256"),
        vec![(200, digest(b"something else").into_bytes())],
    );
    let dir = temp_dir("tampered");

    let error = server
        .updater("3.5.1")
        .prepare_install_for(WIN, None, Channel::Stable, Edition::Cpu, &dir, |_, _| {})
        .await
        .unwrap_err();

    assert!(matches!(error, UpdateError::ChecksumMismatch { .. }));
    assert!(!dir.join(INSTALLER_NAME).exists());
    assert!(!dir.join(format!("{INSTALLER_NAME}.part")).exists());
}

#[tokio::test]
async fn published_but_unreadable_sidecar_aborts_instead_of_skipping_verification() {
    let server = server_with(true);
    server.route(
        &format!("/dl/{INSTALLER_NAME}.sha256"),
        vec![(200, b"<html>oops</html>".to_vec())],
    );
    let dir = temp_dir("bad-sidecar");

    let error = server
        .updater("3.5.1")
        .prepare_install_for(WIN, None, Channel::Stable, Edition::Cpu, &dir, |_, _| {})
        .await
        .unwrap_err();

    assert!(matches!(error, UpdateError::ChecksumUnavailable));
    assert!(!dir.join(INSTALLER_NAME).exists());
}

#[tokio::test]
async fn release_without_sidecar_falls_back_to_size_check_only() {
    let server = server_with(false);
    let dir = temp_dir("no-sidecar");

    let prepared = server
        .updater("3.5.1")
        .prepare_install_for(WIN, None, Channel::Stable, Edition::Cpu, &dir, |_, _| {})
        .await
        .unwrap();

    assert!(!prepared.checksum_verified);
    assert!(prepared.path.exists());
}

#[tokio::test]
async fn transient_download_failure_is_retried() {
    let server = server_with(true);
    server.route(
        &format!("/dl/{INSTALLER_NAME}.sha256"),
        vec![(200, digest(&installer_bytes()).into_bytes())],
    );
    server.route(
        &format!("/dl/{INSTALLER_NAME}"),
        vec![(503, b"busy".to_vec()), (200, installer_bytes())],
    );
    let dir = temp_dir("retry");

    let prepared = server
        .updater("3.5.1")
        .prepare_install_for(WIN, None, Channel::Stable, Edition::Cpu, &dir, |_, _| {})
        .await
        .unwrap();

    assert!(prepared.checksum_verified);
}

#[tokio::test]
async fn pinned_version_below_minimum_or_missing_is_refused() {
    let server = server_with(true);
    let updater = server.updater("3.5.1");
    let dir = temp_dir("pinned");

    let missing = updater
        .prepare_install_for(WIN, Some("9.9.9"), Channel::Stable, Edition::Cpu, &dir, |_, _| {})
        .await
        .unwrap_err();
    assert!(matches!(missing, UpdateError::UnsupportedVersion));

    // 3.5.1 exists but has no installer asset for this platform.
    let no_installer = updater
        .prepare_install_for(WIN, Some("3.5.1"), Channel::Stable, Edition::Cpu, &dir, |_, _| {})
        .await
        .unwrap_err();
    assert!(matches!(no_installer, UpdateError::NoInstaller));
}
