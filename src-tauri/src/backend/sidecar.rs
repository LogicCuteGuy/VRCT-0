use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Emitter};
use tauri_plugin_shell::process::{CommandChild, CommandEvent};
use tauri_plugin_shell::ShellExt;
use vrct_core::config::{ConfigReplica, BRIDGE_ENV};
use vrct_core::protocol::{parse_sidecar_line, sidecar_line};
use vrct_core::router::{Fallback, Router};
use vrct_core::rpc::{rpc_env_value, LineWriter, Rpc, RPC_ENV_NAME};
use vrct_core::sinks::{sinks_env_value, Sinks, SINKS_ENV_NAME};

/// The legacy Python process. Endpoints Rust has not taken over are written to
/// its stdin and its stdout lines come back through the router.
#[derive(Default)]
pub struct Sidecar {
    child: Mutex<Option<CommandChild>>,
}

impl Fallback for Sidecar {
    fn forward(&self, endpoint: &str, data: Option<&str>) -> Result<(), String> {
        let mut child = self.child.lock().unwrap();
        let child = child.as_mut().ok_or("Backend sidecar is not running")?;
        child
            .write(sidecar_line(endpoint, data).as_bytes())
            .map_err(|error| error.to_string())
    }
}

/// Answers to the sidecar's RPC calls travel on the same stdin as requests.
impl LineWriter for Sidecar {
    fn write_line(&self, line: &str) -> Result<(), String> {
        let mut child = self.child.lock().unwrap();
        let child = child.as_mut().ok_or("Backend sidecar is not running")?;
        child.write(line.as_bytes()).map_err(|error| error.to_string())
    }
}

impl Sidecar {
    pub fn spawn(
        self: &Arc<Self>,
        app: &AppHandle,
        router: Arc<Router>,
        replica: Arc<ConfigReplica>,
        sinks: Arc<Sinks>,
        rpc: Arc<Rpc>,
    ) -> Result<(), String> {
        let (mut events, child) = app
            .shell()
            .sidecar("VRCT-sidecar")
            .map(|command| {
                command
                    .env(BRIDGE_ENV.0, BRIDGE_ENV.1)
                    .env(SINKS_ENV_NAME, sinks_env_value())
                    .env(RPC_ENV_NAME, rpc_env_value())
            })
            .and_then(|command| command.spawn())
            .map_err(|error| error.to_string())?;
        *self.child.lock().unwrap() = Some(child);

        let sidecar = Arc::clone(self);
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            while let Some(event) = events.recv().await {
                match event {
                    CommandEvent::Stdout(bytes) => {
                        let line = String::from_utf8_lossy(&bytes);
                        let Some(response) = parse_sidecar_line(&line) else {
                            continue;
                        };
                        // Bridge traffic is for Rust only (config carries secrets,
                        // sink and RPC lines carry the user's chat and API keys).
                        // Sinks and RPC go first because the replica swallows
                        // every other /internal/ line.
                        if sinks.ingest(&response)
                            || rpc.ingest(&response)
                            || replica.ingest(&response)
                        {
                            continue;
                        }
                        let initialized = response.endpoint == "/run/initialization_complete";
                        router.accept_sidecar_response(response);
                        if initialized {
                            // The sidecar's own update push is dropped (it
                            // points at the upstream repo); ask Rust instead.
                            router.dispatch("/run/software_update_info".into(), None);
                        }
                    }
                    CommandEvent::Stderr(bytes) => {
                        let line = String::from_utf8_lossy(&bytes).trim_end().to_string();
                        if !line.is_empty() {
                            let _ = app.emit("backend-stderr", line);
                        }
                    }
                    CommandEvent::Error(message) => {
                        let _ = app.emit("backend-stderr", message);
                    }
                    CommandEvent::Terminated(payload) => {
                        sidecar.child.lock().unwrap().take();
                        let _ = app.emit("backend-terminated", payload.code);
                    }
                    _ => {}
                }
            }
        });
        Ok(())
    }
}
