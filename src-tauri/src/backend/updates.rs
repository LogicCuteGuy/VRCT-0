use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use vrct_core::config::ConfigReplica;
use vrct_core::protocol::Response;
use vrct_core::router::{ResponseSink, Router};
use vrct_core::updater::install::{self, InstallOptions, InstallOutcome};
use vrct_core::updater::{Channel, Edition, UpdateSource, Updater};

/// Releases are read from this repository's GitHub Releases.
const REPO_OWNER: &str = "LogicCuteGuy";
const REPO_NAME: &str = "0-VRCT";
/// Same floor the Python updater used for the version picker.
const MIN_SUPPORTED_VERSION: &str = "3.4.3";

const DEFAULT_UI_LANGUAGE: &str = "en";

struct Deps {
    app: AppHandle,
    updater: Updater,
    replica: Arc<ConfigReplica>,
    sink: Arc<dyn ResponseSink>,
    version: String,
    update_running: AtomicBool,
}

impl Deps {
    /// Read the channel from native Settings, falling back to the running
    /// version when the property is absent.
    fn channel(&self) -> Channel {
        match self.replica.get_str("SELECTED_RELEASE_CHANNEL") {
            Some(channel) => Channel::parse(&channel),
            None if ["-beta", "-rc"].iter().any(|m| self.version.contains(m)) => Channel::Beta,
            None => Channel::Stable,
        }
    }

    fn ui_language(&self) -> String {
        self.replica
            .get_str("UI_LANGUAGE")
            .unwrap_or_else(|| DEFAULT_UI_LANGUAGE.to_string())
    }

    fn download_dir(&self) -> Result<PathBuf, String> {
        self.app
            .path()
            .app_local_data_dir()
            .map(|dir| dir.join("updates"))
            .map_err(|error| error.to_string())
    }
}

fn requested_version(data: Option<Value>) -> Option<String> {
    match data {
        Some(Value::String(version)) if !version.is_empty() => Some(version),
        _ => None,
    }
}

async fn run_update(
    deps: Arc<Deps>,
    endpoint: &'static str,
    edition: Edition,
    version: Option<String>,
) {
    // A second click must not start a download that wipes the first one's files.
    if deps.update_running.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Err(message) = try_update(&deps, edition, version).await {
        eprintln!("update failed: {message}");
        // Status 500 surfaces as the UI's error notification.
        deps.sink.emit(Response::new(
            500,
            endpoint,
            json!(format!("Update failed: {message}")),
        ));
    }
    deps.update_running.store(false, Ordering::SeqCst);
}

async fn try_update(deps: &Deps, edition: Edition, version: Option<String>) -> Result<(), String> {
    let channel = deps.channel();
    let dir = deps.download_dir()?;
    // Old installers are only clutter; the directory is ours alone.
    let _ = tokio::fs::remove_dir_all(&dir).await;

    let prepared = deps
        .updater
        .prepare_install(version.as_deref(), channel, edition, &dir, |_, _| {})
        .await
        .map_err(|error| error.to_string())?;

    let ui_language = deps.ui_language();
    let outcome = install::launch(
        &prepared.path,
        &InstallOptions {
            edition,
            ui_language: &ui_language,
            channel,
            version: version.as_deref(),
        },
    )
    .map_err(|error| error.to_string())?;

    match outcome {
        // The installer replaces files this process holds open; quit now.
        InstallOutcome::Launched => deps.app.exit(0),
        InstallOutcome::Manual(path) => {
            eprintln!(
                "update downloaded; finish installing manually: {}",
                path.display()
            );
        }
    }
    Ok(())
}

pub fn register(
    router: Router,
    app: AppHandle,
    replica: Arc<ConfigReplica>,
    sink: Arc<dyn ResponseSink>,
) -> Result<Router, String> {
    let version = app.package_info().version.to_string();
    let updater = Updater::new(
        UpdateSource::github(REPO_OWNER, REPO_NAME),
        &version,
        MIN_SUPPORTED_VERSION,
    )
    .map_err(|error| error.to_string())?;
    let deps = Arc::new(Deps {
        app,
        updater,
        replica,
        sink,
        version,
        update_running: AtomicBool::new(false),
    });

    let releases = Arc::clone(&deps);
    let info = Arc::clone(&deps);
    let cpu = Arc::clone(&deps);
    let gpu = Arc::clone(&deps);

    Ok(router
        .handle("/get/data/available_releases", move |_| {
            let deps = Arc::clone(&releases);
            async move {
                match deps.updater.list_available().await {
                    Ok(list) => (200, json!(list)),
                    Err(error) => {
                        eprintln!("listing releases failed: {error}");
                        (200, json!([]))
                    }
                }
            }
        })
        .handle("/run/software_update_info", move |_| {
            let deps = Arc::clone(&info);
            async move {
                match deps.updater.check(deps.channel()).await {
                    Ok(check) => (200, json!(check)),
                    Err(error) => {
                        eprintln!("update check failed: {error}");
                        (
                            200,
                            json!({"is_update_available": false, "new_version": null}),
                        )
                    }
                }
            }
        })
        .handle("/run/update_software", move |data| {
            let deps = Arc::clone(&cpu);
            async move {
                tokio::spawn(run_update(
                    deps,
                    "/run/update_software",
                    Edition::Cpu,
                    requested_version(data),
                ));
                (200, json!(true))
            }
        })
        .handle("/run/update_cuda_software", move |data| {
            let deps = Arc::clone(&gpu);
            async move {
                tokio::spawn(run_update(
                    deps,
                    "/run/update_cuda_software",
                    Edition::Gpu,
                    requested_version(data),
                ));
                (200, json!(true))
            }
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_non_empty_string_pins_a_version() {
        assert_eq!(
            requested_version(Some(json!("3.5.1"))),
            Some("3.5.1".into())
        );
        assert_eq!(requested_version(Some(json!(""))), None);
        assert_eq!(requested_version(Some(Value::Null)), None);
        assert_eq!(requested_version(None), None);
    }
}
