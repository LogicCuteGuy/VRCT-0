mod backend;

use tauri::Manager;
use std::fs::{create_dir_all, OpenOptions};
use std::io::{Error, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn startup_log_path(executable_path: &Path) -> PathBuf {
    executable_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("logs")
        .join("startup.log")
}

fn startup_log(message: &str) {
    let Ok(executable_path) = std::env::current_exe() else {
        return;
    };
    let log_path = startup_log_path(&executable_path);
    let Some(log_directory) = log_path.parent() else {
        return;
    };
    if create_dir_all(log_directory).is_err() {
        return;
    }
    let Ok(mut log_file) = OpenOptions::new().create(true).append(true).open(log_path) else {
        return;
    };
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let _ = writeln!(log_file, "[{timestamp}] {message}");
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    startup_log("VRCT startup began");
    let result = tauri::Builder::default()
        .setup(|app| {
            let main_window = app.get_webview_window("main").ok_or_else(|| {
                Error::other("main webview window was not created")
            })?;
            main_window.show()?;
            if let Err(error) = main_window.set_focus() {
                startup_log(&format!("Main window focus failed: {error}"));
            }
            startup_log("Main window is ready");

            app.manage(backend::Backend::new(app.handle()).map_err(Error::other)?);

            #[cfg(debug_assertions)]
            { main_window.open_devtools(); }

            Ok(())
        })
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            get_font_list,
            download_zip_asset,
            backend_start,
            backend_request
        ])
        .run(tauri::generate_context!());
    match result {
        Ok(()) => startup_log("VRCT event loop ended"),
        Err(error) => {
            startup_log(&format!("VRCT startup failed: {error}"));
            panic!("error while running tauri application: {error}");
        }
    }
}


/// Start the backend. Call once the UI is listening for `backend-response`.
#[tauri::command]
async fn backend_start(
    app: tauri::AppHandle,
    backend: tauri::State<'_, backend::Backend>,
) -> Result<(), String> {
    backend.start(&app)
}

/// Same contract as writing `{endpoint, data}` to the sidecar's stdin: returns
/// immediately and the response arrives as a `backend-response` event. `data`
/// is the UI's base64-encoded JSON payload.
#[tauri::command]
async fn backend_request(
    backend: tauri::State<'_, backend::Backend>,
    endpoint: String,
    data: Option<String>,
) -> Result<(), String> {
    backend.request(endpoint, data);
    Ok(())
}

use font_kit::{source::SystemSource};
use std::collections::HashSet;

#[tauri::command]
async fn get_font_list() -> Vec<String> {
    let source = SystemSource::new();
    let mut font_families = HashSet::new();

    if let Ok(fonts) = source.all_fonts() {
        for font in fonts {
            if let Ok(info) = font.load() {
                font_families.insert(info.family_name().to_string());
            }
        }
    }

    font_families.into_iter().collect()
}


use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;

#[tauri::command]
async fn download_zip_asset(url: String) -> Result<String, String> {
    use reqwest;

    let client = reqwest::Client::new();
    let resp = client.get(&url)
        .header("Accept", "application/octet-stream")
        .send()
        .await.map_err(|e| format!("Request error: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP error: {}", resp.status()));
    }

    let bytes = resp.bytes().await.map_err(|e| format!("Reading bytes error: {}", e))?;

    Ok(BASE64.encode(&bytes))
}

#[cfg(test)]
mod tests {
    use super::startup_log_path;
    use std::path::Path;

    #[test]
    fn startup_log_is_stored_next_to_the_application() {
        assert_eq!(
            startup_log_path(Path::new(r"C:\VRCT\VRCT.exe")),
            Path::new(r"C:\VRCT\logs\startup.log")
        );
    }
}
