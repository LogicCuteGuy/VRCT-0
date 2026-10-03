//! OSCQuery discovery and VRChat MuteSelf reception without a Python process.
//!
//! VRChat requires both mDNS services and an `/avatar` subtree to route its
//! outgoing OSC here: https://github.com/vrchat-community/osc/wiki/OSCQuery.
//! The callback runs on the service worker; enqueue lifecycle changes there.

use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::watch;

pub const MUTE_ADDRESS: &str = "/avatar/parameters/MuteSelf";
pub const QUERY_SERVICE: &str = "_oscjson._tcp.local.";
pub const OSC_SERVICE: &str = "_osc._udp.local.";
const MAX_JSON: usize = 1024 * 1024;
static INSTANCE_ID: AtomicU64 = AtomicU64::new(0);

pub type MuteCallback = Arc<dyn Fn(Option<bool>) + Send + Sync>;

#[derive(Clone, Copy, Debug)]
pub struct Endpoints {
    pub http: SocketAddr,
    pub osc: SocketAddr,
}

struct State {
    mute: Mutex<(Option<bool>, u64)>,
    callback: MuteCallback,
}

impl State {
    fn publish(&self, value: Option<bool>, revision: Option<u64>) {
        let mut mute = self.mute.lock().unwrap();
        if revision.is_some_and(|revision| revision != mute.1) {
            return;
        }
        // Even a repeated OSC packet supersedes an in-flight HTTP snapshot.
        mute.1 = mute.1.wrapping_add(1);
        if mute.0 == value {
            return;
        }
        mute.0 = value;
        drop(mute);
        (self.callback)(value);
    }
}

struct Running {
    cancel: watch::Sender<bool>,
    worker: JoinHandle<()>,
    endpoints: Endpoints,
}

struct Lifecycle {
    host: String,
    target_port: u16,
    active: bool,
    running: Option<Running>,
}

/// Local-only, as in the old backend. Remote OSC output continues through
/// OscSink but does not expose a receiver or query server on a remote host.
pub struct OscQueryService {
    state: Arc<State>,
    lifecycle: Mutex<Lifecycle>,
    stopped: AtomicBool,
    mdns_port: u16,
    peer_prefix: String,
}

impl OscQueryService {
    pub fn new(callback: MuteCallback) -> Self {
        Self::with_mdns_port(callback, 5353)
    }

    /// Alternate mDNS port for Unix integration tests; production uses 5353.
    /// mdns-sd's Windows per-interface socket path requires standard port 5353.
    pub fn with_mdns_port(callback: MuteCallback, mdns_port: u16) -> Self {
        Self::with_discovery_prefix(callback, mdns_port, "VRChat-Client")
    }

    /// Restrict discovery to one client name, including when integration tests
    /// share Windows port 5353 with a live VRChat client.
    pub fn with_discovery_prefix(callback: MuteCallback, mdns_port: u16, prefix: &str) -> Self {
        Self {
            state: Arc::new(State {
                mute: Mutex::new((None, 0)),
                callback,
            }),
            lifecycle: Mutex::new(Lifecycle {
                host: "127.0.0.1".into(),
                target_port: 9000,
                active: false,
                running: None,
            }),
            stopped: AtomicBool::new(false),
            mdns_port,
            peer_prefix: prefix.into(),
        }
    }

    pub fn configure(&self, host: &str, port: u16) -> Result<(), String> {
        if port == 0 || host.is_empty() {
            return Err("OSC host and nonzero target port are required".into());
        }
        let mut lifecycle = self.lifecycle.lock().unwrap();
        if lifecycle.host == host && lifecycle.target_port == port {
            return Ok(());
        }
        let was_running = lifecycle.active;
        Self::stop_locked(&mut lifecycle);
        lifecycle.host = host.into();
        lifecycle.target_port = port;
        self.state.publish(None, None);
        if was_running {
            self.start_locked(&mut lifecycle)?;
        }
        Ok(())
    }

    pub fn query_enabled(&self) -> bool {
        let lifecycle = self.lifecycle.lock().unwrap();
        is_local(&lifecycle.host)
            && lifecycle
                .running
                .as_ref()
                .is_some_and(|running| !running.worker.is_finished())
    }

    pub fn current_mute(&self) -> Option<bool> {
        self.state.mute.lock().unwrap().0
    }

    pub fn endpoints(&self) -> Option<Endpoints> {
        self.lifecycle
            .lock()
            .unwrap()
            .running
            .as_ref()
            .map(|running| running.endpoints)
    }

    pub fn start(&self) -> Result<(), String> {
        self.start_locked(&mut self.lifecycle.lock().unwrap())
    }

    fn start_locked(&self, lifecycle: &mut Lifecycle) -> Result<(), String> {
        if cfg!(windows) && self.mdns_port != 5353 {
            return Err("Windows OSCQuery requires mDNS port 5353".into());
        }
        if self.stopped.load(Ordering::Acquire) {
            return Err("OSCQuery service has shut down".into());
        }
        lifecycle.active = true;
        if lifecycle.running.is_some() || !is_local(&lifecycle.host) {
            return Ok(());
        }
        let osc = UdpSocket::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
        let http = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
        let endpoints = Endpoints {
            http: http.local_addr().map_err(|e| e.to_string())?,
            osc: osc.local_addr().map_err(|e| e.to_string())?,
        };
        osc.set_nonblocking(true).map_err(|e| e.to_string())?;
        http.set_nonblocking(true).map_err(|e| e.to_string())?;
        let (cancel, receiver) = watch::channel(false);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let state = self.state.clone();
        let mdns_port = self.mdns_port;
        let peer_prefix = self.peer_prefix.clone();
        let worker = thread::Builder::new()
            .name("vrct-oscquery".into())
            .spawn(move || {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(run(
                        osc,
                        http,
                        endpoints,
                        state,
                        receiver,
                        (mdns_port, peer_prefix),
                        ready_tx,
                    )),
                    Err(error) => {
                        let _ = ready_tx.send(Err(error.to_string()));
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => {
                lifecycle.running = Some(Running {
                    cancel,
                    worker,
                    endpoints,
                });
                Ok(())
            }
            result => {
                let _ = cancel.send(true);
                let _ = worker.join();
                Err(match result {
                    Ok(Err(error)) => error,
                    _ => "OSCQuery worker failed to initialize".into(),
                })
            }
        }
    }

    fn stop_locked(lifecycle: &mut Lifecycle) {
        if let Some(running) = lifecycle.running.take() {
            let _ = running.cancel.send(true);
            let _ = running.worker.join();
        }
    }

    /// Joins the worker, unregisters both advertisements and releases sockets.
    /// Do not call synchronously from the mute callback (enqueue it instead).
    pub fn stop(&self) {
        let mut lifecycle = self.lifecycle.lock().unwrap();
        lifecycle.active = false;
        Self::stop_locked(&mut lifecycle);
        drop(lifecycle);
        self.state.publish(None, None);
    }

    pub fn shutdown(&self) {
        self.stopped.store(true, Ordering::Release);
        self.stop();
    }
}

impl Drop for OscQueryService {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn is_local(host: &str) -> bool {
    host == "127.0.0.1" || host.eq_ignore_ascii_case("localhost")
}

async fn run(
    osc: UdpSocket,
    http: TcpListener,
    endpoints: Endpoints,
    state: Arc<State>,
    mut cancel: watch::Receiver<bool>,
    discovery: (u16, String),
    ready: mpsc::SyncSender<Result<(), String>>,
) {
    let (mdns_port, peer_prefix) = discovery;
    let daemon = match ServiceDaemon::new_with_port(mdns_port) {
        Ok(daemon) => daemon,
        Err(e) => {
            let _ = ready.send(Err(e.to_string()));
            return;
        }
    };
    let name = format!(
        "VRCT-{}-{}",
        std::process::id(),
        INSTANCE_ID.fetch_add(1, Ordering::Relaxed)
    );
    let hostname = format!("{}.local.", name.to_lowercase());
    let mut registered = Vec::new();
    let setup = (|| -> Result<_, String> {
        for (kind, port) in [
            (QUERY_SERVICE, endpoints.http.port()),
            (OSC_SERVICE, endpoints.osc.port()),
        ] {
            let service = ServiceInfo::new(
                kind,
                &name,
                &hostname,
                "127.0.0.1",
                port,
                None::<HashMap<String, String>>,
            )
            .map_err(|e| e.to_string())?;
            registered.push(service.get_fullname().to_string());
            daemon.register(service).map_err(|e| e.to_string())?;
        }
        let browser = daemon.browse(QUERY_SERVICE).map_err(|e| e.to_string())?;
        let osc = tokio::net::UdpSocket::from_std(osc).map_err(|e| e.to_string())?;
        let http = tokio::net::TcpListener::from_std(http).map_err(|e| e.to_string())?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|e| e.to_string())?;
        Ok((browser, osc, http, client))
    })();
    if let Ok((browser, osc, http, client)) = setup {
        let _ = ready.send(Ok(()));
        let mut peers: HashMap<String, SocketAddr> = HashMap::new();
        let mut requests = tokio::task::JoinSet::new();
        let mut retry = tokio::time::interval(Duration::from_secs(3));
        let mut packet = vec![0; 65536];
        loop {
            tokio::select! {
                biased;
                _ = cancel.changed() => break,
                result = osc.recv_from(&mut packet) => {
                    if let Ok((count, sender)) = result {
                        if sender.ip().is_loopback() {
                            if let Some(values) = decode_mute_packet(&packet[..count]) {
                                for value in values { state.publish(Some(value), None); }
                            }
                        }
                    }
                }
                result = http.accept(), if requests.len() < 16 => {
                    if let Ok((stream, _)) = result {
                        let state = state.clone();
                        let name = name.clone();
                        requests.spawn(async move { serve_http(stream, endpoints, &name, state).await; });
                    }
                }
                event = browser.recv_async() => {
                    match event {
                        Ok(ServiceEvent::ServiceResolved(info)) if info.get_fullname().starts_with(&peer_prefix) => {
                            // VRChat Windows serves HTTP on loopback. Never query arbitrary
                            // LAN addresses advertised by another application's mDNS record.
                            if info.get_addresses_v4().iter().any(|ip| ip.is_loopback()) {
                                peers.insert(info.get_fullname().into(), SocketAddr::from(([127,0,0,1], info.get_port())));
                                retry.reset_immediately();
                            }
                        }
                        Ok(ServiceEvent::ServiceRemoved(_, fullname)) => {
                            if peers.remove(&fullname).is_some() && peers.is_empty() {
                                state.publish(None, None);
                            }
                        }
                        Err(_) => break,
                        _ => {}
                    }
                }
                _ = retry.tick() => {
                    // Keep UDP and shutdown responsive while querying a stale peer.
                    for peer in peers.values().copied().take(8) {
                        if requests.len() >= 16 { break; }
                        let client = client.clone();
                        let state = state.clone();
                        let prefix = peer_prefix.clone();
                        requests.spawn(async move { refresh_mute(&client, peer, state, &prefix).await; });
                    }
                }
                _ = requests.join_next(), if !requests.is_empty() => {}
            }
        }
        requests.abort_all();
        while requests.join_next().await.is_some() {}
    } else if let Err(error) = setup {
        let _ = ready.send(Err(error));
    }
    let _ = daemon.stop_browse(QUERY_SERVICE);
    for fullname in registered {
        if let Ok(done) = daemon.unregister(&fullname) {
            let _ = done.recv_timeout(Duration::from_millis(500));
        }
    }
    if let Ok(done) = daemon.shutdown() {
        let _ = done.recv_timeout(Duration::from_secs(1));
    }
}

async fn get_json(client: &reqwest::Client, url: String) -> Option<Value> {
    let mut response = client.get(url).send().await.ok()?.error_for_status().ok()?;
    if response
        .content_length()
        .is_some_and(|size| size > MAX_JSON as u64)
    {
        return None;
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if bytes.len() + chunk.len() > MAX_JSON {
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).ok()
}

async fn refresh_mute(client: &reqwest::Client, peer: SocketAddr, state: Arc<State>, prefix: &str) {
    let revision = state.mute.lock().unwrap().1;
    let Some(host) = get_json(client, format!("http://{peer}/?HOST_INFO")).await else {
        state.publish(None, Some(revision));
        return;
    };
    if !host
        .get("NAME")
        .and_then(Value::as_str)
        .is_some_and(|name| name.starts_with(prefix))
    {
        return;
    }
    let value = get_json(client, format!("http://{peer}{MUTE_ADDRESS}"))
        .await
        .and_then(|node| node.get("VALUE")?.as_array()?.first()?.as_bool());
    state.publish(value, Some(revision));
}

fn node(path: &str, mute: Option<bool>) -> Option<Value> {
    let leaf = json!({"FULL_PATH":MUTE_ADDRESS,"DESCRIPTION":"VRChat microphone mute state","ACCESS":3,"TYPE":"T","VALUE":[mute]});
    let parameters = json!({"FULL_PATH":"/avatar/parameters","CONTENTS":{"MuteSelf":leaf}});
    let avatar = json!({"FULL_PATH":"/avatar","CONTENTS":{"parameters":parameters}});
    match path {
        "/" => Some(json!({"FULL_PATH":"/","CONTENTS":{"avatar":avatar}})),
        "/avatar" => Some(avatar),
        "/avatar/parameters" => Some(parameters),
        MUTE_ADDRESS => Some(leaf),
        _ => None,
    }
}

async fn serve_http(
    mut stream: tokio::net::TcpStream,
    endpoints: Endpoints,
    name: &str,
    state: Arc<State>,
) {
    let task = async {
        let mut request = Vec::new();
        let mut buffer = [0; 512];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let count = stream.read(&mut buffer).await.ok()?;
            if count == 0 || request.len() + count > 8192 {
                return None;
            }
            request.extend_from_slice(&buffer[..count]);
        }
        let line = std::str::from_utf8(&request).ok()?.lines().next()?;
        let mut parts = line.split_whitespace();
        let method = parts.next()?;
        let target = parts.next()?;
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let payload = if method != "GET" {
            None
        } else if path == "/" && query == "HOST_INFO" {
            Some(
                json!({"NAME":name,"OSC_IP":"127.0.0.1","OSC_PORT":endpoints.osc.port(),"OSC_TRANSPORT":"UDP","EXTENSIONS":{"ACCESS":true,"VALUE":true,"DESCRIPTION":true}}),
            )
        } else {
            node(path, state.mute.lock().unwrap().0)
        };
        let (status, body) = match payload {
            Some(value) => ("200 OK", value.to_string()),
            None => ("404 Not Found", "{}".into()),
        };
        let response = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        stream.write_all(response.as_bytes()).await.ok()?;
        stream.shutdown().await.ok()?;
        Some(())
    };
    let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
}

/// Strict OSC 1.0 boolean messages and nested bundles (bounded depth). A malformed
/// packet is discarded atomically, even if an earlier element contained MuteSelf.
pub fn decode_mute_packet(packet: &[u8]) -> Option<Vec<bool>> {
    fn string<'a>(packet: &'a [u8], position: &mut usize) -> Option<&'a str> {
        let start = *position;
        let end = start + packet.get(start..)?.iter().position(|byte| *byte == 0)?;
        let next = (end + 4) & !3;
        if !packet.get(end..next)?.iter().all(|byte| *byte == 0) {
            return None;
        }
        *position = next;
        std::str::from_utf8(packet.get(start..end)?).ok()
    }
    fn decode(packet: &[u8], depth: usize, values: &mut Vec<bool>) -> Option<()> {
        if depth > 16 || packet.is_empty() || !packet.len().is_multiple_of(4) {
            return None;
        }
        if packet.starts_with(b"#bundle\0") {
            let mut position = 16;
            packet.get(..position)?;
            while position < packet.len() {
                let size = u32::from_be_bytes(packet.get(position..position + 4)?.try_into().ok()?)
                    as usize;
                position += 4;
                let end = position.checked_add(size)?;
                decode(packet.get(position..end)?, depth + 1, values)?;
                position = end;
            }
            return Some(());
        }
        let mut position = 0;
        let address = string(packet, &mut position)?;
        if !address.starts_with('/') {
            return None;
        }
        let tags = string(packet, &mut position)?;
        let mut arrays = 0u8;
        for tag in tags.strip_prefix(',')?.chars() {
            match tag {
                'T' | 'F' | 'N' | 'I' => {}
                'i' | 'f' | 'c' | 'r' | 'm' => {
                    position = position.checked_add(4)?;
                }
                'h' | 'd' | 't' => {
                    position = position.checked_add(8)?;
                }
                's' | 'S' => {
                    string(packet, &mut position)?;
                }
                'b' => {
                    let size =
                        u32::from_be_bytes(packet.get(position..position + 4)?.try_into().ok()?)
                            as usize;
                    position = position.checked_add(4)?.checked_add(size)?.checked_add(3)? & !3;
                }
                '[' => {
                    arrays = arrays.checked_add(1)?;
                }
                ']' => {
                    arrays = arrays.checked_sub(1)?;
                }
                _ => return None,
            }
            if position > packet.len() {
                return None;
            }
        }
        if arrays != 0 {
            return None;
        }
        if position != packet.len() {
            return None;
        }
        if address != MUTE_ADDRESS {
            return Some(());
        }
        match tags {
            ",T" => values.push(true),
            ",F" => values.push(false),
            _ => return None,
        }
        Some(())
    }
    let mut values = Vec::new();
    decode(packet, 0, &mut values)?;
    Some(values)
}
